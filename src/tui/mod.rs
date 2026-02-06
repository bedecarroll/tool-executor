#![allow(unexpected_cfgs)]

use std::collections::{HashMap, HashSet};
use std::env;
#[cfg(all(not(test), not(coverage)))]
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use color_eyre::Result;
#[cfg(all(not(test), not(coverage)))]
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
#[cfg(all(not(test), not(coverage)))]
use crossterm::event;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
#[cfg(all(not(test), not(coverage)))]
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
#[cfg(all(not(test), not(coverage)))]
use crossterm::{ExecutableCommand, execute};
#[cfg(all(not(test), not(coverage)))]
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::borrow::Cow;
use tui_markdown::from_str as md_to_text;

use crate::app::{self, EmitMode, UiContext};
use crate::config::model::SearchMode;
#[cfg(all(test, unix))]
use crate::indexer::Indexer;
use crate::pipeline::{
    PipelinePlan, PipelineRequest, PromptInvocation, SessionContext, build_pipeline,
};
use crate::prompts::{PromptStatus, VirtualProfile};
use crate::providers;
use crate::session::{SearchHit, SessionQuery, Transcript};
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};
use tracing::warn;
use unicode_width::UnicodeWidthStr;

const SESSION_LIMIT: usize = 200;
const PREVIEW_MESSAGE_LIMIT: usize = 8;
const MESSAGE_FILTER_MODE: &str = "Filtering results";
const DEFAULT_STATUS_HINT: &str = "↑/↓ scroll  •  Tab emit  •  Enter run  •  Ctrl-Y print ID  •  Ctrl-E export  •  Ctrl-P filter  •  Ctrl-F search  •  Esc quit";
const RELATIVE_TIME_WIDTH: usize = 8;
const PROFILE_IDENTIFIER_LIMIT: usize = 40;

/// Run the TUI event loop until the user selects an action or exits.
///
/// # Errors
///
/// Returns an error if terminal IO fails or database interactions within the UI
/// produce an error.
// The production TUI loop manipulates terminal state and is validated via the
// tmux smoke test; instrumenting it for coverage would require nested pseudo
// terminals, so we exclude it from coverage accounting.
#[cfg(all(not(test), not(coverage)))]
pub fn run<'a>(ctx: &'a mut UiContext<'a>) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut events = CrosstermEvents;
    let outcome = run_with_terminal(ctx, &mut terminal, &mut events);

    disable_raw_mode()?;
    terminal
        .backend_mut()
        .execute(crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let outcome = outcome?;
    dispatch_outcome(outcome)
}

#[cfg(any(test, coverage))]
pub fn run<'a>(ctx: &'a mut UiContext<'a>) -> Result<()> {
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend)?;
    let mut events = FakeEvents::new(vec![Event::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))]);
    let outcome = run_with_terminal(ctx, &mut terminal, &mut events)?;
    dispatch_outcome(outcome)
}

trait EventSource {
    fn poll(&mut self, timeout: Duration) -> Result<bool>;
    fn read(&mut self) -> Result<Event>;
}

#[cfg(all(not(test), not(coverage)))]
struct CrosstermEvents;

#[cfg(all(not(test), not(coverage)))]
impl EventSource for CrosstermEvents {
    fn poll(&mut self, timeout: Duration) -> Result<bool> {
        event::poll(timeout).wrap_err("failed to poll input events")
    }

    fn read(&mut self) -> Result<Event> {
        event::read().wrap_err("failed to read input event")
    }
}

#[cfg(any(test, coverage))]
struct FakeEvents {
    events: Vec<Event>,
}

#[cfg(any(test, coverage))]
impl FakeEvents {
    fn new(events: Vec<Event>) -> Self {
        Self { events }
    }
}

#[cfg(any(test, coverage))]
impl EventSource for FakeEvents {
    fn poll(&mut self, _timeout: Duration) -> Result<bool> {
        Ok(!self.events.is_empty())
    }

    fn read(&mut self) -> Result<Event> {
        if let Some(event) = self.events.first().cloned() {
            self.events.remove(0);
            Ok(event)
        } else {
            Err(eyre!("no more events"))
        }
    }
}

fn run_app<'a, B, E>(
    ctx: &'a mut UiContext<'a>,
    terminal: &mut Terminal<B>,
    events: &mut E,
) -> Result<Option<Outcome>>
where
    B: ratatui::backend::Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    E: EventSource,
{
    let mut state = AppState::new(ctx)?;
    loop {
        state.expire_status_message();
        terminal.draw(|frame| draw(frame, &mut state))?;
        if let Some(outcome) = state.outcome {
            return Ok(Some(outcome));
        }

        if events.poll(Duration::from_millis(200))?
            && let Event::Key(event) = events.read()?
            && state.handle_key(event)?
        {
            return Ok(state.outcome);
        }
    }
}

fn run_with_terminal<'a, B, E>(
    ctx: &'a mut UiContext<'a>,
    terminal: &mut Terminal<B>,
    events: &mut E,
) -> Result<Option<Outcome>>
where
    B: ratatui::backend::Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    E: EventSource,
{
    run_app(ctx, terminal, events)
}

fn dispatch_outcome(outcome: Option<Outcome>) -> Result<()> {
    match outcome {
        Some(Outcome::Emit(plan)) => app::emit_command(
            &plan,
            EmitMode::Plain {
                newline: true,
                friendly: true,
            },
        ),
        Some(Outcome::Execute(plan)) => app::execute_plan(&plan),
        Some(Outcome::PrintSessionId(session_id)) => {
            println!("{session_id}");
            Ok(())
        }
        Some(Outcome::ExportMarkdown(lines)) => {
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        None => Ok(()),
    }
}

#[derive(Debug, Clone)]
enum Outcome {
    Emit(PipelinePlan),
    Execute(PipelinePlan),
    PrintSessionId(String),
    ExportMarkdown(Vec<String>),
}

struct AppState<'ctx> {
    ctx: &'ctx mut UiContext<'ctx>,
    profiles: Vec<ProfileEntry>,
    entries: Vec<Entry>,
    index: usize,
    filter: String,
    provider_filter: Option<String>,
    provider_order: Vec<String>,
    full_text: bool,
    message: Option<String>,
    overlay_message: Option<(String, Instant)>,
    preview_cache: HashMap<String, Preview>,
    outcome: Option<Outcome>,
    list_state: ratatui::widgets::ListState,
}

#[derive(Debug, Clone)]
enum Entry {
    Session(SessionEntry),
    Profile(ProfileEntry),
    Empty(EmptyEntry),
}

#[derive(Debug, Clone)]
struct SessionEntry {
    id: String,
    provider: String,
    wrapper: Option<String>,
    label: Option<String>,
    first_prompt: Option<String>,
    actionable: bool,
    last_active: Option<i64>,
    snippet: Option<String>,
    snippet_role: Option<String>,
}

static TIMESTAMP_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute] UTC");
static LOCAL_TIMESTAMP_FORMAT: &[FormatItem<'static>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute] [offset_hour sign:mandatory]:[offset_minute]"
);
static MONTH_DAY_FORMAT: &[FormatItem<'static>] =
    format_description!("[month repr:short] [day padding:none]");
static FULL_DATE_FORMAT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

#[derive(Debug, Clone)]
struct ProfileEntry {
    display: String,
    provider: String,
    pre: Vec<String>,
    post: Vec<String>,
    wrap: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    inline_pre: Vec<String>,
    stdin_supported: bool,
    prompt_assembler: Option<String>,
    prompt_assembler_args: Vec<String>,
    prompt_available: bool,
    kind: ProfileKind,
    preview_lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct EmptyEntry {
    title: String,
    preview: Vec<String>,
    status: Option<String>,
}

#[derive(Debug, Clone)]
enum ProfileKind {
    Config { name: String },
    Virtual,
    Provider,
}

#[derive(Debug, Clone)]
struct Preview {
    lines: Vec<String>,
    styled: Option<Text<'static>>,
    title: Option<String>,
    timestamp: Option<String>,
    updated_at: Instant,
}

impl SessionEntry {
    fn from_query(query: SessionQuery) -> Self {
        Self {
            id: query.id,
            provider: query.provider,
            wrapper: query.wrapper,
            label: query.label,
            first_prompt: query.first_prompt,
            actionable: query.actionable,
            last_active: query.last_active,
            snippet: None,
            snippet_role: None,
        }
    }

    fn from_hit(hit: SearchHit) -> Self {
        Self {
            id: hit.session_id,
            provider: hit.provider,
            wrapper: hit.wrapper,
            label: hit.label,
            first_prompt: hit.snippet.clone(),
            actionable: hit.actionable,
            last_active: hit.last_active,
            snippet: hit.snippet,
            snippet_role: hit.role,
        }
    }

    fn matches(&self, needle: &str) -> bool {
        let needle = needle.to_ascii_lowercase();
        if self.id.to_ascii_lowercase().contains(&needle) {
            return true;
        }
        if self.provider.to_ascii_lowercase().contains(&needle) {
            return true;
        }
        if self
            .wrapper
            .as_ref()
            .is_some_and(|wrapper| wrapper.to_ascii_lowercase().contains(&needle))
        {
            return true;
        }
        if self
            .label
            .as_ref()
            .is_some_and(|label| label.to_ascii_lowercase().contains(&needle))
        {
            return true;
        }
        if self
            .first_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.to_ascii_lowercase().contains(&needle))
        {
            return true;
        }
        if self
            .snippet
            .as_ref()
            .is_some_and(|snippet| snippet.to_ascii_lowercase().contains(&needle))
        {
            return true;
        }
        if self.display_label().to_ascii_lowercase().contains(&needle) {
            return true;
        }
        self.short_session_tag()
            .to_ascii_lowercase()
            .contains(&needle)
    }

    fn display_label(&self) -> String {
        if let Some(label) = self.label.as_deref() {
            return format_rollout_label(label).unwrap_or_else(|| label.to_string());
        }
        if let Some(last) = self.id.rsplit('/').next() {
            return format_rollout_label(last).unwrap_or_else(|| last.to_string());
        }
        self.id.clone()
    }

    fn snippet_line(&self) -> Option<String> {
        if let Some(prompt) = self.first_prompt.as_deref().and_then(meaningful_excerpt) {
            return Some(prompt);
        }

        if let Some(snippet) = self.snippet.as_deref().and_then(meaningful_excerpt) {
            if let Some(role) = self.snippet_role.as_deref() {
                return Some(format!("[{}] {snippet}", role.to_ascii_lowercase()));
            }
            return Some(snippet);
        }

        None
    }

    fn short_session_tag(&self) -> String {
        if let Some(last) = self.id.rsplit('/').next() {
            if let Some(tag) = rollout_suffix(last) {
                return format!("#{}", truncate_len(&tag, 12));
            }
            return format!("#{}", truncate_len(last, 12));
        }
        format!("#{}", truncate_len(&self.id, 12))
    }
}

fn truncate_len(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        input.to_string()
    } else {
        input.chars().take(max).collect()
    }
}

fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pad_relative_time(value: &str) -> String {
    let width = UnicodeWidthStr::width(value);
    if width >= RELATIVE_TIME_WIDTH {
        value.to_string()
    } else {
        let mut out = String::with_capacity(value.len() + (RELATIVE_TIME_WIDTH - width));
        out.push_str(value);
        out.push_str(&" ".repeat(RELATIVE_TIME_WIDTH - width));
        out
    }
}

fn format_rollout_label(label: &str) -> Option<String> {
    let rest = label.strip_prefix("rollout-")?;
    let (date, time_uuid) = rest.split_once('T')?;
    let mut segments = time_uuid.splitn(4, '-');
    let hour = segments.next()?;
    let minute = segments.next()?;
    let second = segments.next()?;
    let remainder = segments.next().unwrap_or_default();
    let suffix = rollout_suffix(remainder).unwrap_or_else(|| remainder.to_string());
    Some(format!("{date} {hour}:{minute}:{second} • {suffix}"))
}

fn rollout_suffix(value: &str) -> Option<String> {
    let trimmed = value.trim_matches('-');
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .chars()
            .take(12)
            .collect::<String>()
            .to_ascii_lowercase(),
    )
}

fn meaningful_excerpt(text: &str) -> Option<String> {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('<') {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(trimmed);
        if out.chars().count() >= 240 {
            break;
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn format_relative_time(ts: Option<i64>, now: OffsetDateTime) -> Option<String> {
    let ts = ts?;
    let dt = OffsetDateTime::from_unix_timestamp(ts).ok()?;
    let seconds_diff = now.unix_timestamp() - dt.unix_timestamp();
    let future = seconds_diff < 0;
    let seconds = seconds_diff.unsigned_abs();

    if seconds < 60 {
        return Some(if future {
            "in <1m".to_string()
        } else {
            "just now".to_string()
        });
    }

    let minutes = seconds / 60;
    if minutes < 60 {
        let value = minutes;
        return Some(if future {
            format!("in {value}m")
        } else {
            format!("{value}m ago")
        });
    }

    let hours = minutes / 60;
    if hours < 24 {
        let value = hours;
        return Some(if future {
            format!("in {value}h")
        } else {
            format!("{value}h ago")
        });
    }

    let formatted = if now.date().year() == dt.date().year() {
        dt.format(MONTH_DAY_FORMAT).ok()?
    } else {
        dt.format(FULL_DATE_FORMAT).ok()?
    };

    Some(if future {
        format!("on {formatted}")
    } else {
        formatted
    })
}

fn format_dual_time(ts: Option<i64>) -> Option<String> {
    let ts = ts?;
    let dt = OffsetDateTime::from_unix_timestamp(ts).ok()?;
    let utc = dt.format(TIMESTAMP_FORMAT).ok()?;
    let local = UtcOffset::current_local_offset()
        .ok()
        .and_then(|offset| {
            let local_dt = dt.to_offset(offset);
            local_dt
                .format(LOCAL_TIMESTAMP_FORMAT)
                .ok()
                .map(|value| format!("{value} local"))
        })
        .unwrap_or_else(|| "local time unavailable".to_string());
    Some(format!("Last active: {utc} | {local}"))
}

impl ProfileEntry {
    fn matches(&self, needle: &str) -> bool {
        let haystack = format!(
            "{} {} {} {}",
            self.display,
            self.description.as_deref().unwrap_or(""),
            self.tags.join(" "),
            self.provider
        )
        .to_ascii_lowercase();
        haystack.contains(&needle.to_ascii_lowercase())
    }
}

impl EmptyEntry {
    fn for_state(ctx: &UiContext<'_>) -> Self {
        if ctx.config.providers.is_empty() {
            let conf_d = ctx.directories.config_dir.join("conf.d");
            let preview = vec![
                "tx needs at least one provider before sessions can appear.".to_string(),
                format!(
                    "Add a TOML file under {} with a [providers.<name>] entry.",
                    conf_d.display()
                ),
                "See README.md (Quick Start) for a ready-to-copy example.".to_string(),
            ];
            let status = Some(format!(
                "No providers configured yet. Add files under {} to enable new sessions.",
                conf_d.display()
            ));
            Self {
                title: "New session (configure tx)".to_string(),
                preview,
                status,
            }
        } else {
            let providers = ctx
                .config
                .providers
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let preview = vec![
                "No sessions indexed yet.".to_string(),
                format!("Available providers: {providers}"),
                "Select one in the TUI and press Enter to begin.".to_string(),
            ];
            let status =
                Some("No sessions yet — start a provider from the TUI to get started.".to_string());
            Self {
                title: "New session".to_string(),
                preview,
                status,
            }
        }
    }
}

impl<'ctx> AppState<'ctx> {
    fn new(ctx: &'ctx mut UiContext<'ctx>) -> Result<Self> {
        let defaults = &ctx.config.defaults;
        let mut provider_order: Vec<String> = ctx.config.providers.keys().cloned().collect();
        provider_order.sort();

        let mut state = Self {
            ctx,
            profiles: Vec::new(),
            entries: Vec::new(),
            index: 0,
            filter: String::new(),
            provider_filter: None,
            provider_order,
            full_text: matches!(defaults.search_mode, SearchMode::FullText),
            message: None,
            overlay_message: None,
            preview_cache: HashMap::new(),
            outcome: None,
            list_state: ratatui::widgets::ListState::default(),
        };

        state.load_profiles();
        state.refresh_entries()?;
        state.list_state.select(Some(0));
        Ok(state)
    }

    fn set_temporary_status_message(&mut self, message: String, duration: Duration) {
        self.overlay_message = Some((message, Instant::now() + duration));
    }

    fn expire_status_message(&mut self) {
        if let Some((_, until)) = &self.overlay_message
            && Instant::now() >= *until
        {
            self.overlay_message = None;
        }
    }

    fn cycle_provider_filter(&mut self) -> Result<()> {
        if self.provider_order.is_empty() {
            self.provider_filter = None;
        } else if let Some(current) = &self.provider_filter {
            if let Some(index) = self
                .provider_order
                .iter()
                .position(|provider| provider == current)
            {
                if index + 1 < self.provider_order.len() {
                    self.provider_filter = Some(self.provider_order[index + 1].clone());
                } else {
                    self.provider_filter = None;
                }
            } else {
                self.provider_filter = Some(self.provider_order[0].clone());
            }
        } else {
            self.provider_filter = Some(self.provider_order[0].clone());
        }

        self.refresh_entries()?;
        let message = match &self.provider_filter {
            Some(provider) => format!("provider: {provider}"),
            None => "provider: all".to_string(),
        };
        self.set_temporary_status_message(message, Duration::from_secs(3));
        Ok(())
    }

    fn status_message(&self) -> Option<String> {
        if let Some((text, until)) = &self.overlay_message
            && Instant::now() < *until
        {
            return Some(text.clone());
        }

        if let Some(message) = self.entries.get(self.index).and_then(|entry| match entry {
            Entry::Empty(empty) => empty.status.clone(),
            _ => None,
        }) && !message.is_empty()
        {
            return Some(message);
        }

        if let Some(message) = self.message.as_deref()
            && !message.is_empty()
            && message != MESSAGE_FILTER_MODE
        {
            return Some(message.to_string());
        }

        if !self.filter.is_empty() {
            return Some(self.filter.clone());
        }

        None
    }

    fn status_banner(&self) -> String {
        self.status_message()
            .unwrap_or_else(|| DEFAULT_STATUS_HINT.to_string())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        self.handle_key_normal(key)
    }

    #[allow(clippy::too_many_lines)]
    fn load_profiles(&mut self) {
        self.profiles.clear();
        let mut seen = HashSet::new();

        let mut prompt_profiles: Vec<VirtualProfile> = Vec::new();
        let mut available_prompts: HashSet<String> = HashSet::new();
        let mut prompt_warning: Option<String> = None;

        if let Some(prompt) = self.ctx.prompt.as_mut() {
            match prompt.refresh(false) {
                PromptStatus::Ready { profiles, .. } => {
                    available_prompts.extend(profiles.iter().map(|vp| vp.name.clone()));
                    prompt_profiles = profiles;
                }
                PromptStatus::Unavailable { message } => {
                    prompt_warning = Some(message);
                }
                PromptStatus::Disabled => {}
            }
        }

        let default_provider = self
            .ctx
            .config
            .defaults
            .provider
            .clone()
            .or_else(|| self.ctx.config.providers.keys().next().cloned());

        for (name, profile) in &self.ctx.config.profiles {
            let profile_name = name.clone();
            let key = profile_name.to_ascii_lowercase();
            if !seen.insert(key) {
                warn!(
                    entry = %profile_name,
                    "duplicate profile name '{profile_name}' detected; keeping first definition"
                );
                continue;
            }
            let mut prompt_available = true;
            if let Some(prompt_name) = &profile.prompt_assembler {
                prompt_available = available_prompts.contains(prompt_name);
                if !prompt_available && self.message.is_none() {
                    let message = prompt_warning.clone().unwrap_or_else(|| {
                        if self.ctx.prompt.is_some() {
                            format!(
                                "prompt assembler prompt '{prompt_name}' referenced by profile '{profile_name}' not found"
                            )
                        } else {
                            format!(
                                "profile '{profile_name}' references prompt '{prompt_name}' but prompt-assembler is disabled or unavailable"
                            )
                        }
                    });
                    self.message = Some(message);
                }
            }
            self.profiles.push(ProfileEntry {
                display: profile_name.clone(),
                provider: profile.provider.clone(),
                pre: profile.pre.clone(),
                post: profile.post.clone(),
                wrap: profile.wrap.clone(),
                description: profile.description.clone().or_else(|| {
                    Some(format!(
                        "Press Enter to start {} with profile {}.",
                        profile.provider, profile_name
                    ))
                }),
                tags: Vec::new(),
                inline_pre: Vec::new(),
                stdin_supported: false,
                prompt_assembler: profile.prompt_assembler.clone(),
                prompt_assembler_args: profile.prompt_assembler_args.clone(),
                prompt_available,
                kind: ProfileKind::Config { name: profile_name },
                preview_lines: Vec::new(),
            });
        }

        for (name, provider) in &self.ctx.config.providers {
            let provider_name = name.clone();
            let key = provider_name.to_ascii_lowercase();
            if !seen.insert(key) {
                warn!(
                    entry = %provider_name,
                    "provider entry '{provider_name}' conflicts with an existing profile; skipping"
                );
                continue;
            }
            let command = if provider.flags.is_empty() {
                provider.bin.clone()
            } else {
                format!("{} {}", provider.bin, provider.flags.join(" "))
            };
            self.profiles.push(ProfileEntry {
                display: provider_name.clone(),
                provider: provider_name,
                pre: Vec::new(),
                post: Vec::new(),
                wrap: None,
                description: Some(command.clone()),
                tags: Vec::new(),
                inline_pre: Vec::new(),
                stdin_supported: false,
                prompt_assembler: None,
                prompt_assembler_args: Vec::new(),
                prompt_available: true,
                kind: ProfileKind::Provider,
                preview_lines: vec![command],
            });
        }

        if !prompt_profiles.is_empty() {
            if let Some(provider) = default_provider {
                for vp in prompt_profiles {
                    let profile_name = vp.key.clone();
                    let key = profile_name.to_ascii_lowercase();
                    if !seen.insert(key) {
                        warn!(
                            entry = %profile_name,
                            "virtual profile '{profile_name}' conflicts with an existing entry; skipping"
                        );
                        continue;
                    }
                    self.profiles.push(ProfileEntry {
                        display: profile_name.clone(),
                        provider: provider.clone(),
                        pre: Vec::new(),
                        post: Vec::new(),
                        wrap: None,
                        description: vp.description.clone(),
                        tags: vp.tags.clone(),
                        inline_pre: Vec::new(),
                        stdin_supported: vp.stdin_supported,
                        prompt_assembler: Some(vp.name.clone()),
                        prompt_assembler_args: Vec::new(),
                        prompt_available: true,
                        kind: ProfileKind::Virtual,
                        preview_lines: vp.contents.clone(),
                    });
                }
            } else if self.message.is_none() {
                self.message =
                    Some("prompt profiles unavailable: no providers configured".to_string());
            }
        }

        if let Some(message) = prompt_warning
            && self.message.is_none()
        {
            self.message = Some(message);
        }
    }

    fn refresh_entries(&mut self) -> Result<()> {
        let searching = !self.filter.is_empty();
        let mut sessions = if searching {
            if self.full_text {
                self.search_full_text_sessions(&self.filter)?
            } else {
                self.search_prompt_sessions(&self.filter)?
            }
        } else {
            self.load_sessions()?
        };

        if !searching {
            sessions.retain(|session| session.actionable);
        }

        if searching && !self.full_text {
            let query = self.filter.to_ascii_lowercase();
            sessions.retain(|session| session.matches(&query));
        }

        if let Some(provider) = &self.provider_filter {
            sessions.retain(|session| session.provider == *provider);
        }

        let mut profile_entries = Vec::new();
        for profile in &self.profiles {
            if let Some(provider) = &self.provider_filter
                && &profile.provider != provider
            {
                continue;
            }
            if !self.filter.is_empty() && !profile.matches(&self.filter) {
                continue;
            }
            profile_entries.push(Entry::Profile(profile.clone()));
        }

        sessions.sort_by(|a, b| {
            b.last_active
                .unwrap_or_default()
                .cmp(&a.last_active.unwrap_or_default())
        });

        let mut entries = profile_entries;
        entries.extend(sessions.into_iter().map(Entry::Session));

        let is_filtered = !self.filter.is_empty() || self.provider_filter.is_some();

        if entries.is_empty() {
            self.preview_cache.clear();
            if is_filtered {
                self.entries.clear();
                self.index = 0;
                self.list_state.select(None);
            } else {
                let placeholder = EmptyEntry::for_state(self.ctx);
                self.entries = vec![Entry::Empty(placeholder)];
                self.index = 0;
                self.list_state.select(Some(0));
            }
            return Ok(());
        }

        self.entries = entries;
        self.preview_cache.clear();

        if self.index >= self.entries.len() {
            self.index = self.entries.len().saturating_sub(1);
        }
        self.list_state.select(Some(self.index));
        Ok(())
    }

    fn load_sessions(&self) -> Result<Vec<SessionEntry>> {
        let queries = self.ctx.db.list_sessions(
            self.provider_filter.as_deref(),
            true,
            None,
            Some(SESSION_LIMIT),
        )?;
        Ok(queries.into_iter().map(SessionEntry::from_query).collect())
    }

    fn search_full_text_sessions(&self, term: &str) -> Result<Vec<SessionEntry>> {
        let hits = self
            .ctx
            .db
            .search_full_text(term, self.provider_filter.as_deref(), false)?;

        let mut seen = HashSet::new();
        let mut sessions = Vec::new();

        for hit in hits {
            if !seen.insert(hit.session_id.clone()) {
                continue;
            }

            let mut entry = if let Some(summary) = self.ctx.db.session_summary(&hit.session_id)? {
                let query = SessionQuery {
                    id: summary.id.clone(),
                    provider: summary.provider.clone(),
                    wrapper: summary.wrapper.clone(),
                    label: summary.label.clone(),
                    first_prompt: summary.first_prompt.clone(),
                    actionable: summary.actionable,
                    last_active: summary.last_active,
                };
                SessionEntry::from_query(query)
            } else {
                SessionEntry::from_hit(hit.clone())
            };

            if entry.first_prompt.is_none() {
                entry.snippet.clone_from(&hit.snippet);
                entry.snippet_role.clone_from(&hit.role);
            }

            sessions.push(entry);
        }

        Ok(sessions)
    }

    fn search_prompt_sessions(&self, term: &str) -> Result<Vec<SessionEntry>> {
        let hits = self
            .ctx
            .db
            .search_first_prompt(term, self.provider_filter.as_deref(), false)?;
        Ok(hits.into_iter().map(SessionEntry::from_hit).collect())
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> Result<bool> {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Ok(true),
            (KeyCode::Char('y' | 'Y'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                self.trigger_print_session_id()?;
                Ok(false)
            }
            (KeyCode::Char('e' | 'E'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                self.trigger_export_markdown()?;
                Ok(false)
            }
            (KeyCode::Down, _) => {
                self.move_selection(1);
                Ok(false)
            }
            (KeyCode::Up, _) => {
                self.move_selection(-1);
                Ok(false)
            }
            (KeyCode::PageDown, _) => {
                self.move_selection(10);
                Ok(false)
            }
            (KeyCode::PageUp, _) => {
                self.move_selection(-10);
                Ok(false)
            }
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.cycle_provider_filter()?;
                Ok(false)
            }
            (KeyCode::Backspace, mods) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                if !self.filter.is_empty() {
                    self.filter.pop();
                    self.refresh_entries()?;
                }
                Ok(false)
            }
            (KeyCode::Char(ch), mods) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                self.filter.push(ch);
                self.refresh_entries()?;
                Ok(false)
            }
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.full_text = !self.full_text;
                self.refresh_entries()?;
                let mode_label = if self.full_text {
                    "search: full-text"
                } else {
                    "search: prompt"
                };
                self.set_temporary_status_message(mode_label.to_string(), Duration::from_secs(3));
                Ok(false)
            }
            (KeyCode::Tab, _) => {
                if let Some(plan) = self.build_plan()? {
                    self.outcome = Some(Outcome::Emit(plan));
                }
                Ok(false)
            }
            (KeyCode::Enter, _) => {
                if let Some(plan) = self.build_plan()? {
                    self.outcome = Some(Outcome::Execute(plan));
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.index = 0;
            self.list_state.select(None);
            return;
        }
        let len = self.entries.len();
        let max_index = len.saturating_sub(1);
        let next = self.index.saturating_add_signed(delta).min(max_index);
        self.index = next;
        self.list_state.select(Some(self.index));
    }

    fn selected_session(&self) -> Option<&SessionEntry> {
        self.entries.get(self.index).and_then(|entry| match entry {
            Entry::Session(session) => Some(session),
            _ => None,
        })
    }

    fn trigger_print_session_id(&mut self) -> Result<()> {
        if let Some(session) = self.selected_session() {
            match self.ctx.db.session_summary(&session.id)? {
                Some(summary) => {
                    let identifier = summary
                        .uuid
                        .as_deref()
                        .unwrap_or(summary.id.as_str())
                        .to_string();
                    self.outcome = Some(Outcome::PrintSessionId(identifier));
                }
                None => {
                    self.set_temporary_status_message(
                        "Session not found; try refreshing.".into(),
                        Duration::from_secs(3),
                    );
                }
            }
        } else {
            self.set_temporary_status_message(
                "Select a session to print its ID.".into(),
                Duration::from_secs(3),
            );
        }
        Ok(())
    }

    fn trigger_export_markdown(&mut self) -> Result<()> {
        if let Some(session) = self.selected_session() {
            match self.ctx.db.fetch_transcript(&session.id)? {
                Some(transcript) => {
                    let lines = transcript.markdown_lines(None);
                    self.outcome = Some(Outcome::ExportMarkdown(lines));
                }
                None => {
                    self.set_temporary_status_message(
                        "Transcript not available for export.".into(),
                        Duration::from_secs(3),
                    );
                }
            }
        } else {
            self.set_temporary_status_message(
                "Select a session to export its transcript.".into(),
                Duration::from_secs(3),
            );
        }
        Ok(())
    }

    #[cfg(all(test, unix))]
    fn reindex(&mut self) -> Result<()> {
        let mut indexer = Indexer::new(self.ctx.db, self.ctx.config);
        let report = indexer.run()?;
        self.message = Some(format!(
            "Indexed: {} updated, {} removed ({} errors)",
            report.updated,
            report.removed,
            report.errors.len()
        ));
        self.refresh_entries()?;
        Ok(())
    }

    fn build_plan(&mut self) -> Result<Option<PipelinePlan>> {
        let entry = match self.entries.get(self.index) {
            Some(entry) => entry.clone(),
            None => return Ok(None),
        };

        match entry {
            Entry::Session(session) => self.plan_for_session(&session),
            Entry::Profile(profile) => self.plan_for_profile(&profile),
            Entry::Empty(empty) => {
                if let Some(message) = &empty.status {
                    self.message = Some(message.clone());
                }
                Ok(None)
            }
        }
    }

    fn plan_for_session(&mut self, session: &SessionEntry) -> Result<Option<PipelinePlan>> {
        let summary = self
            .ctx
            .db
            .session_summary(&session.id)?
            .ok_or_else(|| eyre!("session '{}' not found", session.id))?;

        let resume_plan = providers::resume_info(&summary)?;
        let mut provider_args = Vec::new();
        let mut resume_token = None;
        if let Some(mut plan) = resume_plan {
            resume_token = plan.resume_token.take();
            provider_args.extend(plan.args);
        }

        let request = PipelineRequest {
            config: self.ctx.config,
            provider_hint: Some(summary.provider.as_str()),
            profile: None,
            additional_pre: Vec::new(),
            additional_post: Vec::new(),
            inline_pre: Vec::new(),
            wrap: None,
            provider_args,
            capture_prompt: false,
            prompt_assembler: None,
            vars: HashMap::new(),
            session: SessionContext {
                id: Some(summary.id.clone()),
                label: summary.label.clone(),
                path: Some(summary.path.to_string_lossy().to_string()),
                resume_token,
            },
            cwd: summary.path.parent().map_or_else(
                || env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                Path::to_path_buf,
            ),
        };

        let plan = build_pipeline(&request)?;
        Ok(Some(plan))
    }

    fn plan_for_profile(&mut self, profile: &ProfileEntry) -> Result<Option<PipelinePlan>> {
        if let Some(prompt_name) = profile.prompt_assembler.as_deref()
            && !profile.prompt_available
        {
            return Err(eyre!(
                "profile '{}' references unknown prompt '{}'",
                profile.display,
                prompt_name
            ));
        }
        let prompt_invocation = profile
            .prompt_assembler
            .as_ref()
            .map(|name| PromptInvocation {
                name: name.clone(),
                args: profile.prompt_assembler_args.clone(),
            });
        let capture_prompt = prompt_invocation.is_some()
            || !profile.pre.is_empty()
            || !profile.inline_pre.is_empty();
        let request = match &profile.kind {
            ProfileKind::Config { name } => PipelineRequest {
                config: self.ctx.config,
                provider_hint: Some(profile.provider.as_str()),
                profile: Some(name.as_str()),
                additional_pre: Vec::new(),
                additional_post: Vec::new(),
                inline_pre: profile.inline_pre.clone(),
                wrap: profile.wrap.as_deref(),
                provider_args: Vec::new(),
                capture_prompt,
                prompt_assembler: prompt_invocation.clone(),
                vars: HashMap::new(),
                session: SessionContext::default(),
                cwd: env::current_dir()?,
            },
            ProfileKind::Virtual => PipelineRequest {
                config: self.ctx.config,
                provider_hint: Some(profile.provider.as_str()),
                profile: None,
                additional_pre: profile.pre.clone(),
                additional_post: profile.post.clone(),
                inline_pre: profile.inline_pre.clone(),
                wrap: profile.wrap.as_deref(),
                provider_args: Vec::new(),
                capture_prompt,
                prompt_assembler: prompt_invocation.clone(),
                vars: HashMap::new(),
                session: SessionContext::default(),
                cwd: env::current_dir()?,
            },
            ProfileKind::Provider => PipelineRequest {
                config: self.ctx.config,
                provider_hint: Some(profile.provider.as_str()),
                profile: None,
                additional_pre: Vec::new(),
                additional_post: Vec::new(),
                inline_pre: Vec::new(),
                wrap: None,
                provider_args: Vec::new(),
                capture_prompt,
                prompt_assembler: None,
                vars: HashMap::new(),
                session: SessionContext::default(),
                cwd: env::current_dir()?,
            },
        };

        let mut plan = build_pipeline(&request)?;
        if profile.stdin_supported {
            plan.needs_stdin_prompt = true;
            plan.stdin_prompt_label = Some(profile.display.clone());
        }
        Ok(Some(plan))
    }

    #[allow(clippy::too_many_lines)]
    fn preview(&mut self) -> Preview {
        let entry = if let Some(entry) = self.entries.get(self.index) {
            entry.clone()
        } else {
            let lines = vec!["No selection".to_string()];
            let styled = markdown_lines_to_text(&lines);
            return Preview {
                lines,
                styled,
                title: Some("Preview".to_string()),
                timestamp: None,
                updated_at: Instant::now(),
            };
        };

        let key = match &entry {
            Entry::Session(session) => format!("session:{}", session.id),
            Entry::Profile(profile) => format!("profile:{}", profile.display),
            Entry::Empty(empty) => format!("empty:{}", empty.title),
        };

        if let Some(preview) = self.preview_cache.get(&key)
            && preview.updated_at.elapsed() < Duration::from_secs(5)
        {
            return preview.clone();
        }

        let preview = match entry {
            Entry::Session(session) => Self::session_preview_from_result(
                &session,
                self.ctx.db.fetch_transcript(&session.id),
            ),
            Entry::Profile(profile) => {
                let mut lines = match profile.kind {
                    ProfileKind::Virtual | ProfileKind::Provider => {
                        build_markdown_profile_preview(&profile)
                    }
                    ProfileKind::Config { .. } => {
                        let mut lines = Vec::new();
                        lines.push(format!("Provider: {}", profile.provider));
                        if let Some(description) = &profile.description {
                            lines.push(String::new());
                            lines.push(description.clone());
                        }
                        if !profile.tags.is_empty()
                            && !matches!(profile.kind, ProfileKind::Provider)
                        {
                            lines.push(String::new());
                            lines.push(format!("Tags: {}", profile.tags.join(", ")));
                        }
                        lines
                    }
                };
                if lines.is_empty() {
                    lines.push(format!("Provider: {}", profile.provider));
                }
                let styled = markdown_lines_to_text(&lines);
                Preview {
                    lines,
                    styled,
                    title: Some(profile.display.clone()),
                    timestamp: None,
                    updated_at: Instant::now(),
                }
            }
            Entry::Empty(empty) => {
                let lines = empty.preview.clone();
                let styled = markdown_lines_to_text(&lines);
                Preview {
                    lines,
                    styled,
                    title: Some(empty.title.clone()),
                    timestamp: None,
                    updated_at: Instant::now(),
                }
            }
        };

        self.preview_cache.insert(key, preview.clone());
        preview
    }

    fn session_preview_from_result(
        session: &SessionEntry,
        result: Result<Option<Transcript>>,
    ) -> Preview {
        match result {
            Ok(Some(transcript)) => {
                let lines = transcript.markdown_lines(Some(PREVIEW_MESSAGE_LIMIT));
                let styled = markdown_lines_to_text(&lines);
                let title = transcript
                    .session
                    .uuid
                    .clone()
                    .unwrap_or_else(|| session.id.clone());

                Preview {
                    lines,
                    styled,
                    title: Some(title),
                    timestamp: format_dual_time(session.last_active),
                    updated_at: Instant::now(),
                }
            }
            Ok(None) => {
                let mut lines = Vec::new();
                if let Some(wrapper) = session.wrapper.as_deref() {
                    lines.push(format!("**Wrapper**: `{wrapper}`"));
                    lines.push(String::new());
                }
                lines.push("No transcript available".to_string());
                let styled = markdown_lines_to_text(&lines);
                Preview {
                    lines,
                    styled,
                    title: Some(session.id.clone()),
                    timestamp: format_dual_time(session.last_active),
                    updated_at: Instant::now(),
                }
            }
            Err(err) => {
                let mut lines = vec![format!("preview error: {err}")];
                if let Some(wrapper) = session.wrapper.as_deref() {
                    lines.insert(0, String::new());
                    lines.insert(0, format!("**Wrapper**: `{wrapper}`"));
                }
                let styled = markdown_lines_to_text(&lines);
                Preview {
                    lines,
                    styled,
                    title: Some(session.id.clone()),
                    timestamp: format_dual_time(session.last_active),
                    updated_at: Instant::now(),
                }
            }
        }
    }
}

fn markdown_lines_to_text(lines: &[String]) -> Option<Text<'static>> {
    if lines.is_empty() {
        return None;
    }
    let markdown = lines.join("\n");
    let text = md_to_text(&markdown);
    Some(text_to_owned(text))
}

fn text_to_owned(text: Text<'_>) -> Text<'static> {
    let lines = text
        .lines
        .into_iter()
        .map(|line| {
            let spans = line
                .spans
                .into_iter()
                .map(|span| Span {
                    style: span.style,
                    content: Cow::Owned(span.content.into_owned()),
                })
                .collect();
            Line {
                style: line.style,
                alignment: line.alignment,
                spans,
            }
        })
        .collect();
    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

fn draw(frame: &mut Frame<'_>, state: &mut AppState<'_>) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(frame.area());
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(vertical[0]);

    draw_entries(frame, columns[0], state);
    draw_preview(frame, columns[1], state);
    draw_status(frame, vertical[1], state);
}

fn draw_entries(frame: &mut Frame<'_>, area: Rect, state: &mut AppState<'_>) {
    let now = OffsetDateTime::now_utc();
    let identifier_width = state
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Profile(profile) => {
                let truncated = truncate(&profile.display, PROFILE_IDENTIFIER_LIMIT);
                Some(truncated.chars().count())
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let items = state
        .entries
        .iter()
        .map(|entry| match entry {
            Entry::Session(session) => {
                let raw_relative = format_relative_time(session.last_active, now)
                    .unwrap_or_else(|| "n/a".to_string());
                let relative = pad_relative_time(&raw_relative);
                let label = session
                    .snippet_line()
                    .unwrap_or_else(|| session.display_label());
                let label = normalize_whitespace(&label);
                let label = truncate(&label, 200);
                let spans = vec![
                    Span::styled(relative, Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
                ];
                ListItem::new(Line::from(spans))
            }
            Entry::Profile(profile) => {
                let identifier = {
                    let truncated = truncate(&profile.display, PROFILE_IDENTIFIER_LIMIT);
                    let truncated_len = truncated.chars().count();
                    if identifier_width == 0 || truncated_len >= identifier_width {
                        truncated
                    } else {
                        format!("{truncated:<identifier_width$}")
                    }
                };
                let mut spans = vec![Span::styled(
                    identifier,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )];
                if let Some(description) = profile.description.as_deref()
                    && !description.is_empty()
                {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        truncate(description, 80),
                        Style::default().fg(Color::White),
                    ));
                }
                ListItem::new(Line::from(spans))
            }
            Entry::Empty(empty) => {
                let spans = vec![Span::styled(
                    truncate(&empty.title, 40),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )];
                ListItem::new(Line::from(spans))
            }
        })
        .collect::<Vec<_>>();

    let block = Block::default()
        .title("Sessions & Profiles")
        .borders(Borders::ALL);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut state.list_state);
}

fn draw_preview(frame: &mut Frame<'_>, area: Rect, state: &mut AppState<'_>) {
    let preview = state.preview();
    let Preview {
        lines,
        styled,
        timestamp,
        title,
        ..
    } = preview;
    let title = timestamp.or(title).unwrap_or_else(|| state.status_banner());

    let mut paragraph = if let Some(styled) = styled {
        Paragraph::new(styled)
    } else {
        let plain_lines = lines
            .iter()
            .map(|line| Line::from(line.as_str()))
            .collect::<Vec<_>>();
        Paragraph::new(plain_lines)
    };

    let block = Block::default().title(title).borders(Borders::ALL);
    paragraph = paragraph.block(block).wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, state: &AppState<'_>) {
    let status = state.status_message();
    let (content, style) = match status {
        Some(text) => (format!(" {text} "), Style::default()),
        None => (
            format!(" {DEFAULT_STATUS_HINT} "),
            Style::default().fg(Color::DarkGray),
        ),
    };

    let paragraph = Paragraph::new(content)
        .style(style)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(paragraph, area);
}

fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        input.to_string()
    } else {
        let mut out = input.chars().take(max - 1).collect::<String>();
        out.push('…');
        out
    }
}

fn build_markdown_profile_preview(profile: &ProfileEntry) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("**Provider**: `{}`", profile.provider));

    if let Some(description) = profile
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let mut desc_lines = description.lines();
        if let Some(first) = desc_lines.next() {
            lines.push(format!("**Description**: {first}"));
        }
        for line in desc_lines {
            lines.push(line.to_string());
        }
    } else {
        lines.push("**Description**: _None_".to_string());
    }

    if profile.tags.is_empty() {
        lines.push("**Tags**: _None_".to_string());
    } else {
        let rendered = profile
            .tags
            .iter()
            .map(|tag| format!("`{tag}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("**Tags**: {rendered}"));
    }

    lines.push(String::new());
    lines.push("```markdown".to_string());
    if profile.preview_lines.is_empty() {
        lines.push("_No prompt output_".to_string());
    } else {
        lines.extend(profile.preview_lines.clone());
    }
    lines.push("```".to_string());
    lines
}

#[cfg(all(test, unix))]
mod tests;
