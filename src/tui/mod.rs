use std::cmp;
use std::collections::{HashMap, HashSet};
use std::env;
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{ExecutableCommand, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use shell_escape::unix::escape as shell_escape;
use std::borrow::Cow;

use crate::app::{self, EmitMode, UiContext};
use crate::config::model::SearchMode;
use crate::indexer::Indexer;
use crate::pipeline::{PipelinePlan, PipelineRequest, SessionContext, build_pipeline};
use crate::prompts::PromptStatus;
use crate::providers;
use crate::session::{SearchHit, SessionQuery};
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};
use tracing::warn;
use unicode_width::UnicodeWidthStr;

const SESSION_LIMIT: usize = 200;
const PREVIEW_MESSAGE_LIMIT: usize = 8;
const MESSAGE_FILTER_MODE: &str = "Filter mode: type to narrow results, Enter to apply, Esc to stop editing (Esc again from the list to quit).";
const DEFAULT_STATUS_HINT: &str = "↑/↓ scroll  •  Tab emit  •  Enter run  •  Ctrl-P cycle provider filter  •  Ctrl-F toggle search mode  •  Esc quit";
const RELATIVE_TIME_WIDTH: usize = 8;
const PROFILE_IDENTIFIER_LIMIT: usize = 40;

/// Run the TUI event loop until the user selects an action or exits.
///
/// # Errors
///
/// Returns an error if terminal IO fails or database interactions within the UI
/// produce an error.
pub fn run<'a>(ctx: &'a mut UiContext<'a>) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_app(ctx, &mut terminal);

    disable_raw_mode()?;
    terminal
        .backend_mut()
        .execute(crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    match result? {
        Some(Outcome::Emit(plan)) => app::emit_command(&plan, EmitMode::Plain { newline: true }),
        Some(Outcome::Execute(plan)) => app::execute_plan(&plan),
        None => Ok(()),
    }
}

fn run_app<'a>(
    ctx: &'a mut UiContext<'a>,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<Option<Outcome>> {
    let mut state = AppState::new(ctx)?;
    loop {
        state.expire_status_message();
        terminal.draw(|frame| draw(frame, &mut state))?;
        if let Some(outcome) = state.outcome {
            return Ok(Some(outcome));
        }

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(event) = event::read()?
            && state.handle_key(event)?
        {
            return Ok(state.outcome);
        }
    }
}

#[derive(Debug, Clone)]
enum Outcome {
    Emit(PipelinePlan),
    Execute(PipelinePlan),
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
    input_mode: InputMode,
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
    kind: ProfileKind,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Filter,
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
                format!("Run `tx launch <provider>` using one of: {providers}"),
                "The list will populate once session logs are ingested.".to_string(),
            ];
            let status = Some("No sessions yet — launch a provider to get started.".to_string());
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
            input_mode: InputMode::Normal,
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

    fn set_status_message(&mut self, message: &'static str) {
        self.message = Some(message.to_string());
    }

    fn clear_status_message(&mut self, message: &'static str) {
        if self.message.as_deref() == Some(message) {
            self.message = None;
        }
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

        if matches!(self.input_mode, InputMode::Filter) {
            return if self.filter.is_empty() {
                Some(String::new())
            } else {
                Some(self.filter.clone())
            };
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
        match self.input_mode {
            InputMode::Normal => self.handle_key_normal(key),
            InputMode::Filter => self.handle_key_filter(key),
        }
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
                        "Launch via 'tx launch {} --profile {}'",
                        profile.provider, profile_name
                    ))
                }),
                tags: Vec::new(),
                inline_pre: Vec::new(),
                kind: ProfileKind::Config { name: profile_name },
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
            let mut description = format!("Launch via 'tx launch {name}' using {}", provider.bin);
            if provider.flags.is_empty() {
                description.push('.');
            } else {
                description.push_str(" with flags: ");
                description.push_str(&provider.flags.join(" "));
                description.push('.');
            }
            self.profiles.push(ProfileEntry {
                display: provider_name.clone(),
                provider: provider_name,
                pre: Vec::new(),
                post: Vec::new(),
                wrap: None,
                description: Some(description),
                tags: Vec::new(),
                inline_pre: Vec::new(),
                kind: ProfileKind::Provider,
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
                                kind: ProfileKind::Virtual,
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
            (KeyCode::Char('/'), _) => {
                self.input_mode = InputMode::Filter;
                self.set_status_message(MESSAGE_FILTER_MODE);
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
                    if self.filter.is_empty() {
                        self.clear_status_message(MESSAGE_FILTER_MODE);
                    }
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
            (KeyCode::Char('R'), _) => {
                self.reindex()?;
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

    fn handle_key_filter(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                self.clear_status_message(MESSAGE_FILTER_MODE);
                Ok(false)
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.refresh_entries()?;
                if self.filter.is_empty() {
                    self.clear_status_message(MESSAGE_FILTER_MODE);
                }
                Ok(false)
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    if self.filter.is_empty() {
                        self.set_status_message(MESSAGE_FILTER_MODE);
                    }
                    self.filter.push(ch);
                    self.refresh_entries()?;
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
            Entry::Session(session) => {
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
                Ok(Some(build_pipeline(&request)?))
            }
            Entry::Profile(profile) => {
                let request = match profile.kind {
                    ProfileKind::Config { ref name } => PipelineRequest {
                        config: self.ctx.config,
                        provider_hint: Some(profile.provider.as_str()),
                        profile: Some(name.as_str()),
                        additional_pre: Vec::new(),
                        additional_post: Vec::new(),
                        inline_pre: Vec::new(),
                        wrap: profile.wrap.as_deref(),
                        provider_args: Vec::new(),
                        capture_prompt: true,
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
                        capture_prompt: true,
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
                        capture_prompt: false,
                        vars: HashMap::new(),
                        session: SessionContext::default(),
                        cwd: env::current_dir()?,
                    },
                };
                Ok(Some(build_pipeline(&request)?))
            }
            Entry::Empty(empty) => {
                if let Some(message) = &empty.status {
                    self.message = Some(message.clone());
                }
                Ok(None)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn preview(&mut self) -> Preview {
        let entry = if let Some(entry) = self.entries.get(self.index) {
            entry.clone()
        } else {
            let lines = vec!["No selection".to_string()];
            let styled = lines_to_plain_text(&lines);
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
            Entry::Session(session) => match self.ctx.db.fetch_transcript(&session.id) {
                Ok(Some(transcript)) => {
                    let mut lines = transcript.markdown_lines(Some(PREVIEW_MESSAGE_LIMIT));
                    let styled = if let Some(filtered) = apply_preview_filter(
                        self.ctx.config.defaults.preview_filter.as_ref(),
                        &lines,
                    ) {
                        let styled = lines_to_ansi_text(&filtered)
                            .or_else(|| lines_to_plain_text(&filtered));
                        lines = filtered;
                        styled
                    } else {
                        lines_to_plain_text(&lines)
                    };
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
                    let styled = lines_to_plain_text(&lines);
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
                    let styled = lines_to_plain_text(&lines);
                    Preview {
                        lines,
                        styled,
                        title: Some(session.id.clone()),
                        timestamp: format_dual_time(session.last_active),
                        updated_at: Instant::now(),
                    }
                }
            },
            Entry::Profile(profile) => {
                let mut lines = Vec::new();
                lines.push(format!("Provider: {}", profile.provider));
                if let Some(description) = &profile.description {
                    lines.push(String::new());
                    lines.push(description.clone());
                }
                if !profile.tags.is_empty() && !matches!(profile.kind, ProfileKind::Provider) {
                    lines.push(String::new());
                    lines.push(format!("Tags: {}", profile.tags.join(", ")));
                }
                let styled = lines_to_plain_text(&lines);
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
                let styled = lines_to_plain_text(&lines);
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
}

fn apply_preview_filter(filter: Option<&Vec<String>>, lines: &[String]) -> Option<Vec<String>> {
    let args = filter?;
    if args.is_empty() {
        return None;
    }

    let input = lines.join("\n");
    let output = run_preview_filter(args, &input)?;
    let out = output.lines().map(str::to_string).collect::<Vec<_>>();
    if out.is_empty() {
        return None;
    }
    Some(out)
}

fn lines_to_plain_text(lines: &[String]) -> Option<Text<'static>> {
    if lines.is_empty() {
        return None;
    }
    Some(Text::from(lines.join("\n")))
}

fn lines_to_ansi_text(lines: &[String]) -> Option<Text<'static>> {
    if lines.is_empty() {
        return None;
    }
    lines.join("\n").into_text().ok()
}

fn run_preview_filter(args: &[String], input: &str) -> Option<String> {
    let mut command = Command::new(&args[0]);
    if args.len() > 1 {
        command.args(&args[1..]);
    }
    if env::var_os("TERM").is_none() {
        command.env("TERM", "xterm-256color");
    }
    if env::var_os("COLORTERM").is_none() {
        command.env("COLORTERM", "truecolor");
    }
    if env::var_os("CLICOLOR_FORCE").is_none() {
        command.env("CLICOLOR_FORCE", "1");
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        if !input.is_empty() && stdin.write_all(input.as_bytes()).is_err() {
            return None;
        }
        if !input.ends_with('\n') && stdin.write_all(b"\n").is_err() {
            return None;
        }
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
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
