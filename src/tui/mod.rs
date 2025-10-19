use std::cmp;
use std::collections::HashMap;
use std::env;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use color_eyre::Result;
use color_eyre::eyre::eyre;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{ExecutableCommand, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::app::{self, UiContext};
use crate::config::model::SearchMode;
use crate::indexer::Indexer;
use crate::pipeline::{PipelinePlan, PipelineRequest, SessionContext, build_pipeline};
use crate::prompts::PromptStatus;
use crate::session::{SearchHit, SessionQuery};

const SESSION_LIMIT: usize = 200;

pub fn run<'a>(ctx: &'a mut UiContext<'a>) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let json_mode = ctx.cli.json;
    let result = run_app(ctx, &mut terminal);

    disable_raw_mode()?;
    terminal
        .backend_mut()
        .execute(crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    match result? {
        Some(Outcome::Emit(plan)) => app::emit_command(&plan, json_mode),
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
        terminal.draw(|frame| draw(frame, &mut state))?;
        if let Some(outcome) = state.outcome {
            return Ok(Some(outcome));
        }

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(event) => {
                    if state.handle_key(event)? {
                        return Ok(state.outcome);
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
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
    actionable_only: bool,
    full_text: bool,
    input_mode: InputMode,
    message: Option<String>,
    preview_cache: HashMap<String, Preview>,
    outcome: Option<Outcome>,
    list_state: ratatui::widgets::ListState,
}

#[derive(Debug, Clone)]
enum Entry {
    Session(SessionEntry),
    Profile(ProfileEntry),
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
}

#[derive(Debug, Clone)]
struct ProfileEntry {
    display: String,
    provider: String,
    pre: Vec<String>,
    post: Vec<String>,
    wrap: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    kind: ProfileKind,
}

#[derive(Debug, Clone)]
enum ProfileKind {
    Config { name: String },
    Virtual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Filter,
    Provider,
    Help,
}

#[derive(Debug, Clone)]
struct Preview {
    lines: Vec<String>,
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
        }
    }

    fn from_hit(hit: SearchHit) -> Self {
        Self {
            id: hit.session_id,
            provider: hit.provider,
            label: hit.label,
            first_prompt: None,
            actionable: true,
            last_active: hit.last_active,
            snippet: hit.snippet,
        }
    }

    fn matches(&self, needle: &str) -> bool {
        let needle = needle.to_ascii_lowercase();
        self.id.to_ascii_lowercase().contains(&needle)
            || self
                .label
                .as_ref()
                .map(|label| label.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false)
            || self
                .first_prompt
                .as_ref()
                .map(|prompt| prompt.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false)
            || self
                .snippet
                .as_ref()
                .map(|snippet| snippet.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false)
    }
}

impl ProfileEntry {
    fn matches(&self, needle: &str) -> bool {
        let haystack = format!(
            "{} {} {}",
            self.display,
            self.description.as_deref().unwrap_or(""),
            self.tags.join(" ")
        )
        .to_ascii_lowercase();
        haystack.contains(&needle.to_ascii_lowercase())
    }
}

impl<'ctx> AppState<'ctx> {
    fn new(ctx: &'ctx mut UiContext<'ctx>) -> Result<Self> {
        let defaults = &ctx.config.defaults;

        let mut state = Self {
            ctx,
            profiles: Vec::new(),
            entries: Vec::new(),
            index: 0,
            filter: String::new(),
            provider_filter: None,
            actionable_only: defaults.actionable_only,
            full_text: matches!(defaults.search_mode, SearchMode::FullText),
            input_mode: InputMode::Normal,
            message: None,
            preview_cache: HashMap::new(),
            outcome: None,
            list_state: ratatui::widgets::ListState::default(),
        };

        state.load_profiles()?;
        state.refresh_entries()?;
        state.list_state.select(Some(0));
        Ok(state)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        match self.input_mode {
            InputMode::Normal => self.handle_key_normal(key),
            InputMode::Filter => self.handle_key_filter(key),
            InputMode::Provider => self.handle_key_provider(key),
            InputMode::Help => {
                self.input_mode = InputMode::Normal;
                Ok(false)
            }
        }
    }

    fn load_profiles(&mut self) -> Result<()> {
        self.profiles.clear();

        for (name, profile) in &self.ctx.config.profiles {
            self.profiles.push(ProfileEntry {
                display: name.clone(),
                provider: profile.provider.clone(),
                pre: profile.pre.clone(),
                post: profile.post.clone(),
                wrap: profile.wrap.clone(),
                description: None,
                tags: Vec::new(),
                kind: ProfileKind::Config { name: name.clone() },
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
                            self.profiles.push(ProfileEntry {
                                display: vp.key.clone(),
                                provider: provider.clone(),
                                pre: vec!["assemble".to_string()],
                                post: Vec::new(),
                                wrap: None,
                                description: vp.description.clone(),
                                tags: vp.tags.clone(),
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

        Ok(())
    }

    fn refresh_entries(&mut self) -> Result<()> {
        let mut sessions = if self.full_text && !self.filter.is_empty() {
            self.search_sessions(&self.filter)?
        } else {
            self.load_sessions()?
        };

        if !self.filter.is_empty() && !self.full_text {
            let query = self.filter.to_ascii_lowercase();
            sessions.retain(|session| session.matches(&query));
        }

        if let Some(provider) = &self.provider_filter {
            sessions.retain(|session| session.provider == *provider);
        }

        if self.actionable_only {
            sessions.retain(|session| session.actionable);
        }

        let mut entries: Vec<Entry> = sessions.into_iter().map(Entry::Session).collect();

        for profile in &self.profiles {
            if let Some(provider) = &self.provider_filter {
                if &profile.provider != provider {
                    continue;
                }
            }
            if !self.filter.is_empty() {
                if !profile.matches(&self.filter) {
                    continue;
                }
            }
            entries.push(Entry::Profile(profile.clone()));
        }

        entries.sort_by(|a, b| match (a, b) {
            (Entry::Session(a), Entry::Session(b)) => b
                .last_active
                .unwrap_or_default()
                .cmp(&a.last_active.unwrap_or_default()),
            (Entry::Session(_), Entry::Profile(_)) => cmp::Ordering::Less,
            (Entry::Profile(_), Entry::Session(_)) => cmp::Ordering::Greater,
            (Entry::Profile(a), Entry::Profile(b)) => a.display.cmp(&b.display),
        });

        self.entries = entries;
        self.preview_cache.clear();

        if self.entries.is_empty() {
            self.index = 0;
            self.list_state.select(None);
            if !self.filter.is_empty() || self.provider_filter.is_some() {
                self.message = Some("no results".to_string());
            }
        } else {
            if self.index >= self.entries.len() {
                self.index = self.entries.len().saturating_sub(1);
            }
            self.list_state.select(Some(self.index));
            if matches!(self.message.as_deref(), Some("no results")) {
                self.message = None;
            }
        }

        Ok(())
    }

    fn load_sessions(&self) -> Result<Vec<SessionEntry>> {
        let queries = self.ctx.db.list_sessions(
            self.provider_filter.as_deref(),
            self.actionable_only,
            None,
            Some(SESSION_LIMIT),
        )?;
        Ok(queries.into_iter().map(SessionEntry::from_query).collect())
    }

    fn search_sessions(&self, term: &str) -> Result<Vec<SessionEntry>> {
        let hits = self.ctx.db.search_full_text(
            term,
            self.provider_filter.as_deref(),
            self.actionable_only,
        )?;
        Ok(hits.into_iter().map(SessionEntry::from_hit).collect())
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> Result<bool> {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => Ok(true),
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
                Ok(false)
            }
            (KeyCode::Char('p'), _) => {
                self.input_mode = InputMode::Provider;
                Ok(false)
            }
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.full_text = !self.full_text;
                self.refresh_entries()?;
                Ok(false)
            }
            (KeyCode::Char('a'), KeyModifiers::ALT) => {
                self.actionable_only = !self.actionable_only;
                self.refresh_entries()?;
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
            (KeyCode::Char('?'), _) => {
                self.input_mode = InputMode::Help;
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn handle_key_filter(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                Ok(false)
            }
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                Ok(false)
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.refresh_entries()?;
                Ok(false)
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.filter.push(ch);
                    self.refresh_entries()?;
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn handle_key_provider(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.provider_filter = None;
                self.input_mode = InputMode::Normal;
                self.refresh_entries()?;
                Ok(false)
            }
            KeyCode::Enter => {
                if let Some(provider) = &self.provider_filter {
                    if provider.trim().is_empty() {
                        self.provider_filter = None;
                    }
                }
                self.input_mode = InputMode::Normal;
                self.refresh_entries()?;
                Ok(false)
            }
            KeyCode::Backspace => {
                if let Some(filter) = &mut self.provider_filter {
                    filter.pop();
                    if filter.is_empty() {
                        self.provider_filter = None;
                    }
                }
                self.refresh_entries()?;
                Ok(false)
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    let filter = self.provider_filter.get_or_insert_with(String::new);
                    filter.push(ch);
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
        let len = self.entries.len() as isize;
        let mut idx = self.index as isize + delta;
        if idx < 0 {
            idx = 0;
        } else if idx >= len {
            idx = len - 1;
        }
        self.index = idx as usize;
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

                let request = PipelineRequest {
                    config: self.ctx.config,
                    provider_hint: Some(summary.provider.as_str()),
                    profile: None,
                    additional_pre: Vec::new(),
                    additional_post: Vec::new(),
                    wrap: None,
                    provider_args: Vec::new(),
                    vars: HashMap::new(),
                    session: SessionContext {
                        id: Some(summary.id.clone()),
                        label: summary.label.clone(),
                    },
                    cwd: summary
                        .path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| {
                            env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                        }),
                };
                Ok(Some(build_pipeline(request)?))
            }
            Entry::Profile(profile) => {
                let request = match profile.kind {
                    ProfileKind::Config { ref name } => PipelineRequest {
                        config: self.ctx.config,
                        provider_hint: Some(profile.provider.as_str()),
                        profile: Some(name.as_str()),
                        additional_pre: Vec::new(),
                        additional_post: Vec::new(),
                        wrap: profile.wrap.as_deref(),
                        provider_args: Vec::new(),
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
                        wrap: profile.wrap.as_deref(),
                        provider_args: Vec::new(),
                        vars: HashMap::new(),
                        session: SessionContext::default(),
                        cwd: env::current_dir()?,
                    },
                };
                Ok(Some(build_pipeline(request)?))
            }
        }
    }

    fn preview(&mut self) -> Preview {
        let entry = match self.entries.get(self.index) {
            Some(entry) => entry.clone(),
            None => {
                return Preview {
                    lines: vec!["No selection".to_string()],
                    updated_at: Instant::now(),
                };
            }
        };

        let key = match &entry {
            Entry::Session(session) => format!("session:{}", session.id),
            Entry::Profile(profile) => format!("profile:{}", profile.display),
        };

        if let Some(preview) = self.preview_cache.get(&key) {
            if preview.updated_at.elapsed() < Duration::from_secs(5) {
                return preview.clone();
            }
        }

        let preview = match entry {
            Entry::Session(session) => match self.ctx.db.fetch_transcript(&session.id) {
                Ok(Some(transcript)) => Preview {
                    lines: transcript
                        .messages
                        .iter()
                        .take(6)
                        .map(|msg| format!("{}: {}", msg.role, msg.content))
                        .collect(),
                    updated_at: Instant::now(),
                },
                Ok(None) => Preview {
                    lines: vec!["No transcript available".to_string()],
                    updated_at: Instant::now(),
                },
                Err(err) => Preview {
                    lines: vec![format!("preview error: {}", err)],
                    updated_at: Instant::now(),
                },
            },
            Entry::Profile(profile) => {
                let mut lines = Vec::new();
                lines.push(format!("Provider: {}", profile.provider));
                if let Some(description) = &profile.description {
                    lines.push(String::new());
                    lines.push(description.clone());
                }
                if !profile.tags.is_empty() {
                    lines.push(String::new());
                    lines.push(format!("Tags: {}", profile.tags.join(", ")));
                }
                Preview {
                    lines,
                    updated_at: Instant::now(),
                }
            }
        };

        self.preview_cache.insert(key, preview.clone());
        preview
    }
}

fn draw(frame: &mut Frame<'_>, state: &mut AppState<'_>) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(frame.area());
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(vertical[0]);

    draw_entries(frame, columns[0], state);
    draw_preview(frame, columns[1], state);
    draw_status(frame, vertical[1], state);

    if state.input_mode == InputMode::Help {
        draw_help(frame, frame.area());
    }
}

fn draw_entries(frame: &mut Frame<'_>, area: Rect, state: &mut AppState<'_>) {
    let items = state
        .entries
        .iter()
        .map(|entry| match entry {
            Entry::Session(session) => {
                let display = session
                    .label
                    .as_deref()
                    .unwrap_or_else(|| session.first_prompt.as_deref().unwrap_or("(no label)"));
                let mut spans = vec![Span::raw(truncate(display, 40))];
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    session.provider.clone(),
                    Style::default().fg(Color::Blue),
                ));
                ListItem::new(Line::from(spans))
            }
            Entry::Profile(profile) => {
                let mut spans = vec![Span::styled(
                    truncate(&profile.display, 40),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )];
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    profile.provider.clone(),
                    Style::default().fg(Color::Blue),
                ));
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
    let lines = preview
        .lines
        .iter()
        .map(|line| Line::from(line.clone()))
        .collect::<Vec<_>>();
    let block = Block::default().title("Preview").borders(Borders::ALL);
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, state: &AppState<'_>) {
    let filter = if state.filter.is_empty() {
        "filter: (off)".to_string()
    } else {
        format!("filter: {}", state.filter)
    };
    let provider = state
        .provider_filter
        .as_ref()
        .map(|p| format!("provider: {}", p))
        .unwrap_or_else(|| "provider: (all)".to_string());
    let flags = format!(
        "mode: {}{}",
        if state.full_text {
            "full-text"
        } else {
            "prompt"
        },
        if state.actionable_only {
            " | actionable"
        } else {
            ""
        }
    );
    let status = vec![
        Line::from(vec![
            Span::raw(filter),
            Span::raw("  |  "),
            Span::raw(provider),
            Span::raw("  |  "),
            Span::raw(flags),
        ]),
        Line::from(
            state
                .message
                .as_deref()
                .unwrap_or("Press Tab to emit, Enter to run, / to filter, ? for help")
                .to_string(),
        ),
    ];
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(Paragraph::new(status).block(block), area);
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    let help = vec![
        Line::from("Tab: emit command    Enter: run pipeline"),
        Line::from("/: filter    Ctrl-F: toggle full text    Alt-A: toggle actionable"),
        Line::from("p: provider filter    R: reindex    q/Esc: quit"),
    ];
    let chunk = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(40),
            Constraint::Percentage(30),
        ])
        .split(area)[1];
    let inner = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(60),
            Constraint::Percentage(20),
        ])
        .split(chunk)[1];
    let block = Block::default()
        .title("Help")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new(help).block(block).wrap(Wrap { trim: true });
    frame.render_widget(paragraph, inner);
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
