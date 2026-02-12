use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use color_eyre::Result;
use serde_json::Value;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::db::Database;
use crate::session::{SessionSummary, TokenUsageRecord};

const PRICING_AS_OF: &str = "2026-01-21";

static LOCAL_TIMESTAMP_FORMAT: &[FormatItem<'static>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory]:[offset_minute]"
);
static DATE_FORMAT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

#[derive(Clone, Copy)]
struct PricingRate {
    input: f64,
    cached_input: f64,
    output: f64,
}

#[derive(Default, Clone, Copy)]
struct UsageTotals {
    input: i64,
    cached_input: i64,
    output: i64,
    reasoning_output: i64,
    total: i64,
}

#[derive(Default)]
struct WindowCounts {
    last_24h: i64,
    last_7d: i64,
    last_30d: i64,
    all: i64,
}

#[derive(Default)]
struct WindowCosts {
    last_24h: f64,
    last_7d: f64,
    last_30d: f64,
    all: f64,
}

#[derive(Default, Clone, Copy)]
struct SessionActivityCounts {
    turns: i64,
    compactions: i64,
}

struct TokenStats {
    totals: WindowedUsage,
    by_day: BTreeMap<String, UsageTotals>,
    model_totals: HashMap<String, UsageTotals>,
    cost_totals: WindowCosts,
    cost_by_pricing_model: HashMap<String, f64>,
    unpriced_tokens: WindowCounts,
    unpriced_models: HashSet<String>,
    latest_event: Option<i64>,
    latest_rate_limits: Option<String>,
}

struct CodexStats {
    generated_at: i64,
    sessions_count: usize,
    first_session: Option<i64>,
    last_session: Option<i64>,
    top_sessions_by_turns: Vec<(String, i64)>,
    top_session_by_compactions: Option<(String, i64)>,
    prompt_totals: WindowCounts,
    last_prompt: Option<i64>,
    token_stats: TokenStats,
}

/// Render Codex usage statistics from the indexed database.
///
/// # Errors
///
/// Returns an error if the database query fails or the output cannot be rendered.
pub fn codex(db: &Database, db_path: &Path) -> Result<()> {
    let now_ts = OffsetDateTime::now_utc().unix_timestamp();
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let cutoff_day = now_ts.saturating_sub(24 * 60 * 60);
    let cutoff_week = now_ts.saturating_sub(7 * 24 * 60 * 60);
    let cutoff_month = now_ts.saturating_sub(30 * 24 * 60 * 60);

    let sessions = db.sessions_for_provider("codex")?;
    let (first_session, last_session) = session_window(&sessions);
    let session_activity = collect_session_activity_counts(&sessions);

    let prompt_timestamps = db.user_message_timestamps("codex")?;
    let (prompt_totals, last_prompt) =
        prompt_stats(&prompt_timestamps, cutoff_day, cutoff_week, cutoff_month);

    let token_usage = db.token_usage_for_provider("codex")?;
    let token_stats =
        collect_token_stats(&token_usage, cutoff_day, cutoff_week, cutoff_month, offset);

    let stats = CodexStats {
        generated_at: now_ts,
        sessions_count: sessions.len(),
        first_session,
        last_session,
        top_sessions_by_turns: top_sessions_by_turns(&session_activity, 5),
        top_session_by_compactions: top_session_by_compactions(&session_activity),
        prompt_totals,
        last_prompt,
        token_stats,
    };

    render_codex_stats(&stats, db_path, offset);
    Ok(())
}

#[derive(Default)]
struct WindowedUsage {
    last_24h: UsageTotals,
    last_7d: UsageTotals,
    last_30d: UsageTotals,
    all: UsageTotals,
}

impl WindowedUsage {
    fn add(&mut self, usage: &TokenUsageRecord, in_day: bool, in_week: bool, in_month: bool) {
        self.all.add(usage);
        if in_day {
            self.last_24h.add(usage);
        }
        if in_week {
            self.last_7d.add(usage);
        }
        if in_month {
            self.last_30d.add(usage);
        }
    }
}

impl UsageTotals {
    fn add(&mut self, usage: &TokenUsageRecord) {
        self.input += usage.input_tokens;
        self.cached_input += usage.cached_input_tokens;
        self.output += usage.output_tokens;
        self.reasoning_output += usage.reasoning_output_tokens;
        self.total += usage.total_tokens;
    }
}

fn collect_token_stats(
    token_usage: &[TokenUsageRecord],
    cutoff_day: i64,
    cutoff_week: i64,
    cutoff_month: i64,
    offset: UtcOffset,
) -> TokenStats {
    let mut totals = WindowedUsage::default();
    let mut by_day: BTreeMap<String, UsageTotals> = BTreeMap::new();
    let mut model_totals: HashMap<String, UsageTotals> = HashMap::new();
    let mut cost_totals = WindowCosts::default();
    let mut cost_by_pricing_model: HashMap<String, f64> = HashMap::new();
    let mut unpriced_tokens = WindowCounts::default();
    let mut unpriced_models: HashSet<String> = HashSet::new();
    let mut latest_event: Option<i64> = None;
    let mut latest_rate_limits: Option<String> = None;

    for usage in token_usage {
        let ts = usage.timestamp;
        let in_day = ts >= cutoff_day;
        let in_week = ts >= cutoff_week;
        let in_month = ts >= cutoff_month;

        totals.add(usage, in_day, in_week, in_month);

        let day_key = fmt_date(ts, offset);
        let day_entry = by_day.entry(day_key).or_default();
        day_entry.add(usage);

        let model_key = usage.model.clone().unwrap_or_else(|| "unknown".to_string());
        let model_entry = model_totals.entry(model_key.clone()).or_default();
        model_entry.add(usage);

        if let Some(model) = usage.model.as_deref().and_then(pricing_model_for) {
            let rate = pricing_rate(model);
            let cost = usage_cost(usage, rate);
            cost_totals.all += cost;
            *cost_by_pricing_model
                .entry(model.to_string())
                .or_insert(0.0) += cost;
            if in_day {
                cost_totals.last_24h += cost;
            }
            if in_week {
                cost_totals.last_7d += cost;
            }
            if in_month {
                cost_totals.last_30d += cost;
            }
        } else {
            unpriced_models.insert(model_key);
            unpriced_tokens.all += usage.total_tokens;
            if in_day {
                unpriced_tokens.last_24h += usage.total_tokens;
            }
            if in_week {
                unpriced_tokens.last_7d += usage.total_tokens;
            }
            if in_month {
                unpriced_tokens.last_30d += usage.total_tokens;
            }
        }

        if latest_event.is_none_or(|current| ts > current) {
            latest_event = Some(ts);
            latest_rate_limits.clone_from(&usage.rate_limits);
        }
    }

    TokenStats {
        totals,
        by_day,
        model_totals,
        cost_totals,
        cost_by_pricing_model,
        unpriced_tokens,
        unpriced_models,
        latest_event,
        latest_rate_limits,
    }
}

fn render_codex_stats(stats: &CodexStats, db_path: &Path, offset: UtcOffset) {
    println!(
        "Codex Stats (generated {})",
        fmt_dt(Some(stats.generated_at), offset)
    );
    println!("Data sources:");
    println!("  database: {}", db_path.display());
    println!();

    println!("Sessions:");
    println!(
        "  count: {}",
        human_int(i64::try_from(stats.sessions_count).unwrap_or(0))
    );
    println!("  first seen: {}", fmt_dt(stats.first_session, offset));
    println!("  last seen: {}", fmt_dt(stats.last_session, offset));
    println!();
    render_session_activity(stats);

    println!("Prompts (indexed user messages):");
    println!("  last 24h: {}", human_int(stats.prompt_totals.last_24h));
    println!("  last 7d: {}", human_int(stats.prompt_totals.last_7d));
    println!("  last 30d: {}", human_int(stats.prompt_totals.last_30d));
    println!("  all time: {}", human_int(stats.prompt_totals.all));
    println!("  last prompt: {}", fmt_dt(stats.last_prompt, offset));
    println!();

    println!("Token usage (token_count events):");
    println!(
        "  {}",
        usage_line("last 24h", &stats.token_stats.totals.last_24h)
    );
    println!(
        "  {}",
        usage_line("last 7d", &stats.token_stats.totals.last_7d)
    );
    println!(
        "  {}",
        usage_line("last 30d", &stats.token_stats.totals.last_30d)
    );
    println!(
        "  {}",
        usage_line("all time", &stats.token_stats.totals.all)
    );
    println!();

    render_costs(stats);
    render_daily_totals(stats);
    render_top_costs(stats);
    render_top_models(stats);

    for line in render_rate_limits(
        stats.token_stats.latest_event,
        stats.token_stats.latest_rate_limits.as_deref(),
        offset,
    ) {
        println!("{line}");
    }
}

fn render_session_activity(stats: &CodexStats) {
    println!("Top sessions by turns (turn_context events):");
    if stats.top_sessions_by_turns.is_empty() {
        println!("  none");
    } else {
        for (session_id, turns) in &stats.top_sessions_by_turns {
            println!("  {session_id}: {}", human_int(*turns));
        }
    }
    println!();

    println!("Session with most compactions (context_compacted events):");
    if let Some((session_id, compactions)) = &stats.top_session_by_compactions {
        println!("  {session_id}: {}", human_int(*compactions));
    } else {
        println!("  none");
    }
    println!();
}

fn render_costs(stats: &CodexStats) {
    let token_stats = &stats.token_stats;
    if token_stats.cost_totals.all <= 0.0 && token_stats.unpriced_models.is_empty() {
        return;
    }

    println!("API-equivalent cost estimate (USD, pricing as of {PRICING_AS_OF}):");
    println!("  last 24h: {}", fmt_usd(token_stats.cost_totals.last_24h));
    println!("  last 7d: {}", fmt_usd(token_stats.cost_totals.last_7d));
    println!("  last 30d: {}", fmt_usd(token_stats.cost_totals.last_30d));
    println!("  all time: {}", fmt_usd(token_stats.cost_totals.all));
    if !token_stats.unpriced_models.is_empty() {
        render_unpriced_models(token_stats);
    }
    println!();
}

fn render_unpriced_models(token_stats: &TokenStats) {
    let mut models: Vec<_> = token_stats
        .unpriced_models
        .iter()
        .filter(|model| !model.is_empty())
        .cloned()
        .collect();
    models.sort();
    let list = if models.is_empty() {
        "unknown".to_string()
    } else {
        models.join(", ")
    };
    println!("  unpriced models: {list}");
    println!(
        "  unpriced tokens (all time): {}",
        human_int(token_stats.unpriced_tokens.all)
    );
}

fn render_daily_totals(stats: &CodexStats) {
    if stats.token_stats.by_day.is_empty() {
        return;
    }
    println!("Daily token totals (last 7 days):");
    for (day, usage) in stats.token_stats.by_day.iter().rev().take(7) {
        println!("  {day}: total {}", human_int(usage.total));
    }
    println!();
}

fn render_top_costs(stats: &CodexStats) {
    if stats.token_stats.cost_by_pricing_model.is_empty() {
        return;
    }
    println!("Top pricing models by cost (all time):");
    let mut rows: Vec<_> = stats.token_stats.cost_by_pricing_model.iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(a.1));
    for (model, cost) in rows.into_iter().take(5) {
        println!("  {model}: {}", fmt_usd(*cost));
    }
    println!();
}

fn render_top_models(stats: &CodexStats) {
    if stats.token_stats.model_totals.is_empty() {
        return;
    }
    println!("Top models by total tokens:");
    let mut rows: Vec<_> = stats.token_stats.model_totals.iter().collect();
    rows.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    for (model, usage) in rows.into_iter().take(5) {
        println!("  {model}: total {}", human_int(usage.total));
    }
    println!();
}

fn session_window(sessions: &[SessionSummary]) -> (Option<i64>, Option<i64>) {
    let mut first: Option<i64> = None;
    let mut last: Option<i64> = None;

    for session in sessions {
        let start = session
            .started_at
            .or(session.created_at)
            .or(session.last_active);
        first = start
            .map(|ts| first.map_or(ts, |current| current.min(ts)))
            .or(first);

        let end = session
            .last_active
            .or(session.started_at)
            .or(session.created_at);
        last = end
            .map(|ts| last.map_or(ts, |current| current.max(ts)))
            .or(last);
    }

    (first, last)
}

fn collect_session_activity_counts(
    sessions: &[SessionSummary],
) -> Vec<(String, SessionActivityCounts)> {
    sessions
        .iter()
        .map(|session| (session.id.clone(), session_activity_counts(&session.path)))
        .collect()
}

fn session_activity_counts(path: &Path) -> SessionActivityCounts {
    let Ok(file) = File::open(path) else {
        return SessionActivityCounts::default();
    };
    let reader = BufReader::new(file);
    let mut counts = SessionActivityCounts::default();

    for line in reader.lines().map_while(std::result::Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if parsed.get("type").and_then(Value::as_str) == Some("turn_context") {
            counts.turns += 1;
            continue;
        }

        if parsed.get("type").and_then(Value::as_str) == Some("event_msg")
            && parsed
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|ty| ty == "context_compacted")
        {
            counts.compactions += 1;
        }
    }

    counts
}

fn top_sessions_by_turns(
    rows: &[(String, SessionActivityCounts)],
    limit: usize,
) -> Vec<(String, i64)> {
    let mut ranked: Vec<_> = rows
        .iter()
        .filter(|(_, counts)| counts.turns > 0)
        .map(|(session_id, counts)| (session_id.clone(), counts.turns))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(limit);
    ranked
}

fn top_session_by_compactions(rows: &[(String, SessionActivityCounts)]) -> Option<(String, i64)> {
    let mut ranked: Vec<_> = rows
        .iter()
        .filter(|(_, counts)| counts.compactions > 0)
        .map(|(session_id, counts)| (session_id.clone(), counts.compactions))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().next()
}

fn prompt_stats(
    timestamps: &[i64],
    cutoff_day: i64,
    cutoff_week: i64,
    cutoff_month: i64,
) -> (WindowCounts, Option<i64>) {
    let mut totals = WindowCounts::default();
    let mut last_prompt = None;

    for ts in timestamps {
        let ts = *ts;
        totals.all += 1;
        if ts >= cutoff_day {
            totals.last_24h += 1;
        }
        if ts >= cutoff_week {
            totals.last_7d += 1;
        }
        if ts >= cutoff_month {
            totals.last_30d += 1;
        }
        if last_prompt.is_none_or(|current| ts > current) {
            last_prompt = Some(ts);
        }
    }

    (totals, last_prompt)
}

fn pricing_model_for(model: &str) -> Option<&'static str> {
    let model = model.to_ascii_lowercase();
    if model.starts_with("gpt-5.2") {
        Some("gpt-5.2")
    } else if model.starts_with("gpt-5.1") {
        Some("gpt-5.1")
    } else if model.starts_with("gpt-5-mini") {
        Some("gpt-5-mini")
    } else if model.starts_with("gpt-5") {
        Some("gpt-5")
    } else {
        None
    }
}

fn pricing_rate(model: &str) -> PricingRate {
    match model {
        "gpt-5.2" => PricingRate {
            input: 1.75,
            cached_input: 0.175,
            output: 14.0,
        },
        "gpt-5-mini" => PricingRate {
            input: 0.25,
            cached_input: 0.025,
            output: 2.0,
        },
        _ => PricingRate {
            input: 1.25,
            cached_input: 0.125,
            output: 10.0,
        },
    }
}

#[allow(clippy::cast_precision_loss)]
fn usage_cost(usage: &TokenUsageRecord, rate: PricingRate) -> f64 {
    let billed_input = (usage.input_tokens - usage.cached_input_tokens).max(0) as f64;
    let cached = usage.cached_input_tokens as f64;
    let output = usage.output_tokens as f64;
    let per_token = 1_000_000.0;
    let input_cost = billed_input * rate.input / per_token;
    let cached_cost = cached * rate.cached_input / per_token;
    let output_cost = output * rate.output / per_token;
    input_cost + cached_cost + output_cost
}

fn usage_line(label: &str, usage: &UsageTotals) -> String {
    format!(
        "{label}: input {} (cached {}) output {} reasoning {} total {}",
        human_int(usage.input),
        human_int(usage.cached_input),
        human_int(usage.output),
        human_int(usage.reasoning_output),
        human_int(usage.total),
    )
}

fn fmt_dt(ts: Option<i64>, offset: UtcOffset) -> String {
    let Some(ts) = ts else {
        return "unknown".to_string();
    };
    let dt = OffsetDateTime::from_unix_timestamp(ts).ok();
    let Some(dt) = dt else {
        return "unknown".to_string();
    };
    let local = dt.to_offset(offset);
    local
        .format(LOCAL_TIMESTAMP_FORMAT)
        .unwrap_or_else(|_| "unknown".to_string())
}

fn fmt_date(ts: i64, offset: UtcOffset) -> String {
    let dt = OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .map(|dt| dt.to_offset(offset));
    dt.and_then(|dt| dt.format(DATE_FORMAT).ok())
        .unwrap_or_else(|| "unknown".to_string())
}

fn human_int(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let mut digits = value.abs().to_string();
    let mut out = String::new();
    while digits.len() > 3 {
        let chunk = digits.split_off(digits.len() - 3);
        out = format!(",{chunk}{out}");
    }
    format!("{sign}{digits}{out}")
}

fn fmt_usd(amount: f64) -> String {
    let sign = if amount < 0.0 { "-" } else { "" };
    let formatted = format!("{:.2}", amount.abs());
    let mut parts = formatted.split('.');
    let integer = parts.next().unwrap_or("0");
    let fractional = parts.next().unwrap_or("00");
    format!("{sign}${}.{}", human_int_str(integer), fractional)
}

fn human_int_str(raw: &str) -> String {
    let mut digits = raw.to_string();
    let mut out = String::new();
    while digits.len() > 3 {
        let chunk = digits.split_off(digits.len() - 3);
        out = format!(",{chunk}{out}");
    }
    format!("{digits}{out}")
}

fn render_rate_limits(
    latest_token_event: Option<i64>,
    rate_limits: Option<&str>,
    offset: UtcOffset,
) -> Vec<String> {
    let Some(latest_ts) = latest_token_event else {
        return Vec::new();
    };
    let Some(rate_limits) = rate_limits else {
        return Vec::new();
    };
    let parsed: Value = match serde_json::from_str(rate_limits) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let mut lines = Vec::new();
    lines.push(format!(
        "Rate limits (as of {}):",
        fmt_dt(Some(latest_ts), offset)
    ));

    if let Some(primary) = parsed.get("primary").and_then(Value::as_object) {
        lines.push(format_rate_limit_line("primary", primary, offset));
    }

    if let Some(secondary) = parsed.get("secondary").and_then(Value::as_object) {
        lines.push(format_rate_limit_line("secondary", secondary, offset));
    }

    let credits = parsed.get("credits");
    if let Some(credits) = credits.and_then(Value::as_object) {
        let has_credits = display_value(credits.get("has_credits"));
        let unlimited = display_value(credits.get("unlimited"));
        let balance = display_value(credits.get("balance"));
        lines.push(format!(
            "  credits: has_credits {has_credits} unlimited {unlimited} balance {balance}"
        ));
    }

    if let Some(plan_type) = parsed.get("plan_type") {
        lines.push(format!("  plan_type: {}", display_value(Some(plan_type))));
    }

    lines
}

fn format_rate_limit_line(
    label: &str,
    section: &serde_json::Map<String, Value>,
    offset: UtcOffset,
) -> String {
    let used = display_value(section.get("used_percent"));
    let window = display_value(section.get("window_minutes"));
    let reset_dt = section
        .get("resets_at")
        .and_then(parse_epoch)
        .map_or_else(|| "unknown".to_string(), |ts| fmt_dt(Some(ts), offset));
    format!("  {label}: used {used}% window {window}m resets {reset_dt}")
}

fn parse_epoch(value: &Value) -> Option<i64> {
    match value {
        Value::Number(num) => parse_epoch_number(num),
        Value::String(text) => parse_epoch_str(text),
        _ => None,
    }
}

fn parse_epoch_number(num: &serde_json::Number) -> Option<i64> {
    if let Some(value) = num.as_i64() {
        Some(normalize_epoch(value))
    } else if let Some(value) = num.as_u64() {
        i64::try_from(value).ok().map(normalize_epoch)
    } else {
        None
    }
}

fn parse_epoch_str(text: &str) -> Option<i64> {
    if let Ok(value) = text.parse::<i64>() {
        Some(normalize_epoch(value))
    } else if let Ok(value) = text.parse::<u64>() {
        i64::try_from(value).ok().map(normalize_epoch)
    } else {
        None
    }
}

fn normalize_epoch(value: i64) -> i64 {
    if value > 1_000_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn display_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(num)) => num.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Null) | None => "unknown".to_string(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::session::{MessageRecord, SessionIngest, SessionSummary};
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::path::PathBuf;
    use time::OffsetDateTime;

    #[test]
    fn pricing_model_matches_prefixes() {
        assert_eq!(pricing_model_for("gpt-5.2-preview"), Some("gpt-5.2"));
        assert_eq!(pricing_model_for("gpt-5.1"), Some("gpt-5.1"));
        assert_eq!(pricing_model_for("gpt-5-mini-2025"), Some("gpt-5-mini"));
        assert_eq!(pricing_model_for("gpt-5"), Some("gpt-5"));
        assert_eq!(pricing_model_for("o3-mini"), None);
    }

    #[test]
    fn usage_cost_uses_cached_input_rate() {
        let usage = TokenUsageRecord {
            session_id: "sess".into(),
            timestamp: 0,
            input_tokens: 2_000_000,
            cached_input_tokens: 500_000,
            output_tokens: 1_000_000,
            reasoning_output_tokens: 0,
            total_tokens: 3_000_000,
            model: Some("gpt-5".into()),
            rate_limits: None,
        };
        let rate = pricing_rate("gpt-5");
        let cost = usage_cost(&usage, rate);
        let expected = (1_500_000.0 * 1.25 / 1_000_000.0)
            + (500_000.0 * 0.125 / 1_000_000.0)
            + (1_000_000.0 * 10.0 / 1_000_000.0);
        assert!((cost - expected).abs() < 1e-6);
    }

    #[test]
    fn parse_epoch_handles_seconds_and_millis() {
        let seconds = Value::Number(1_700_000_000i64.into());
        let millis = Value::Number(1_700_000_000_000i64.into());
        let from_seconds = parse_epoch(&seconds).expect("seconds");
        let from_millis = parse_epoch(&millis).expect("millis");
        assert_eq!(from_seconds, 1_700_000_000);
        assert_eq!(from_millis, 1_700_000_000);

        let from_str = parse_epoch(&Value::String("1700000000".into())).expect("string");
        assert_eq!(from_str, 1_700_000_000);
    }

    #[test]
    fn render_rate_limits_formats_entries() {
        let payload = r#"{
            "primary": {"used_percent": 20, "window_minutes": 60, "resets_at": 1700000000000},
            "secondary": {"used_percent": 5, "window_minutes": 15, "resets_at": 1700000000},
            "credits": {"has_credits": true, "unlimited": false, "balance": 7},
            "plan_type": "pro"
        }"#;
        let lines = render_rate_limits(Some(0), Some(payload), UtcOffset::UTC);
        assert!(lines.iter().any(|line| line.contains("primary:")));
        assert!(lines.iter().any(|line| line.contains("secondary:")));
        assert!(lines.iter().any(|line| line.contains("credits:")));
        assert!(lines.iter().any(|line| line.contains("plan_type:")));
    }

    #[test]
    fn collect_token_stats_tracks_windows_and_costs() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let usage_priced = TokenUsageRecord {
            session_id: "sess".into(),
            timestamp: now - 10,
            input_tokens: 1000,
            cached_input_tokens: 100,
            output_tokens: 500,
            reasoning_output_tokens: 0,
            total_tokens: 1500,
            model: Some("gpt-5.2".into()),
            rate_limits: None,
        };
        let usage_unpriced = TokenUsageRecord {
            session_id: "sess".into(),
            timestamp: now - 40 * 24 * 60 * 60,
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 5,
            reasoning_output_tokens: 0,
            total_tokens: 15,
            model: None,
            rate_limits: None,
        };
        let stats = collect_token_stats(
            &[usage_priced, usage_unpriced],
            now - 24 * 60 * 60,
            now - 7 * 24 * 60 * 60,
            now - 30 * 24 * 60 * 60,
            UtcOffset::UTC,
        );
        assert!(stats.cost_totals.all > 0.0);
        assert!(stats.unpriced_tokens.all > 0);
        assert!(!stats.by_day.is_empty());
    }

    #[test]
    fn codex_stats_smoke_test() {
        let temp = TempDir::new().expect("temp dir");
        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path()).expect("open database");
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let summary = SessionSummary {
            id: "codex/demo".into(),
            provider: "codex".into(),
            wrapper: None,
            model: Some("gpt-5.2".into()),
            label: Some("Demo".into()),
            path: db_path.path().to_path_buf(),
            uuid: Some("demo".into()),
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(now - 120),
            started_at: Some(now - 120),
            last_active: Some(now),
            size: 1,
            mtime: now,
        };
        let mut message = MessageRecord::new(
            summary.id.clone(),
            0,
            "user",
            "Hello",
            None,
            Some(now - 120),
        );
        message.is_first = true;
        let usage_priced = TokenUsageRecord {
            session_id: summary.id.clone(),
            timestamp: now - 60,
            input_tokens: 1000,
            cached_input_tokens: 100,
            output_tokens: 500,
            reasoning_output_tokens: 0,
            total_tokens: 1500,
            model: Some("gpt-5.2".into()),
            rate_limits: Some(
                r#"{"primary":{"used_percent":20,"window_minutes":60,"resets_at":1700000000000},"secondary":{"used_percent":5,"window_minutes":15,"resets_at":1700000000},"credits":{"has_credits":true,"unlimited":false,"balance":7},"plan_type":"pro"}"#.into(),
            ),
        };
        let usage_unpriced = TokenUsageRecord {
            session_id: summary.id.clone(),
            timestamp: now - 30,
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 5,
            reasoning_output_tokens: 0,
            total_tokens: 15,
            model: None,
            rate_limits: None,
        };
        let ingest = SessionIngest::new(summary, vec![message])
            .with_token_usage(vec![usage_priced, usage_unpriced]);
        db.upsert_session(&ingest).expect("insert session");

        codex(&db, db_path.path()).expect("render codex stats");
        drop(db);
    }

    #[test]
    fn collect_session_activity_counts_tracks_turns_and_compactions() {
        let temp = TempDir::new().expect("temp dir");
        let session_file = temp.child("session.jsonl");
        session_file
            .write_str(concat!(
                "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"context_compacted\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":null}}\n",
                "{not-json}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
            ))
            .expect("write session");

        let summary = SessionSummary {
            id: "codex/session.jsonl".into(),
            provider: "codex".into(),
            wrapper: None,
            model: Some("gpt-5".into()),
            label: Some("Session".into()),
            path: session_file.path().to_path_buf(),
            uuid: Some("session".into()),
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(0),
            started_at: Some(0),
            last_active: Some(0),
            size: 1,
            mtime: 0,
        };

        let rows = collect_session_activity_counts(&[summary]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "codex/session.jsonl");
        assert_eq!(rows[0].1.turns, 2);
        assert_eq!(rows[0].1.compactions, 1);
    }

    #[test]
    fn session_activity_rankings_are_sorted_and_filtered() {
        let rows = vec![
            (
                "codex/b".to_string(),
                SessionActivityCounts {
                    turns: 5,
                    compactions: 1,
                },
            ),
            (
                "codex/a".to_string(),
                SessionActivityCounts {
                    turns: 5,
                    compactions: 0,
                },
            ),
            (
                "codex/c".to_string(),
                SessionActivityCounts {
                    turns: 2,
                    compactions: 3,
                },
            ),
            (
                "codex/d".to_string(),
                SessionActivityCounts {
                    turns: 0,
                    compactions: 0,
                },
            ),
        ];

        let top_turns = top_sessions_by_turns(&rows, 5);
        assert_eq!(
            top_turns,
            vec![
                ("codex/a".to_string(), 5),
                ("codex/b".to_string(), 5),
                ("codex/c".to_string(), 2),
            ]
        );

        let top_compactions = top_session_by_compactions(&rows);
        assert_eq!(top_compactions, Some(("codex/c".to_string(), 3)));

        let no_compactions = top_session_by_compactions(&[(
            "codex/none".to_string(),
            SessionActivityCounts {
                turns: 1,
                compactions: 0,
            },
        )]);
        assert_eq!(no_compactions, None);
    }

    #[test]
    fn render_codex_stats_exercises_non_empty_sections() {
        let mut by_day = BTreeMap::new();
        by_day.insert(
            "2026-01-01".to_string(),
            UsageTotals {
                total: 42,
                ..UsageTotals::default()
            },
        );
        let mut model_totals = HashMap::new();
        model_totals.insert(
            "gpt-5".to_string(),
            UsageTotals {
                total: 42,
                ..UsageTotals::default()
            },
        );
        let mut cost_by_pricing_model = HashMap::new();
        cost_by_pricing_model.insert("gpt-5".to_string(), 1.25);
        let mut unpriced_models = HashSet::new();
        unpriced_models.insert(String::new());

        let stats = CodexStats {
            generated_at: 0,
            sessions_count: 1,
            first_session: Some(0),
            last_session: Some(1),
            top_sessions_by_turns: vec![("codex/demo".to_string(), 3)],
            top_session_by_compactions: Some(("codex/demo".to_string(), 1)),
            prompt_totals: WindowCounts {
                last_24h: 1,
                last_7d: 2,
                last_30d: 3,
                all: 4,
            },
            last_prompt: Some(1),
            token_stats: TokenStats {
                totals: WindowedUsage {
                    all: UsageTotals {
                        input: 10,
                        cached_input: 1,
                        output: 5,
                        reasoning_output: 0,
                        total: 15,
                    },
                    ..WindowedUsage::default()
                },
                by_day,
                model_totals,
                cost_totals: WindowCosts {
                    last_24h: 1.0,
                    last_7d: 1.0,
                    last_30d: 1.0,
                    all: 1.0,
                },
                cost_by_pricing_model,
                unpriced_tokens: WindowCounts {
                    all: 15,
                    ..WindowCounts::default()
                },
                unpriced_models,
                latest_event: Some(1),
                latest_rate_limits: Some(
                    r#"{
                        "primary":{"used_percent":20,"window_minutes":60,"resets_at":1700000000000},
                        "secondary":{"used_percent":10,"window_minutes":15,"resets_at":1700000000},
                        "credits":{"has_credits":true,"unlimited":false,"balance":7},
                        "plan_type":"pro"
                    }"#
                    .into(),
                ),
            },
        };

        render_codex_stats(&stats, Path::new("/tmp/tx.sqlite3"), UtcOffset::UTC);
    }

    #[test]
    fn session_window_prefers_earliest_start_and_latest_end() {
        let sessions = vec![
            SessionSummary {
                id: "a".into(),
                provider: "codex".into(),
                wrapper: None,
                model: None,
                label: None,
                path: PathBuf::from("a.jsonl"),
                uuid: None,
                first_prompt: None,
                actionable: true,
                created_at: Some(20),
                started_at: Some(10),
                last_active: Some(30),
                size: 1,
                mtime: 30,
            },
            SessionSummary {
                id: "b".into(),
                provider: "codex".into(),
                wrapper: None,
                model: None,
                label: None,
                path: PathBuf::from("b.jsonl"),
                uuid: None,
                first_prompt: None,
                actionable: true,
                created_at: Some(5),
                started_at: None,
                last_active: Some(40),
                size: 1,
                mtime: 40,
            },
        ];

        let (first, last) = session_window(&sessions);
        assert_eq!(first, Some(5));
        assert_eq!(last, Some(40));
    }

    #[test]
    fn session_activity_counts_returns_default_when_file_missing() {
        let counts = session_activity_counts(Path::new("/tmp/tx-stats-missing-file.jsonl"));
        assert_eq!(counts.turns, 0);
        assert_eq!(counts.compactions, 0);
    }

    #[test]
    fn session_activity_counts_skips_empty_lines() {
        let temp = TempDir::new().expect("temp dir");
        let session_file = temp.child("session.jsonl");
        session_file
            .write_str(concat!(
                "\n",
                "   \n",
                "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
            ))
            .expect("write session");

        let counts = session_activity_counts(session_file.path());
        assert_eq!(counts.turns, 1);
        assert_eq!(counts.compactions, 0);
    }

    #[test]
    fn pricing_rate_includes_gpt_5_mini_tier() {
        let rate = pricing_rate("gpt-5-mini");
        assert!((rate.input - 0.25).abs() < f64::EPSILON);
        assert!((rate.cached_input - 0.025).abs() < f64::EPSILON);
        assert!((rate.output - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fmt_dt_returns_unknown_for_invalid_epoch() {
        let rendered = fmt_dt(Some(i64::MAX), UtcOffset::UTC);
        assert_eq!(rendered, "unknown");
    }

    #[test]
    fn human_int_str_adds_separators() {
        assert_eq!(human_int_str("123456789"), "123,456,789");
    }

    #[test]
    fn human_int_and_fmt_usd_support_negative_values() {
        assert_eq!(human_int(-1_234_567), "-1,234,567");
        assert_eq!(fmt_usd(-1234.5), "-$1,234.50");
    }

    #[test]
    fn render_rate_limits_returns_empty_for_invalid_json() {
        let lines = render_rate_limits(Some(0), Some("{bad-json}"), UtcOffset::UTC);
        assert!(lines.is_empty());
    }

    #[test]
    fn parse_epoch_handles_edge_numeric_and_string_inputs() {
        assert_eq!(parse_epoch(&Value::Bool(true)), None);

        let unsigned = serde_json::Number::from((i64::MAX as u64) + 1);
        assert_eq!(parse_epoch_number(&unsigned), None);

        let float = serde_json::Number::from_f64(1.5).expect("finite");
        assert_eq!(parse_epoch_number(&float), None);

        let large_text = ((i64::MAX as u64) + 1).to_string();
        assert_eq!(parse_epoch_str(&large_text), None);
        assert_eq!(parse_epoch_str("not-a-number"), None);
    }

    #[test]
    fn display_value_defaults_null_and_missing_to_unknown() {
        assert_eq!(display_value(None), "unknown");
        assert_eq!(display_value(Some(&Value::Null)), "unknown");
    }

    #[test]
    fn display_value_formats_non_scalar_values() {
        assert_eq!(
            display_value(Some(&serde_json::json!([1, 2, 3]))),
            "[1,2,3]"
        );
    }

    #[test]
    fn session_window_updates_first_and_last_when_timestamps_present() {
        let sessions = vec![SessionSummary {
            id: "x".into(),
            provider: "codex".into(),
            wrapper: None,
            model: None,
            label: None,
            path: PathBuf::from("x.jsonl"),
            uuid: None,
            first_prompt: None,
            actionable: true,
            created_at: Some(10),
            started_at: Some(20),
            last_active: Some(30),
            size: 1,
            mtime: 30,
        }];
        let (first, last) = session_window(&sessions);
        assert_eq!(first, Some(20));
        assert_eq!(last, Some(30));
    }

    #[test]
    fn render_rate_limits_includes_primary_and_secondary_sections() {
        let payload = r#"{
            "primary": {"used_percent": 10, "window_minutes": 60, "resets_at": 1700000000},
            "secondary": {"used_percent": 5, "window_minutes": 15, "resets_at": 1700000100}
        }"#;
        let lines = render_rate_limits(Some(1), Some(payload), UtcOffset::UTC);
        assert!(lines.iter().any(|line| line.contains("primary:")));
        assert!(lines.iter().any(|line| line.contains("secondary:")));
    }
}
