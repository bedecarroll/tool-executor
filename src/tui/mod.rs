#![allow(unexpected_cfgs)]

use std::cmp;
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
use shell_escape::unix::escape as shell_escape;
use std::borrow::Cow;
use tui_markdown::from_str as md_to_text;

use crate::app::{self, EmitMode, UiContext};
use crate::config::model::SearchMode;
#[cfg(all(test, unix))]
use crate::indexer::Indexer;
use crate::pipeline::{PipelinePlan, PipelineRequest, SessionContext, build_pipeline};
use crate::prompts::PromptStatus;
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
                kind: ProfileKind::Provider,
                preview_lines: vec![command],
            });
        }

        if let Some(prompt) = self.ctx.prompt.as_mut() {
            let prompt = &mut **prompt;
            match prompt.refresh(false) {
                PromptStatus::Ready { profiles, .. } => {
                    if let Some(provider) = self
                        .ctx
                        .config
                        .defaults
                        .provider
                        .clone()
                        .or_else(|| self.ctx.config.providers.keys().next().cloned())
                    {
                        for vp in profiles {
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
                                inline_pre: vec![format!(
                                    "pa {}",
                                    shell_escape(Cow::Borrowed(vp.name.as_str()))
                                )],
                                stdin_supported: vp.stdin_supported,
                                kind: ProfileKind::Virtual,
                                preview_lines: vp.contents.clone(),
                            });
                        }
                    } else {
                        self.message = Some(
                            "prompt profiles unavailable: no providers configured".to_string(),
                        );
                    }
                }
                PromptStatus::Unavailable { message } => {
                    self.message = Some(message);
                }
                PromptStatus::Disabled => {}
            }
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

        let mut entries: Vec<Entry> = sessions.into_iter().map(Entry::Session).collect();

        for profile in &self.profiles {
            if let Some(provider) = &self.provider_filter
                && &profile.provider != provider
            {
                continue;
            }
            if !self.filter.is_empty() && !profile.matches(&self.filter) {
                continue;
            }
            entries.push(Entry::Profile(profile.clone()));
        }

        entries.sort_by(|a, b| match (a, b) {
            (Entry::Profile(a), Entry::Profile(b)) => a.display.cmp(&b.display),
            (Entry::Profile(_), Entry::Session(_)) => cmp::Ordering::Less,
            (Entry::Session(_), Entry::Profile(_)) => cmp::Ordering::Greater,
            (Entry::Session(a), Entry::Session(b)) => b
                .last_active
                .unwrap_or_default()
                .cmp(&a.last_active.unwrap_or_default()),
            _ => cmp::Ordering::Equal,
        });

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
        let capture_prompt = !profile.pre.is_empty() || !profile.inline_pre.is_empty();
        let request = match &profile.kind {
            ProfileKind::Config { name } => PipelineRequest {
                config: self.ctx.config,
                provider_hint: Some(profile.provider.as_str()),
                profile: Some(name.as_str()),
                additional_pre: Vec::new(),
                additional_post: Vec::new(),
                inline_pre: Vec::new(),
                wrap: profile.wrap.as_deref(),
                provider_args: Vec::new(),
                capture_prompt,
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
                let lines = vec!["No transcript available".to_string()];
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
                let lines = vec![format!("preview error: {err}")];
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
mod tests {
    #![allow(
        unused_mut,
        clippy::implicit_clone,
        clippy::redundant_clone,
        clippy::map_unwrap_or
    )]
    use super::*;
    use assert_fs::TempDir;
    #[cfg(unix)]
    use assert_fs::fixture::PathChild;
    #[cfg(unix)]
    use assert_fs::prelude::*;
    use color_eyre::Result;
    use color_eyre::eyre::eyre;
    #[cfg(unix)]
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use indexmap::IndexMap;
    #[cfg(unix)]
    use ratatui::Terminal;
    #[cfg(unix)]
    use ratatui::backend::TestBackend;
    #[cfg(unix)]
    use ratatui::buffer::Buffer;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use std::sync::{LazyLock, Mutex, MutexGuard};
    #[cfg(unix)]
    use std::time::Duration as StdDuration;
    use time::{Duration, OffsetDateTime};

    use crate::config::AppDirectories;
    use crate::config::Config;
    use crate::config::model::{
        Defaults, FeatureConfig, ProfileConfig, ProviderConfig, SearchMode, SnippetConfig,
    };
    #[cfg(unix)]
    use crate::config::model::{PromptAssemblerConfig, Snippet, WrapperConfig, WrapperMode};
    use crate::db::Database;
    #[cfg(unix)]
    use crate::pipeline::Invocation;
    #[cfg(unix)]
    use crate::prompts::PromptAssembler;
    use crate::session::{MessageRecord, SessionIngest, SessionSummary, Transcript};

    #[cfg(unix)]
    static PATH_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[cfg(unix)]
    struct PathGuard {
        original: Option<String>,
        _lock: MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl PathGuard {
        fn push(temp: &TempDir) -> Self {
            let lock = PATH_LOCK.lock().unwrap();
            let original = env::var("PATH").ok();
            let mut paths = vec![temp.path().to_path_buf()];
            if let Some(value) = &original {
                paths.extend(env::split_paths(value).collect::<Vec<_>>());
            }
            let joined = env::join_paths(paths).expect("join paths");
            unsafe {
                env::set_var("PATH", joined);
            }
            Self {
                original,
                _lock: lock,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for PathGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                unsafe {
                    env::set_var("PATH", value);
                }
            } else {
                unsafe {
                    env::remove_var("PATH");
                }
            }
        }
    }

    fn provider_config(root: &Path) -> ProviderConfig {
        ProviderConfig {
            name: "codex".into(),
            bin: "echo".into(),
            flags: vec!["hello".into()],
            env: Vec::new(),
            session_roots: vec![root.join("sessions")],
            stdin: None,
        }
    }

    fn build_config(root: &Path) -> Config {
        let mut providers = IndexMap::new();
        providers.insert("codex".into(), provider_config(root));

        let snippets = SnippetConfig {
            pre: IndexMap::new(),
            post: IndexMap::new(),
        };

        let mut profiles = IndexMap::new();
        profiles.insert(
            "default".into(),
            ProfileConfig {
                name: "default".into(),
                provider: "codex".into(),
                description: Some("Default profile".into()),
                pre: Vec::new(),
                post: Vec::new(),
                wrap: None,
            },
        );

        Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: Some("default".into()),
                search_mode: SearchMode::FirstPrompt,
            },
            providers,
            snippets,
            wrappers: IndexMap::new(),
            profiles,
            features: FeatureConfig {
                prompt_assembler: None,
            },
        }
    }

    fn build_directories(temp: &TempDir) -> AppDirectories {
        AppDirectories {
            config_dir: temp.path().join("config"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
        }
    }

    fn insert_session(db: &mut Database, path: &Path, id: &str) -> Result<SessionSummary> {
        let summary = SessionSummary {
            id: id.into(),
            provider: "codex".into(),
            label: Some(format!("Session {id}")),
            path: path.to_path_buf(),
            uuid: Some(format!("uuid-{id}")),
            first_prompt: Some("Hello there".into()),
            actionable: true,
            created_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
            started_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
            last_active: Some(OffsetDateTime::now_utc().unix_timestamp()),
            size: 15,
            mtime: 20,
        };
        let mut message = MessageRecord::new(
            summary.id.clone(),
            0,
            "user",
            "Hello there",
            Some(format!("event_msg_{id}")),
            Some(summary.last_active.unwrap()),
        );
        message.is_first = true;
        let ingest = SessionIngest::new(summary.clone(), vec![message]);
        db.upsert_session(&ingest)?;
        Ok(summary)
    }

    fn insert_session_without_uuid(
        db: &mut Database,
        path: &Path,
        id: &str,
    ) -> Result<SessionSummary> {
        let summary = SessionSummary {
            id: id.into(),
            provider: "codex".into(),
            label: Some(format!("Session {id}")),
            path: path.to_path_buf(),
            uuid: None,
            first_prompt: Some("Hello there".into()),
            actionable: true,
            created_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
            started_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
            last_active: Some(OffsetDateTime::now_utc().unix_timestamp()),
            size: 15,
            mtime: 20,
        };
        let mut message = MessageRecord::new(
            summary.id.clone(),
            0,
            "user",
            "Hello there",
            Some(format!("event_msg_{id}")),
            Some(summary.last_active.unwrap()),
        );
        message.is_first = true;
        let ingest = SessionIngest::new(summary.clone(), vec![message]);
        db.upsert_session(&ingest)?;
        Ok(summary)
    }

    #[cfg(unix)]
    #[test]
    fn app_state_navigation_and_plan() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;

        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .clone();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("demo.jsonl");
        fs::File::create(&session_path)?.write_all(b"{}")?;
        insert_session(&mut db, &session_path, "sess-1")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        assert!(!state.entries.is_empty());

        // Navigate list and type filter characters.
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
        state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))?;
        assert_eq!(state.filter, "/d");
        assert!(state.message.is_none());
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        assert!(state.filter.is_empty());

        // Cycle provider filters.
        state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
        state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
        state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;

        // Build a plan for the selected session.
        let plan = state.build_plan()?.expect("plan generated");
        match plan.invocation {
            Invocation::Shell { ref command } => assert!(command.contains("echo")),
            Invocation::Exec { .. } => panic!("expected shell invocation"),
        }

        // Render the UI.
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| draw(frame, &mut state))?;
        Ok(())
    }

    #[test]
    fn session_entry_helpers_and_matching() {
        let entry = SessionEntry {
            id: "sess".into(),
            provider: "codex".into(),
            label: Some("Demo".into()),
            first_prompt: Some("Hello prompt".into()),
            actionable: true,
            last_active: Some(0),
            snippet: Some("Snippet line".into()),
            snippet_role: Some("user".into()),
        };
        assert!(entry.matches("demo"));
        assert!(entry.matches("codex"));
        assert!(entry.matches("hello"));
        assert!(entry.matches("snippet"));
        assert!(entry.matches("#sess"));
        assert_eq!(entry.display_label(), "Demo");
        assert_eq!(entry.snippet_line().as_deref(), Some("Hello prompt"));
        assert_eq!(entry.short_session_tag(), "#sess");
    }

    #[test]
    fn profile_entry_matching() {
        let entry = ProfileEntry {
            display: "Default".into(),
            provider: "codex".into(),
            pre: vec!["pre".into()],
            post: vec!["post".into()],
            wrap: None,
            description: Some("helpful".into()),
            tags: vec!["team".into()],
            inline_pre: Vec::new(),
            stdin_supported: true,
            kind: ProfileKind::Config {
                name: "default".into(),
            },
            preview_lines: Vec::new(),
        };
        assert!(entry.matches("help"));
        assert!(entry.matches("team"));
    }

    #[test]
    fn formatting_helpers_cover_paths() {
        assert_eq!(truncate_len("hello", 10), "hello");
        assert_eq!(truncate_len("longword", 4), "long");
        assert_eq!(normalize_whitespace(" test\nvalue "), "test value");
        assert_eq!(pad_relative_time("3m"), "3m      ");
        assert_eq!(
            format_rollout_label("rollout-2024-10-26T02-42-13-abcd"),
            Some("2024-10-26 02:42:13 • abcd".into())
        );
        assert_eq!(rollout_suffix("custom-xyz"), Some("custom-xyz".into()));
        assert_eq!(
            meaningful_excerpt("   \n# Title\nContent line."),
            Some("# Title Content line.".into())
        );

        let now = OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        let past = now - Duration::minutes(5);
        assert_eq!(
            format_relative_time(Some(past.unix_timestamp()), now),
            Some("5m ago".into())
        );
        assert_eq!(format_relative_time(None, now), None);
        assert_eq!(
            format_relative_time(Some((now + Duration::seconds(30)).unix_timestamp()), now),
            Some("in <1m".into())
        );
        assert_eq!(
            format_relative_time(Some((now - Duration::seconds(20)).unix_timestamp()), now),
            Some("just now".into())
        );
        assert_eq!(
            format_relative_time(Some((now + Duration::hours(2)).unix_timestamp()), now),
            Some("in 2h".into())
        );
        let days_ago = now - Duration::days(3);
        assert!(
            format_relative_time(Some(days_ago.unix_timestamp()), now)
                .unwrap()
                .contains(' ')
        );
        let last_year = now.replace_year(now.year() - 1).unwrap();
        assert!(
            format_relative_time(Some(last_year.unix_timestamp()), now)
                .unwrap()
                .contains('-')
        );
        assert!(format_dual_time(Some(now.unix_timestamp())).is_some());
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("truncate", 4), "tru…");
    }

    #[cfg(unix)]
    #[test]
    fn state_filtering_and_provider_cycle() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .clone();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("filter.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"filter\"}\n")?;
        insert_session(&mut db, &session_path, "sess-1")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        state.message = Some("filter mode".into());
        state.message = None;
        state.set_temporary_status_message("temp".into(), StdDuration::from_secs(0));
        state.expire_status_message();
        let _ = state.status_message();

        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
        state.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))?;
        assert_eq!(state.filter, "/h");
        assert!(state.message.is_none());

        state.filter = "Hello".into();
        state.full_text = true;
        state.refresh_entries()?;
        state.move_selection(5);
        state.move_selection(-5);
        state.provider_filter = Some("alt".into());
        state.refresh_entries()?;
        state.provider_filter = None;
        state.filter.clear();
        state.refresh_entries()?;

        state.full_text = false;
        state.filter = "Demo".into();
        state.refresh_entries()?;
        state.filter.clear();
        state.refresh_entries()?;

        state.cycle_provider_filter()?;
        state.cycle_provider_filter()?;

        state.list_state.select(Some(0));
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
        state.reindex()?;

        if let Some((idx, Entry::Profile(_))) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Profile(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
            let plan = state.build_plan()?.expect("profile plan");
            if let Invocation::Shell { command } = plan.invocation {
                assert!(command.contains("echo"));
            }
        }
        Ok(())
    }

    fn make_session_entry(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.into(),
            provider: "codex".into(),
            label: Some("Demo".into()),
            first_prompt: Some("Hello".into()),
            actionable: true,
            last_active: Some(123),
            snippet: None,
            snippet_role: None,
        }
    }

    fn make_transcript(id: &str) -> Transcript {
        let summary = SessionSummary {
            id: id.into(),
            provider: "codex".into(),
            label: Some("Demo".into()),
            path: PathBuf::from("/tmp/demo.jsonl"),
            uuid: Some("uuid-demo".into()),
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(1),
            started_at: Some(1),
            last_active: Some(2),
            size: 42,
            mtime: 2,
        };
        let message = MessageRecord::new(
            id,
            0,
            "user",
            "Hello world",
            Some("event_msg".into()),
            Some(2),
        );
        Transcript {
            session: summary,
            messages: vec![message],
        }
    }

    #[test]
    fn session_preview_from_result_success() {
        let session = make_session_entry("sess-success");
        let transcript = make_transcript("sess-success");
        let preview = AppState::session_preview_from_result(&session, Ok(Some(transcript)));
        assert!(preview.lines.join("\n").contains("Hello world"));
        assert!(preview.title.is_some());
        let styled = preview
            .styled
            .as_ref()
            .expect("expected styled markdown output");
        assert!(
            styled
                .lines
                .iter()
                .any(|line| line.style != Style::default()
                    || line.spans.iter().any(|span| span.style != Style::default()))
        );
    }

    #[test]
    fn session_preview_from_result_missing_transcript() {
        let session = make_session_entry("sess-missing");
        let preview = AppState::session_preview_from_result(&session, Ok(None));
        assert_eq!(preview.lines, vec![String::from("No transcript available")]);
        assert_eq!(preview.title.as_deref(), Some("sess-missing"));
    }

    #[test]
    fn session_preview_from_result_error() {
        let session = make_session_entry("sess-error");
        let preview = AppState::session_preview_from_result(&session, Err(eyre!("boom")));
        assert!(preview.lines[0].contains("boom"));
    }

    #[test]
    #[cfg(unix)]
    fn handle_key_navigation_and_provider_cycle() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;

        for idx in 0..12 {
            let filename = format!("session-{idx}.jsonl");
            let path = session_dir.join(&filename);
            fs::File::create(&path)?.write_all(b"{\"event\":\"demo\"}\n")?;
            insert_session(&mut db, &path, &format!("sess-{idx}"))?;
        }

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        assert!(state.entries.len() >= 12);

        state.filter = "hello".into();
        state.message = Some(MESSAGE_FILTER_MODE.to_string());
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        assert_eq!(state.filter, "hell");
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::SHIFT))?;
        assert_eq!(state.filter, "he");
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        assert!(state.filter.is_empty());
        state.message = None;

        state.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
        assert!(state.index >= 10);
        let after_page_down = state.index;
        state.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
        assert!(state.index <= after_page_down);
        state.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
        assert_eq!(state.index, 0);

        state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
        assert_eq!(state.provider_filter.as_deref(), Some("codex"));
        assert!(state.overlay_message.is_some());

        state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
        assert!(state.provider_filter.is_none());

        state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
        assert_eq!(state.provider_filter.as_deref(), Some("codex"));

        state.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))?;
        assert!(state.full_text);
        assert!(matches!(state.status_message(), Some(message) if message.contains("full-text")));

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn filter_typing_supports_slash_and_uppercase() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        assert!(state.filter.is_empty());
        assert!(state.message.is_none());

        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
        assert_eq!(state.filter, "/");
        assert!(state.message.is_none());

        state.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))?;
        assert_eq!(state.filter, "/R");
        assert!(state.message.is_none());

        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        assert_eq!(state.filter, "/");
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        assert!(state.filter.is_empty());
        assert!(state.message.is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn preview_cache_invalidation_and_profile_plans() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let pre_snippet_name = "prepare";
        config.snippets.pre.insert(
            pre_snippet_name.into(),
            Snippet {
                name: pre_snippet_name.into(),
                command: "echo pre".into(),
            },
        );
        config.wrappers.insert(
            "shellwrap".into(),
            WrapperConfig {
                name: "shellwrap".into(),
                mode: WrapperMode::Shell {
                    command: "echo '{{CMD}} --wrapped'".into(),
                },
            },
        );
        if let Some(default_profile) = config.profiles.get_mut("default") {
            default_profile.wrap = Some("shellwrap".into());
            default_profile.pre = vec![pre_snippet_name.into()];
        }

        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        state.entries = vec![Entry::Empty(EmptyEntry {
            title: "Nothing here".into(),
            preview: vec!["No entries found".into()],
            status: Some("refresh to load".into()),
        })];
        state.index = 0;
        state.list_state.select(Some(0));

        let first_preview = state.preview();
        assert_eq!(first_preview.lines, vec!["No entries found".to_string()]);

        if let Some(cached) = state.preview_cache.get_mut("empty:Nothing here") {
            cached.lines = vec!["stale".into()];
            cached.updated_at = cached
                .updated_at
                .checked_sub(StdDuration::from_secs(10))
                .expect("cached preview timestamp should allow subtraction");
        }

        let refreshed_preview = state.preview();
        assert_eq!(
            refreshed_preview.lines,
            vec!["No entries found".to_string()]
        );

        let profile_entry = state
            .profiles
            .iter()
            .find(|entry| entry.display == "default")
            .cloned()
            .expect("default profile");
        let wrapped_plan = state.plan_for_profile(&profile_entry)?.expect("plan");
        assert_eq!(wrapped_plan.wrapper.as_deref(), Some("shellwrap"));
        if let Invocation::Shell { command } = &wrapped_plan.invocation {
            assert!(command.contains("--wrapped"));
        } else {
            panic!("expected shell invocation");
        }

        let provider_entry = state
            .profiles
            .iter()
            .find(|entry| matches!(entry.kind, ProfileKind::Provider))
            .cloned()
            .expect("provider entry");
        let provider_plan = state.plan_for_profile(&provider_entry)?.expect("plan");
        assert!(provider_plan.wrapper.is_none());

        Ok(())
    }

    #[test]
    fn empty_entry_for_state_variants() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;

        {
            let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
            let mut ctx = UiContext {
                config: &config,
                directories: &directories,
                db: &mut db,
                prompt: None,
            };
            let entry = EmptyEntry::for_state(&ctx);
            assert_eq!(entry.title, "New session");
            assert!(
                entry
                    .status
                    .as_deref()
                    .is_some_and(|status| status.contains("No sessions yet"))
            );
        }

        let mut config_without = config.clone();
        config_without.providers.clear();
        {
            let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
            let mut ctx = UiContext {
                config: &config_without,
                directories: &directories,
                db: &mut db,
                prompt: None,
            };
            let entry = EmptyEntry::for_state(&ctx);
            assert_eq!(entry.title, "New session (configure tx)");
            assert!(
                entry
                    .preview
                    .iter()
                    .any(|line| line.contains("Add a TOML file"))
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn handle_key_normal_enter_executes_plan() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("enter.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"enter\"}\n")?;
        insert_session(&mut db, &session_path, "sess-enter")?;
        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        if let Some((idx, Entry::Session(_))) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Session(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
            assert!(matches!(state.outcome, Some(Outcome::Execute(_))));
        } else {
            panic!("expected session entry");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn handle_key_normal_ctrl_tab_emits_plan() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("ctrl-tab.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"ctrl_tab\"}\n")?;
        insert_session(&mut db, &session_path, "sess-ctrl-tab")?;
        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        if let Some((idx, Entry::Session(_))) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Session(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
            state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL))?;
            assert!(matches!(state.outcome, Some(Outcome::Emit(_))));
        } else {
            panic!("expected session entry");
        }
        Ok(())
    }

    #[test]
    fn build_plan_empty_entry_sets_message() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        state.entries = vec![Entry::Empty(EmptyEntry {
            title: "Nothing".into(),
            preview: vec![],
            status: Some("Select a session".into()),
        })];
        state.index = 0;
        state.list_state.select(Some(0));
        assert!(state.build_plan()?.is_none());
        assert_eq!(state.message.as_deref(), Some("Select a session"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn preview_renders_session_with_filter_and_cache() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("preview.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
        insert_session(&mut db, &session_path, "sess-preview")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        let first = state.preview();
        assert!(!first.lines.is_empty());
        assert!(first.title.is_some());

        if let Some(preview) = state.preview_cache.values_mut().next() {
            preview.updated_at = Instant::now();
        }
        let cached = state.preview();
        assert!(!cached.lines.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn preview_renders_markdown_without_filter() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("markdown.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
        insert_session(&mut db, &session_path, "sess-markdown")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        let preview = state.preview();
        let joined = preview.lines.join("\n");
        assert!(
            joined.contains("**Provider**") || joined.contains("# Codex Session"),
            "expected markdown markers in preview lines: {joined:?}"
        );
        let styled = preview
            .styled
            .as_ref()
            .expect("expected styled markdown output without external helpers");
        let first_line = styled
            .lines
            .first()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .unwrap_or_default();
        assert!(
            !first_line.contains("**"),
            "expected markdown renderer to strip formatting markers, saw: {first_line:?}"
        );
        Ok(())
    }

    #[test]
    fn dispatch_outcome_handles_emit_and_execute() -> Result<()> {
        let plan = PipelinePlan {
            pipeline: "echo hi".into(),
            display: "echo hi".into(),
            friendly_display: "echo hi".into(),
            env: Vec::new(),
            invocation: Invocation::Shell {
                command: "echo hi".into(),
            },
            provider: "codex".into(),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrapper: None,
            needs_stdin_prompt: false,
            stdin_prompt_label: None,
            cwd: std::env::current_dir()?,
        };

        dispatch_outcome(Some(Outcome::Emit(plan.clone())))?;

        let mut exec_plan = plan.clone();
        exec_plan.invocation = Invocation::Exec {
            argv: vec!["/bin/sh".into(), "-c".into(), "true".into()],
        };
        dispatch_outcome(Some(Outcome::Execute(exec_plan)))?;
        dispatch_outcome(Some(Outcome::PrintSessionId("session-123".into())))?;
        dispatch_outcome(Some(Outcome::ExportMarkdown(vec![
            "# heading".into(),
            "body".into(),
        ])))?;
        dispatch_outcome(None)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn reindex_updates_message_and_refreshes_entries() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;

        let sessions_dir = config
            .providers
            .get("codex")
            .expect("provider present")
            .session_roots
            .first()
            .expect("session root")
            .clone();
        fs::create_dir_all(&sessions_dir)?;
        let session_path = sessions_dir.join("reindex.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"index\"}\n")?;

        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        insert_session(&mut db, &session_path, "sess-reindex")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        state.reindex()?;
        assert!(
            state
                .message
                .as_ref()
                .is_some_and(|msg| msg.contains("Indexed")),
            "expected reindex to emit status message"
        );
        Ok(())
    }

    #[test]
    fn run_with_terminal_emits_plan() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
        let mut events = FakeEvents::new(vec![Event::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        ))]);
        let outcome = run_with_terminal(&mut ctx, &mut terminal, &mut events)?;
        match outcome {
            Some(Outcome::Emit(plan)) => assert!(!plan.display.is_empty()),
            other => panic!("expected emit outcome, got {other:?}"),
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn status_message_prefers_overlay_then_filter() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("overlay.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"overlay\"}\n")?;
        insert_session(&mut db, &session_path, "sess-overlay")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        state.set_temporary_status_message("overlay".into(), StdDuration::from_secs(60));
        assert_eq!(state.status_message().as_deref(), Some("overlay"));

        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            let area = frame.area();
            draw_status(frame, area, &state);
        })?;

        state.overlay_message = Some((
            "expired".into(),
            Instant::now()
                .checked_sub(StdDuration::from_secs(1))
                .expect("now should be later than the duration"),
        ));
        state.expire_status_message();
        state.filter = "filter text".into();
        assert_eq!(state.status_message().as_deref(), Some("filter text"));

        state.filter.clear();
        state.message = Some("custom status".into());
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            let area = frame.area();
            draw_status(frame, area, &state);
        })?;

        Ok(())
    }

    #[test]
    fn cycle_provider_filter_wraps_and_resets() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                name: "alpha".into(),
                bin: "echo".into(),
                flags: Vec::new(),
                env: Vec::new(),
                session_roots: vec![temp.child("alpha-sessions").path().to_path_buf()],
                stdin: None,
            },
        );

        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        assert_eq!(
            state.provider_order,
            vec!["alpha".to_string(), "codex".to_string()]
        );
        assert!(state.provider_filter.is_none());

        state.cycle_provider_filter()?;
        assert_eq!(state.provider_filter.as_deref(), Some("alpha"));
        assert_eq!(state.status_message().as_deref(), Some("provider: alpha"));
        assert_eq!(state.status_banner(), "provider: alpha");

        state.cycle_provider_filter()?;
        assert_eq!(state.provider_filter.as_deref(), Some("codex"));
        assert_eq!(state.status_message().as_deref(), Some("provider: codex"));

        state.cycle_provider_filter()?;
        assert!(state.provider_filter.is_none());
        assert_eq!(state.status_message().as_deref(), Some("provider: all"));

        state.provider_filter = Some("ghost".into());
        state.cycle_provider_filter()?;
        assert_eq!(state.provider_filter.as_deref(), Some("alpha"));

        state.provider_order.clear();
        state.cycle_provider_filter()?;
        assert!(state.provider_filter.is_none());
        assert_eq!(state.status_message().as_deref(), Some("provider: all"));
        Ok(())
    }

    #[test]
    fn status_message_handles_all_sources() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;

        state.set_temporary_status_message("overlay".into(), StdDuration::from_secs(60));
        assert_eq!(state.status_message().as_deref(), Some("overlay"));

        state.overlay_message = Some((
            "expired".into(),
            Instant::now()
                .checked_sub(StdDuration::from_secs(1))
                .expect("now should be later than the duration"),
        ));
        state.filter = "typing".into();
        assert_eq!(state.status_message().as_deref(), Some("typing"));
        assert_eq!(state.status_banner(), "typing");

        state.filter.clear();
        assert_eq!(state.status_message(), None);

        state.entries = vec![Entry::Empty(EmptyEntry {
            title: "empty".into(),
            preview: Vec::new(),
            status: Some("entry-status".into()),
        })];
        state.index = 0;
        assert_eq!(state.status_message().as_deref(), Some("entry-status"));

        state.entries = vec![Entry::Empty(EmptyEntry {
            title: "empty".into(),
            preview: Vec::new(),
            status: None,
        })];
        state.message = Some("manual message".into());
        assert_eq!(state.status_message().as_deref(), Some("manual message"));

        state.message = Some(MESSAGE_FILTER_MODE.to_string());
        state.filter = "residual".into();
        assert_eq!(state.status_message().as_deref(), Some("residual"));

        state.filter.clear();
        state.message = None;
        state.entries.clear();
        assert_eq!(state.status_message(), None);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn draw_preview_uses_cached_styled_content() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("preview.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
        insert_session(&mut db, &session_path, "sess-preview")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        let key = match &state.entries[state.index] {
            Entry::Session(session) => format!("session:{}", session.id),
            Entry::Profile(profile) => format!("profile:{}", profile.display),
            Entry::Empty(empty) => format!("empty:{}", empty.title),
        };
        state.preview_cache.insert(
            key,
            Preview {
                lines: vec!["cached".into()],
                styled: Some(Text::styled("cached", Style::default().fg(Color::Yellow))),
                title: Some("Cached Preview".into()),
                timestamp: Some("2025-10-26".into()),
                updated_at: Instant::now(),
            },
        );

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            let area = frame.area();
            draw_preview(frame, area, &mut state);
        })?;

        if let Some(preview) = state.preview_cache.values_mut().next() {
            preview.updated_at = Instant::now()
                .checked_sub(StdDuration::from_secs(10))
                .expect("now should be later than the duration");
        }
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            let area = frame.area();
            draw_preview(frame, area, &mut state);
        })?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_app_exits_on_escape_event() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut events = FakeEvents::new(vec![Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))]);
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend)?;
        let result = run_app(&mut ctx, &mut terminal, &mut events)?;
        assert!(result.is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_app_emits_plan_on_tab_event() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("planned.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
        insert_session(&mut db, &session_path, "sess-plan")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut events = FakeEvents::new(vec![Event::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        ))]);
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend)?;
        let result = run_app(&mut ctx, &mut terminal, &mut events)?;
        assert!(matches!(result, Some(Outcome::Emit(_))));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_app_prints_session_id_on_ctrl_y() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("ctrl-i.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
        insert_session(&mut db, &session_path, "sess-ctrl-i")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        if let Some((idx, _)) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Session(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
        }
        state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
        match state.outcome {
            Some(Outcome::PrintSessionId(session_id)) => {
                assert_eq!(session_id, "uuid-sess-ctrl-i");
            }
            other => panic!("expected PrintSessionId outcome, got {other:?}"),
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_app_prints_session_id_on_ctrl_y_without_uuid() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("ctrl-y.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
        insert_session_without_uuid(&mut db, &session_path, "sess-no-uuid")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        if let Some((idx, _)) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Session(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
        }
        state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
        match state.outcome {
            Some(Outcome::PrintSessionId(session_id)) => {
                assert_eq!(session_id, "sess-no-uuid");
            }
            other => panic!("expected PrintSessionId outcome, got {other:?}"),
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_app_prints_session_id_on_ctrl_y_session_missing() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("ctrl-y-missing.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
        insert_session(&mut db, &session_path, "sess-missing")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        if let Some((idx, _)) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Session(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
        }
        state.ctx.db.delete_session("sess-missing")?;
        state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
        assert!(state.outcome.is_none());
        assert_eq!(
            state.status_message().as_deref(),
            Some("Session not found; try refreshing.")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_app_exports_transcript_missing_session() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("ctrl-e-missing.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"export\"}\n")?;
        insert_session(&mut db, &session_path, "sess-export-missing")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        if let Some((idx, _)) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Session(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
        }
        state.ctx.db.delete_session("sess-export-missing")?;
        state.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))?;
        assert!(state.outcome.is_none());
        assert_eq!(
            state.status_message().as_deref(),
            Some("Transcript not available for export.")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_app_exports_transcript_on_ctrl_e() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("ctrl-e.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"export\"}\n")?;
        insert_session(&mut db, &session_path, "sess-ctrl-e")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        if let Some((idx, _)) = state
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| matches!(entry, Entry::Session(_)))
        {
            state.index = idx;
            state.list_state.select(Some(idx));
        }
        state.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))?;
        match state.outcome {
            Some(Outcome::ExportMarkdown(lines)) => {
                assert!(
                    lines.iter().any(|line| line.contains("Hello there")),
                    "export should include transcript content"
                );
                assert_eq!(
                    lines.first().map(String::as_str),
                    Some("# Codex Session uuid-sess-ctrl-e")
                );
            }
            other => panic!("expected ExportMarkdown outcome, got {other:?}"),
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn load_profiles_handles_prompt_statuses() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        config.profiles.insert(
            "ALPHA".into(),
            ProfileConfig {
                name: "ALPHA".into(),
                provider: "codex".into(),
                description: None,
                pre: Vec::new(),
                post: Vec::new(),
                wrap: None,
            },
        );
        config.profiles.insert(
            "alpha".into(),
            ProfileConfig {
                name: "alpha".into(),
                provider: "codex".into(),
                description: None,
                pre: Vec::new(),
                post: Vec::new(),
                wrap: None,
            },
        );

        let alpha_root = temp.path().join("alpha_sessions");
        fs::create_dir_all(&alpha_root)?;
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                name: "alpha".into(),
                bin: "echo".into(),
                flags: vec!["--alpha".into()],
                env: Vec::new(),
                session_roots: vec![alpha_root],
                stdin: None,
            },
        );

        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("profile.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"profile\"}\n")?;
        insert_session(&mut db, &session_path, "sess-profile")?;

        let script = temp.child("pa");
        script.write_str(
            "#!/bin/sh\necho '[{\"name\":\"virtual\",\"description\":\"Virtual entry\",\"stdin_supported\":true}]'\n",
        )?;
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(script.path(), perms.clone())?;
        }

        let _guard = PathGuard::push(&temp);
        let mut prompt = PromptAssembler::new(PromptAssemblerConfig {
            namespace: "tests".into(),
        });

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: Some(&mut prompt),
        };

        let mut state = AppState::new(&mut ctx)?;
        let alpha_count = state
            .profiles
            .iter()
            .filter(|entry| entry.display.eq_ignore_ascii_case("alpha"))
            .count();
        assert_eq!(alpha_count, 1);
        assert!(
            state
                .profiles
                .iter()
                .any(|entry| matches!(entry.kind, ProfileKind::Virtual))
        );

        script.write_str("#!/bin/sh\nexit 1\n")?;
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(script.path(), perms)?;
        }

        state.load_profiles();
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|msg| msg.contains("prompt assembler unavailable"))
        );

        Ok(())
    }

    #[test]
    fn preview_handles_missing_and_profile_entries() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("preview.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
        insert_session(&mut db, &session_path, "sess-preview")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        state.preview_cache.clear();
        state.entries = vec![Entry::Session(SessionEntry {
            id: "missing".into(),
            provider: "codex".into(),
            label: Some("missing".into()),
            first_prompt: None,
            actionable: true,
            last_active: Some(0),
            snippet: None,
            snippet_role: None,
        })];
        state.index = 0;
        let missing_preview = state.preview();
        assert_eq!(
            missing_preview.lines,
            vec![String::from("No transcript available")]
        );

        state.preview_cache.clear();
        if let Some(profile) = state
            .profiles
            .iter()
            .find(|entry| matches!(entry.kind, ProfileKind::Config { .. }))
            .cloned()
        {
            let mut profile_entry = profile;
            profile_entry.tags = vec!["alpha".into(), "beta".into()];
            state.entries = vec![Entry::Profile(profile_entry)];
            let profile_preview = state.preview();
            assert!(
                profile_preview
                    .lines
                    .iter()
                    .any(|line| line.contains("Tags: alpha, beta"))
            );
        }

        state.entries = vec![Entry::Empty(EmptyEntry {
            title: "Empty".into(),
            preview: vec![String::from("Nothing to show")],
            status: None,
        })];
        let empty_preview = state.preview();
        assert_eq!(empty_preview.lines, vec![String::from("Nothing to show")]);

        Ok(())
    }

    #[test]
    fn preview_prefers_virtual_profile_contents() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        state.preview_cache.clear();
        state.entries = vec![Entry::Profile(ProfileEntry {
            display: "pa/demo".into(),
            provider: "codex".into(),
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            description: Some("Original description".into()),
            tags: vec!["team".into()],
            inline_pre: vec!["pa demo".into()],
            stdin_supported: true,
            kind: ProfileKind::Virtual,
            preview_lines: vec!["Instruction 1".into(), "Instruction 2".into()],
        })];
        state.index = 0;
        state.list_state.select(Some(0));

        let preview = state.preview();
        assert_eq!(
            preview.lines,
            vec![
                "**Provider**: `codex`".to_string(),
                "**Description**: Original description".to_string(),
                "**Tags**: `team`".to_string(),
                String::new(),
                "```markdown".to_string(),
                "Instruction 1".to_string(),
                "Instruction 2".to_string(),
                "```".to_string()
            ]
        );

        Ok(())
    }

    #[test]
    fn provider_profile_preview_formats_command() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        let provider_entry = state
            .profiles
            .iter()
            .find(|entry| matches!(entry.kind, ProfileKind::Provider))
            .cloned()
            .expect("provider profile");

        let description = provider_entry
            .description
            .clone()
            .expect("provider description");
        let command_line = provider_entry
            .preview_lines
            .first()
            .cloned()
            .expect("preview line");

        state.entries = vec![Entry::Profile(provider_entry.clone())];
        state.index = 0;
        state.list_state.select(Some(0));
        state.preview_cache.clear();

        let preview = state.preview();
        assert_eq!(
            preview.lines,
            vec![
                format!("**Provider**: `{}`", provider_entry.provider),
                format!("**Description**: {description}"),
                "**Tags**: _None_".to_string(),
                String::new(),
                "```markdown".to_string(),
                command_line,
                "```".to_string()
            ]
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn plan_for_session_builds_resume_plan() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let session_dir = config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("resume.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"resume\"}\n")?;
        insert_session(&mut db, &session_path, "sess-resume")?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        let session_entry = state
            .entries
            .iter()
            .find_map(|entry| match entry {
                Entry::Session(session) => Some(session.clone()),
                _ => None,
            })
            .expect("session entry");
        let plan = state.plan_for_session(&session_entry)?;
        assert!(plan.is_some());
        Ok(())
    }

    #[test]
    fn plan_for_profile_marks_stdin_prompt() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        if let Some(profile) = state
            .profiles
            .iter()
            .find(|entry| matches!(entry.kind, ProfileKind::Config { .. }))
            .cloned()
        {
            let mut profile_entry = profile;
            profile_entry.stdin_supported = true;
            let plan = state.plan_for_profile(&profile_entry)?.expect("plan");
            assert!(plan.needs_stdin_prompt);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn typing_uppercase_r_in_normal_mode_updates_filter() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let mut state = AppState::new(&mut ctx)?;
        assert!(state.filter.is_empty());
        state.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))?;
        assert_eq!(state.filter, "R");
        assert!(state.message.is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn tui_draw_snapshots() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        state.entries = vec![
            Entry::Empty(EmptyEntry {
                title: "No Sessions".into(),
                preview: vec!["Use tx search to populate sessions.".into()],
                status: Some("Run tx index to refresh listings.".into()),
            }),
            Entry::Profile(ProfileEntry {
                display: "Wrapped Profile".into(),
                provider: "codex".into(),
                pre: vec!["echo prepare".into()],
                post: vec!["echo cleanup".into()],
                wrap: Some("shellwrap".into()),
                description: Some("Run codex with helpers".into()),
                tags: vec!["team".into(), "demo".into()],
                inline_pre: vec!["pa demo".into()],
                stdin_supported: true,
                kind: ProfileKind::Config {
                    name: "wrapped".into(),
                },
                preview_lines: Vec::new(),
            }),
        ];
        state.index = 0;
        state.list_state.select(Some(0));
        state.preview_cache.clear();

        let empty_snapshot = render_to_string(&mut state, 60, 16)?;
        insta::assert_snapshot!("tui_empty_entry_render", empty_snapshot);

        state.index = 1;
        state.list_state.select(Some(1));
        state.preview_cache.clear();
        let profile_snapshot = render_to_string(&mut state, 60, 16)?;
        insta::assert_snapshot!("tui_profile_entry_render", profile_snapshot);

        state.overlay_message = Some((
            "provider: codex".into(),
            Instant::now() + StdDuration::from_secs(60),
        ));
        let overlay_snapshot = render_to_string(&mut state, 60, 10)?;
        insta::assert_snapshot!("tui_status_overlay_render", overlay_snapshot);

        state.overlay_message = None;
        state.filter = "codex".into();
        state.message = Some("doctor: missing CODEX_TOKEN".into());
        let filter_snapshot = render_to_string(&mut state, 60, 10)?;
        insta::assert_snapshot!("tui_filter_status_render", filter_snapshot);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn tui_draw_search_results_snapshot() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let mut state = AppState::new(&mut ctx)?;
        state.entries.clear();
        state.preview_cache.clear();
        let snapshot = render_search_results_snapshot(&mut state)?;
        insta::assert_snapshot!("tui_search_results_render", snapshot);
        Ok(())
    }

    #[cfg(unix)]
    fn render_search_results_snapshot(state: &mut AppState<'_>) -> Result<String> {
        state.full_text = true;
        state.filter = "refactor".into();
        state.message = Some("search: 2 matches".into());
        state.entries = vec![
            Entry::Session(SessionEntry {
                id: "codex/refactor-feature".into(),
                provider: "codex".into(),
                label: Some("Refactor Feature".into()),
                first_prompt: Some("refactor feature layout".into()),
                actionable: true,
                last_active: Some(1_697_000_000),
                snippet: Some("Assistant suggested extracting helpers".into()),
                snippet_role: Some("assistant".into()),
            }),
            Entry::Session(SessionEntry {
                id: "codex/refactor-tests".into(),
                provider: "codex".into(),
                label: Some("Refactor Tests".into()),
                first_prompt: Some("improve test names".into()),
                actionable: true,
                last_active: Some(1_697_050_000),
                snippet: Some("User requested clearer snapshot titles".into()),
                snippet_role: Some("user".into()),
            }),
        ];
        state.index = 0;
        state.list_state.select(Some(0));
        state.preview_cache.clear();

        let preview_lines = vec![
            "Session: Refactor Feature".into(),
            String::new(),
            "Assistant suggested extracting helpers".into(),
        ];
        state.preview_cache.insert(
            "session:codex/refactor-feature".into(),
            Preview {
                lines: preview_lines.clone(),
                styled: markdown_lines_to_text(&preview_lines),
                title: Some("codex/refactor-feature".into()),
                timestamp: Some("2023-10-22 19:00 UTC".into()),
                updated_at: Instant::now(),
            },
        );

        render_to_string(state, 60, 16)
    }

    #[cfg(unix)]
    fn render_to_string(state: &mut AppState<'_>, width: u16, height: u16) -> Result<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| draw(frame, state))?;
        let buffer = terminal.backend_mut().buffer().clone();
        Ok(buffer_to_string(&buffer))
    }

    #[cfg(unix)]
    fn buffer_to_string(buffer: &Buffer) -> String {
        let area = buffer.area();
        let mut lines = Vec::new();
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                let symbol = buffer
                    .cell((x, y))
                    .map_or(" ", ratatui::buffer::Cell::symbol);
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            while line.ends_with(' ') {
                line.pop();
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    #[test]
    fn empty_entry_placeholder_when_no_providers() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = build_config(temp.path());
        config.providers.clear();
        config.profiles.clear();
        config.defaults.provider = None;
        config.defaults.profile = None;
        let directories = build_directories(&temp);
        directories.ensure_all()?;
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };

        let placeholder = EmptyEntry::for_state(&ctx);
        let state = AppState::new(&mut ctx)?;
        assert!(matches!(state.entries.first(), Some(Entry::Empty(_))));
        assert!(placeholder.status.is_some());
        Ok(())
    }
}
