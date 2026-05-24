use chrono::{Local, SecondsFormat, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: &str = concat!("tokcat-core/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
}

impl TokenBreakdown {
    fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write + self.reasoning
    }

    fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input.max(0));
        self.output = self.output.saturating_add(other.output.max(0));
        self.cache_read = self.cache_read.saturating_add(other.cache_read.max(0));
        self.cache_write = self.cache_write.saturating_add(other.cache_write.max(0));
        self.reasoning = self.reasoning.saturating_add(other.reasoning.max(0));
    }
}

#[derive(Debug, Clone)]
struct UsageMessage {
    client: String,
    model_id: String,
    provider_id: String,
    timestamp_ms: i64,
    date: String,
    tokens: TokenBreakdown,
    cost: f64,
    messages: i32,
    dedup_key: Option<String>,
}

impl UsageMessage {
    fn new(
        client: impl Into<String>,
        model_id: impl Into<String>,
        provider_id: impl Into<String>,
        timestamp_ms: i64,
        tokens: TokenBreakdown,
        cost: f64,
    ) -> Self {
        Self {
            client: client.into(),
            model_id: normalize_model_id(&model_id.into()),
            provider_id: provider_id.into(),
            timestamp_ms,
            date: date_from_timestamp_ms(timestamp_ms),
            tokens,
            cost: cost.max(0.0),
            messages: 1,
            dedup_key: None,
        }
    }

    fn total_tokens(&self) -> i64 {
        self.tokens.total()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientContribution {
    client: String,
    model_id: String,
    provider_id: String,
    tokens: TokenBreakdown,
    cost: f64,
    messages: i32,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct DailyTotals {
    tokens: i64,
    cost: f64,
    messages: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DailyContribution {
    date: String,
    totals: DailyTotals,
    intensity: u8,
    token_breakdown: TokenBreakdown,
    clients: Vec<ClientContribution>,
}

#[derive(Debug, Clone, Serialize)]
struct DateRange {
    start: String,
    end: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct YearSummary {
    year: String,
    total_tokens: i64,
    total_cost: f64,
    range: DateRange,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DataSummary {
    total_tokens: i64,
    total_cost: f64,
    total_days: i32,
    active_days: i32,
    average_per_day: f64,
    max_cost_in_single_day: f64,
    clients: Vec<String>,
    models: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportMeta {
    generated_at: String,
    version: String,
    date_range: DateRange,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenContributionData {
    meta: ExportMeta,
    summary: DataSummary,
    years: Vec<YearSummary>,
    contributions: Vec<DailyContribution>,
}

#[derive(Default)]
struct DayAccumulator {
    totals: DailyTotals,
    token_breakdown: TokenBreakdown,
    clients: HashMap<String, ClientContribution>,
}

pub fn run(year: &str) -> Result<Value, String> {
    let year = normalize_year(year)?;
    let mut messages = collect_messages();
    if let Some(year) = year.as_deref() {
        let prefix = format!("{}-", year);
        messages.retain(|m| m.date.starts_with(&prefix));
    }
    let payload = build_payload(messages);
    serde_json::to_value(payload).map_err(|e| format!("serialize usage graph: {}", e))
}

fn normalize_year(year: &str) -> Result<Option<String>, String> {
    let trimmed = year.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() == 4 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        Ok(Some(trimmed.to_string()))
    } else {
        Err(format!("invalid year filter: {}", year))
    }
}

fn collect_messages() -> Vec<UsageMessage> {
    let mut messages = Vec::new();
    messages.extend(parse_claude());
    messages.extend(parse_codex());
    messages.extend(parse_cursor());
    messages.extend(parse_opencode());
    messages.extend(parse_gemini());
    messages.extend(parse_copilot());
    messages.extend(parse_amp());
    messages.extend(parse_droid());
    messages.extend(parse_hermes());

    dedup_messages(messages)
        .into_iter()
        .filter_map(|mut msg| {
            if msg.timestamp_ms <= 0 || msg.total_tokens() <= 0 {
                return None;
            }
            if msg.provider_id.trim().is_empty() {
                msg.provider_id = infer_provider(&msg.model_id).to_string();
            }
            if msg.cost <= 0.0 {
                msg.cost = estimate_cost(&msg.model_id, &msg.provider_id, &msg.tokens);
            }
            Some(msg)
        })
        .collect()
}

fn dedup_messages(messages: Vec<UsageMessage>) -> Vec<UsageMessage> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(messages.len());
    for msg in messages {
        let key = msg.dedup_key.clone().unwrap_or_else(|| {
            format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                msg.client,
                msg.model_id,
                msg.provider_id,
                msg.timestamp_ms,
                msg.tokens.input,
                msg.tokens.output,
                msg.tokens.cache_read,
                msg.tokens.cache_write
            )
        });
        if seen.insert(key) {
            out.push(msg);
        }
    }
    out
}

fn build_payload(messages: Vec<UsageMessage>) -> TokenContributionData {
    let mut day_map: BTreeMap<String, DayAccumulator> = BTreeMap::new();

    for msg in messages {
        let day = day_map.entry(msg.date.clone()).or_default();
        day.totals.tokens = day.totals.tokens.saturating_add(msg.total_tokens());
        day.totals.cost += msg.cost;
        day.totals.messages = day.totals.messages.saturating_add(msg.messages);
        day.token_breakdown.add(&msg.tokens);

        let key = format!("{}:{}:{}", msg.client, msg.provider_id, msg.model_id);
        let client = day
            .clients
            .entry(key)
            .or_insert_with(|| ClientContribution {
                client: msg.client.clone(),
                model_id: msg.model_id.clone(),
                provider_id: msg.provider_id.clone(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                messages: 0,
            });
        client.tokens.add(&msg.tokens);
        client.cost += msg.cost;
        client.messages = client.messages.saturating_add(msg.messages);
    }

    let mut contributions: Vec<DailyContribution> = day_map
        .into_iter()
        .map(|(date, day)| {
            let mut clients: Vec<ClientContribution> = day.clients.into_values().collect();
            clients.sort_by(|a, b| {
                a.client
                    .cmp(&b.client)
                    .then(a.provider_id.cmp(&b.provider_id))
                    .then(a.model_id.cmp(&b.model_id))
            });
            DailyContribution {
                date,
                totals: day.totals,
                intensity: 0,
                token_breakdown: day.token_breakdown,
                clients,
            }
        })
        .collect();

    calculate_intensities(&mut contributions);
    let summary = calculate_summary(&contributions);
    let years = calculate_years(&contributions);
    let date_range = DateRange {
        start: contributions
            .first()
            .map(|c| c.date.clone())
            .unwrap_or_default(),
        end: contributions
            .last()
            .map(|c| c.date.clone())
            .unwrap_or_default(),
    };

    TokenContributionData {
        meta: ExportMeta {
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            version: VERSION.to_string(),
            date_range,
        },
        summary,
        years,
        contributions,
    }
}

fn calculate_summary(contributions: &[DailyContribution]) -> DataSummary {
    let mut clients = BTreeSet::new();
    let mut models = BTreeSet::new();
    let mut total_tokens = 0;
    let mut total_cost = 0.0;
    let mut max_cost = 0.0f64;

    for c in contributions {
        total_tokens += c.totals.tokens;
        total_cost += c.totals.cost;
        max_cost = max_cost.max(c.totals.cost);
        for client in &c.clients {
            clients.insert(client.client.clone());
            models.insert(client.model_id.clone());
        }
    }

    let active_days = contributions.iter().filter(|c| c.totals.tokens > 0).count() as i32;
    DataSummary {
        total_tokens,
        total_cost,
        total_days: contributions.len() as i32,
        active_days,
        average_per_day: if active_days > 0 {
            total_cost / active_days as f64
        } else {
            0.0
        },
        max_cost_in_single_day: max_cost,
        clients: clients.into_iter().collect(),
        models: models.into_iter().collect(),
    }
}

fn calculate_years(contributions: &[DailyContribution]) -> Vec<YearSummary> {
    #[derive(Default)]
    struct Acc {
        tokens: i64,
        cost: f64,
        start: String,
        end: String,
    }

    let mut by_year: BTreeMap<String, Acc> = BTreeMap::new();
    for c in contributions {
        if c.date.len() < 4 {
            continue;
        }
        let year = c.date[..4].to_string();
        let acc = by_year.entry(year).or_default();
        acc.tokens += c.totals.tokens;
        acc.cost += c.totals.cost;
        if acc.start.is_empty() || c.date < acc.start {
            acc.start = c.date.clone();
        }
        if acc.end.is_empty() || c.date > acc.end {
            acc.end = c.date.clone();
        }
    }

    by_year
        .into_iter()
        .map(|(year, acc)| YearSummary {
            year,
            total_tokens: acc.tokens,
            total_cost: acc.cost,
            range: DateRange {
                start: acc.start,
                end: acc.end,
            },
        })
        .collect()
}

fn calculate_intensities(contributions: &mut [DailyContribution]) {
    let max_cost = contributions
        .iter()
        .map(|c| c.totals.cost)
        .fold(0.0f64, f64::max);
    if max_cost <= 0.0 {
        return;
    }
    for c in contributions {
        let ratio = c.totals.cost / max_cost;
        c.intensity = if ratio >= 0.75 {
            4
        } else if ratio >= 0.5 {
            3
        } else if ratio >= 0.25 {
            2
        } else if ratio > 0.0 {
            1
        } else {
            0
        };
    }
}

fn parse_claude() -> Vec<UsageMessage> {
    let mut out = Vec::new();
    let Some(home) = home_dir() else {
        return out;
    };
    let roots = [
        home.join(".claude").join("projects"),
        home.join(".claude").join("transcripts"),
    ];
    for root in roots {
        let files = collect_files(&root, |p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("jsonl") | Some("json")
            )
        });
        for file in files {
            out.extend(parse_claude_file(&file));
        }
    }
    out
}

fn parse_claude_file(path: &Path) -> Vec<UsageMessage> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let fallback_ts = file_modified_timestamp_ms(path);
    let mut out: Vec<UsageMessage> = Vec::new();
    let mut dedup: HashMap<String, usize> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let Some(model) = string_value(message.get("model")) else {
            continue;
        };
        let tokens = TokenBreakdown {
            input: i64_value(usage.get("input_tokens")).unwrap_or(0).max(0),
            output: i64_value(usage.get("output_tokens")).unwrap_or(0).max(0),
            cache_read: i64_value(usage.get("cache_read_input_tokens"))
                .unwrap_or(0)
                .max(0),
            cache_write: i64_value(usage.get("cache_creation_input_tokens"))
                .unwrap_or(0)
                .max(0),
            reasoning: 0,
        };
        if tokens.total() <= 0 {
            continue;
        }
        let ts = timestamp_ms_from_value(value.get("timestamp")).unwrap_or(fallback_ts);
        let mut msg = UsageMessage::new("claude", model, "anthropic", ts, tokens, 0.0);
        if let (Some(id), Some(req)) = (
            string_value(message.get("id")),
            string_value(value.get("requestId")),
        ) {
            let key = format!("claude:{}:{}", id, req);
            if let Some(index) = dedup.get(&key).copied() {
                let existing = &mut out[index].tokens;
                existing.input = existing.input.max(msg.tokens.input);
                existing.output = existing.output.max(msg.tokens.output);
                existing.cache_read = existing.cache_read.max(msg.tokens.cache_read);
                existing.cache_write = existing.cache_write.max(msg.tokens.cache_write);
                continue;
            }
            dedup.insert(key.clone(), out.len());
            msg.dedup_key = Some(key);
        }
        out.push(msg);
    }
    out
}

fn parse_codex() -> Vec<UsageMessage> {
    let mut out = Vec::new();
    let Some(home) = home_dir() else {
        return out;
    };
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let mut roots = vec![
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ];
    // Keep reading this legacy env var so users with existing headless exports
    // do not lose Codex history after removing the runtime CLI dependency.
    if let Ok(headless) = std::env::var("TOKSCALE_HEADLESS_DIR") {
        roots.push(PathBuf::from(headless).join("codex"));
    }
    for root in roots {
        for file in collect_files(&root, |p| {
            p.extension().and_then(|s| s.to_str()) == Some("jsonl")
        }) {
            out.extend(parse_codex_file(&file));
        }
    }
    out
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CodexTotals {
    input: i64,
    output: i64,
    cached: i64,
    reasoning: i64,
}

impl CodexTotals {
    fn from_usage(value: &Value) -> Self {
        Self {
            input: i64_value(value.get("input_tokens")).unwrap_or(0).max(0),
            output: i64_value(value.get("output_tokens")).unwrap_or(0).max(0),
            cached: i64_value(value.get("cached_input_tokens"))
                .unwrap_or(0)
                .max(i64_value(value.get("cache_read_input_tokens")).unwrap_or(0))
                .max(0),
            reasoning: i64_value(value.get("reasoning_output_tokens"))
                .unwrap_or(0)
                .max(0),
        }
    }

    fn into_tokens(self) -> TokenBreakdown {
        let cache_read = self.cached.min(self.input).max(0);
        TokenBreakdown {
            input: (self.input - cache_read).max(0),
            output: self.output.max(0),
            cache_read,
            cache_write: 0,
            reasoning: self.reasoning.max(0),
        }
    }
}

fn parse_codex_file(path: &Path) -> Vec<UsageMessage> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let fallback_ts = file_modified_timestamp_ms(path);
    let mut current_model: Option<String> = None;
    let mut provider = "openai".to_string();
    let mut out = Vec::new();

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let entry_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload").unwrap_or(&Value::Null);

        if entry_type == "session_meta" {
            if let Some(p) = string_value(payload.get("model_provider")) {
                provider = p;
            }
        }

        if entry_type == "turn_context" {
            if let Some(model) = string_value(
                payload
                    .get("model_info")
                    .and_then(|v| v.get("slug"))
                    .or_else(|| payload.get("model"))
                    .or_else(|| payload.get("model_name")),
            ) {
                if !model.is_empty() {
                    current_model = Some(model);
                }
            }
            continue;
        }

        if entry_type != "event_msg"
            || payload.get("type").and_then(Value::as_str) != Some("token_count")
        {
            continue;
        }
        let Some(info) = payload.get("info") else {
            continue;
        };
        let model = string_value(info.get("model"))
            .or_else(|| string_value(info.get("model_name")))
            .or_else(|| current_model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        if model != "unknown" {
            current_model = Some(model.clone());
        }
        let usage = info
            .get("last_token_usage")
            .or_else(|| info.get("total_token_usage"));
        let Some(usage) = usage else {
            continue;
        };
        let tokens = CodexTotals::from_usage(usage).into_tokens();
        if tokens.total() <= 0 {
            continue;
        }
        let ts = timestamp_ms_from_value(value.get("timestamp")).unwrap_or(fallback_ts);
        let mut msg = UsageMessage::new("codex", model, provider.clone(), ts, tokens, 0.0);
        msg.dedup_key = Some(format!(
            "codex:{}:{}:{}:{}:{}",
            msg.timestamp_ms,
            msg.model_id,
            msg.tokens.input,
            msg.tokens.output,
            msg.tokens.cache_read
        ));
        out.push(msg);
    }
    out
}

fn parse_cursor() -> Vec<UsageMessage> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    // Cursor's usage source is still the local compatibility cache produced by
    // older Tokcat/tokscale setups. Reading it avoids dropping historical data.
    let root = home.join(".config").join("tokscale").join("cursor-cache");
    collect_files(&root, |p| {
        p.file_name().and_then(|s| s.to_str()).is_some_and(|name| {
            name == "usage.csv" || (name.starts_with("usage.") && name.ends_with(".csv"))
        })
    })
    .into_iter()
    .flat_map(|p| parse_cursor_file(&p))
    .collect()
}

fn parse_cursor_file(path: &Path) -> Vec<UsageMessage> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = content.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let header_fields = parse_csv_line(header);
    let has_kind = header_fields.iter().any(|f| f.trim_matches('"') == "Kind");
    let column_count = header_fields.len();
    let (model_idx, input_with_cache_idx, input_no_cache_idx, cache_read_idx, output_idx, cost_idx) =
        if has_kind && column_count >= 12 {
            (4, 6, 7, 8, 9, 11)
        } else if has_kind && column_count >= 10 {
            (2, 4, 5, 6, 7, 9)
        } else {
            (1, 2, 3, 4, 5, 7)
        };

    let account = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("usage")
        .to_string();
    let mut out = Vec::new();
    for line in lines {
        let fields = parse_csv_line(line);
        if fields.len() <= cost_idx {
            continue;
        }
        let date = clean_csv(fields[0]);
        let model = clean_csv(fields[model_idx]);
        if model.is_empty() {
            continue;
        }
        let input_with_cache = clean_csv(fields[input_with_cache_idx])
            .parse::<i64>()
            .unwrap_or(0);
        let input_without_cache = clean_csv(fields[input_no_cache_idx])
            .parse::<i64>()
            .unwrap_or(0);
        let cache_read = clean_csv(fields[cache_read_idx])
            .parse::<i64>()
            .unwrap_or(0);
        let output = clean_csv(fields[output_idx]).parse::<i64>().unwrap_or(0);
        let cost = parse_cost(clean_csv(fields[cost_idx]));
        let ts = parse_date_to_timestamp_ms(clean_csv(fields[0]));
        if ts <= 0 {
            continue;
        }
        let mut msg = UsageMessage::new(
            "cursor",
            model,
            infer_provider(model),
            ts,
            TokenBreakdown {
                input: input_without_cache.max(0),
                output: output.max(0),
                cache_read: cache_read.max(0),
                cache_write: (input_with_cache - input_without_cache).max(0),
                reasoning: 0,
            },
            cost,
        );
        msg.dedup_key = Some(format!("cursor:{}:{}", account, date));
        out.push(msg);
    }
    out
}

fn parse_opencode() -> Vec<UsageMessage> {
    let mut out = Vec::new();
    let Some(home) = home_dir() else {
        return out;
    };
    let xdg = xdg_data_home(&home);
    let data_root = xdg.join("opencode");
    for db_path in discover_opencode_dbs(&data_root) {
        out.extend(parse_opencode_sqlite(&db_path));
    }
    let legacy = data_root.join("storage").join("message");
    for file in collect_files(&legacy, |p| {
        p.extension().and_then(|s| s.to_str()) == Some("json")
    }) {
        if let Some(msg) = parse_opencode_json_file(&file) {
            out.push(msg);
        }
    }
    out
}

fn discover_opencode_dbs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.is_file()
                && (name == "opencode.db"
                    || (name.starts_with("opencode-") && name.ends_with(".db")))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn parse_opencode_sqlite(path: &Path) -> Vec<UsageMessage> {
    let conn = match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let query_with_session = r#"
        SELECT m.id, m.session_id, m.data
        FROM message m
        LEFT JOIN session s ON s.id = m.session_id
        ORDER BY m.id, m.session_id
    "#;
    let query_legacy = "SELECT id, session_id, data FROM message ORDER BY id, session_id";

    let mut stmt = match conn
        .prepare(query_with_session)
        .or_else(|_| conn.prepare(query_legacy))
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, String>(2)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for row in rows.flatten() {
        if let Some(mut msg) = parse_opencode_json(&row.2) {
            if msg.dedup_key.is_none() {
                msg.dedup_key = Some(row.0);
            }
            if row.1.len() > 0 && msg.dedup_key.as_deref() == Some("unknown") {
                msg.dedup_key = Some(row.1);
            }
            out.push(msg);
        }
    }
    out
}

fn parse_opencode_json_file(path: &Path) -> Option<UsageMessage> {
    let data = fs::read_to_string(path).ok()?;
    let mut msg = parse_opencode_json(&data)?;
    if msg.dedup_key.is_none() {
        msg.dedup_key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string);
    }
    Some(msg)
}

fn parse_opencode_json(data: &str) -> Option<UsageMessage> {
    let value: Value = serde_json::from_str(data).ok()?;
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let tokens = value.get("tokens")?;
    let cache = tokens.get("cache").unwrap_or(&Value::Null);
    let model = string_value(value.get("modelID")).or_else(|| string_value(value.get("model")))?;
    let provider =
        string_value(value.get("providerID")).unwrap_or_else(|| infer_provider(&model).to_string());
    let ts = value
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(|v| timestamp_ms_from_value(Some(v)))
        .or_else(|| timestamp_ms_from_value(value.get("created")))
        .unwrap_or_else(now_ms);
    let mut msg = UsageMessage::new(
        "opencode",
        model,
        provider,
        ts,
        TokenBreakdown {
            input: i64_value(tokens.get("input")).unwrap_or(0).max(0),
            output: i64_value(tokens.get("output")).unwrap_or(0).max(0),
            cache_read: i64_value(cache.get("read")).unwrap_or(0).max(0),
            cache_write: i64_value(cache.get("write")).unwrap_or(0).max(0),
            reasoning: i64_value(tokens.get("reasoning")).unwrap_or(0).max(0),
        },
        f64_value(value.get("cost")).unwrap_or(0.0),
    );
    msg.dedup_key = string_value(value.get("id")).map(|id| format!("opencode:{}", id));
    Some(msg)
}

fn parse_gemini() -> Vec<UsageMessage> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    collect_files(&home.join(".gemini").join("tmp"), |p| {
        matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("json") | Some("jsonl")
        )
    })
    .into_iter()
    .flat_map(|p| parse_gemini_file(&p))
    .collect()
}

fn parse_gemini_file(path: &Path) -> Vec<UsageMessage> {
    let fallback_ts = file_modified_timestamp_ms(path);
    if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
        return parse_gemini_jsonl(path, fallback_ts);
    }
    let Ok(data) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&data) else {
        return Vec::new();
    };
    parse_gemini_value(&value, fallback_ts)
}

fn parse_gemini_jsonl(path: &Path, fallback_ts: i64) -> Vec<UsageMessage> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut current_model: Option<String> = None;
    let mut session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("gemini")
        .to_string();
    let mut direct_by_id: HashMap<String, usize> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if let Some(id) = string_value(value.get("session_id").or_else(|| value.get("sessionId"))) {
            session_id = id;
        }
        if let Some(model) = string_value(value.get("model")) {
            current_model = Some(model);
        }
        if value.get("type").and_then(Value::as_str) == Some("gemini") {
            if let Some(mut msg) = build_gemini_message(&value, current_model.clone(), fallback_ts)
            {
                msg.dedup_key = string_value(value.get("id")).map(|id| format!("gemini:{}", id));
                if let Some(key) = msg.dedup_key.clone() {
                    if let Some(idx) = direct_by_id.get(&key).copied() {
                        out[idx] = msg;
                    } else {
                        direct_by_id.insert(key, out.len());
                        out.push(msg);
                    }
                } else {
                    msg.dedup_key = Some(format!("gemini:{}:{}", session_id, out.len()));
                    out.push(msg);
                }
            }
            continue;
        }
        if let Some(stats) = value
            .get("stats")
            .or_else(|| value.get("result").and_then(|r| r.get("stats")))
        {
            out.extend(build_gemini_stats_messages(
                stats,
                current_model.clone(),
                fallback_ts,
            ));
        }
    }
    out
}

fn parse_gemini_value(value: &Value, fallback_ts: i64) -> Vec<UsageMessage> {
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        return messages
            .iter()
            .filter(|m| m.get("type").and_then(Value::as_str) == Some("gemini"))
            .filter_map(|m| build_gemini_message(m, None, fallback_ts))
            .collect();
    }
    if let Some(message) = build_gemini_message(value, None, fallback_ts) {
        return vec![message];
    }
    if let Some(stats) = value
        .get("stats")
        .or_else(|| value.get("result").and_then(|r| r.get("stats")))
    {
        return build_gemini_stats_messages(stats, string_value(value.get("model")), fallback_ts);
    }
    Vec::new()
}

fn build_gemini_message(
    value: &Value,
    model_hint: Option<String>,
    fallback_ts: i64,
) -> Option<UsageMessage> {
    let tokens = value.get("tokens")?;
    let model = string_value(value.get("model")).or(model_hint)?;
    let output = i64_value(tokens.get("output")).unwrap_or(0).max(0);
    let reasoning = i64_value(tokens.get("thoughts")).unwrap_or(0).max(0);
    let tool = i64_value(tokens.get("tool")).unwrap_or(0).max(0);
    let cache_read = i64_value(tokens.get("cached")).unwrap_or(0).max(0);
    let input_raw = i64_value(tokens.get("input")).unwrap_or(0).max(0);
    let total = i64_value(tokens.get("total"));
    let inclusive_total = input_raw + output + reasoning + tool;
    let input = if cache_read > 0 && total == Some(inclusive_total) {
        input_raw.saturating_sub(cache_read)
    } else {
        input_raw
    };
    let ts = timestamp_ms_from_value(value.get("timestamp").or_else(|| value.get("created_at")))
        .unwrap_or(fallback_ts);
    Some(UsageMessage::new(
        "gemini",
        model,
        "google",
        ts,
        TokenBreakdown {
            input: input + tool,
            output,
            cache_read,
            cache_write: 0,
            reasoning,
        },
        0.0,
    ))
}

fn build_gemini_stats_messages(
    stats: &Value,
    model_hint: Option<String>,
    fallback_ts: i64,
) -> Vec<UsageMessage> {
    let mut out = Vec::new();
    if let Some(models) = stats.get("models").and_then(Value::as_object) {
        for (model, data) in models {
            if let Some(msg) = build_gemini_stats_message(model, data, fallback_ts) {
                out.push(msg);
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    if let Some(model) = model_hint {
        if let Some(msg) = build_gemini_stats_message(&model, stats, fallback_ts) {
            out.push(msg);
        }
    }
    out
}

fn build_gemini_stats_message(
    model: &str,
    value: &Value,
    fallback_ts: i64,
) -> Option<UsageMessage> {
    let tokens = value.get("tokens").unwrap_or(value);
    let input_raw = i64_value(tokens.get("prompt"))
        .or_else(|| i64_value(tokens.get("input_tokens")))
        .or_else(|| i64_value(tokens.get("prompt_tokens")))
        .or_else(|| i64_value(tokens.get("input")))
        .unwrap_or(0);
    let output = i64_value(tokens.get("candidates"))
        .or_else(|| i64_value(tokens.get("output")))
        .or_else(|| i64_value(tokens.get("output_tokens")))
        .unwrap_or(0);
    let cache_read = i64_value(tokens.get("cached"))
        .or_else(|| i64_value(tokens.get("cached_tokens")))
        .unwrap_or(0);
    let reasoning = i64_value(tokens.get("thoughts"))
        .or_else(|| i64_value(tokens.get("thoughts_tokens")))
        .or_else(|| i64_value(tokens.get("reasoning")))
        .or_else(|| i64_value(tokens.get("reasoning_tokens")))
        .unwrap_or(0);
    if input_raw == 0 && output == 0 && cache_read == 0 && reasoning == 0 {
        return None;
    }
    Some(UsageMessage::new(
        "gemini",
        model.to_string(),
        "google",
        fallback_ts,
        TokenBreakdown {
            input: input_raw.saturating_sub(cache_read).max(0),
            output: output.max(0),
            cache_read: cache_read.max(0),
            cache_write: 0,
            reasoning: reasoning.max(0),
        },
        0.0,
    ))
}

fn parse_copilot() -> Vec<UsageMessage> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut files = collect_files(&home.join(".copilot").join("otel"), |p| {
        p.extension().and_then(|s| s.to_str()) == Some("jsonl")
    });
    if let Ok(path) = std::env::var("COPILOT_OTEL_FILE_EXPORTER_PATH") {
        let p = PathBuf::from(path);
        if p.is_file() {
            files.push(p);
        }
    }
    files
        .into_iter()
        .flat_map(|p| parse_copilot_file(&p))
        .collect()
}

fn parse_copilot_file(path: &Path) -> Vec<UsageMessage> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let fallback_ts = file_modified_timestamp_ms(path);
    let mut out = Vec::new();
    for (index, line) in BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .enumerate()
    {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(attrs) = value.get("attributes") else {
            continue;
        };
        let input = attr_i64(attrs, &["gen_ai.usage.input_tokens"]);
        let output = attr_i64(attrs, &["gen_ai.usage.output_tokens"]);
        let cache_read = attr_i64(attrs, &["gen_ai.usage.cache_read.input_tokens"]);
        let cache_write = attr_i64(
            attrs,
            &[
                "gen_ai.usage.cache_write.input_tokens",
                "gen_ai.usage.cache_creation.input_tokens",
            ],
        );
        let reasoning = attr_i64(
            attrs,
            &[
                "gen_ai.usage.reasoning.output_tokens",
                "gen_ai.usage.reasoning_tokens",
            ],
        );
        let cache_for_input = cache_read.max(0).min(input.max(0));
        let tokens = TokenBreakdown {
            input: input.saturating_sub(cache_for_input).max(0),
            output: output.max(0),
            cache_read: cache_read.max(0),
            cache_write: cache_write.max(0),
            reasoning: reasoning.max(0),
        };
        if tokens.total() <= 0 {
            continue;
        }
        let model = attr_string(attrs, &["gen_ai.response.model", "gen_ai.request.model"])
            .unwrap_or_else(|| "unknown".to_string());
        let session = attr_string(
            attrs,
            &[
                "gen_ai.conversation.id",
                "copilot_chat.session_id",
                "gen_ai.response.id",
                "session.id",
            ],
        )
        .unwrap_or_else(|| "unknown-session".to_string());
        let ts = copilot_timestamp_ms(&value).unwrap_or(fallback_ts);
        let provider = infer_provider(&model).to_string();
        let mut msg = UsageMessage::new("copilot", model, provider, ts, tokens, 0.0);
        let trace = string_value(value.get("traceId")).or_else(|| {
            value
                .get("spanContext")
                .and_then(|s| string_value(s.get("traceId")))
        });
        let span = string_value(value.get("spanId")).or_else(|| {
            value
                .get("spanContext")
                .and_then(|s| string_value(s.get("spanId")))
        });
        msg.dedup_key = Some(match (trace, span) {
            (Some(t), Some(s)) => format!("copilot:{}:{}", t, s),
            _ => format!("copilot:{}:{}:{}", session, ts, index),
        });
        out.push(msg);
    }
    out
}

fn parse_amp() -> Vec<UsageMessage> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let root = xdg_data_home(&home).join("amp").join("threads");
    collect_files(&root, |p| {
        p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.starts_with("T-") && name.ends_with(".json"))
    })
    .into_iter()
    .flat_map(|p| parse_amp_file(&p))
    .collect()
}

fn parse_amp_file(path: &Path) -> Vec<UsageMessage> {
    let Ok(data) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&data) else {
        return Vec::new();
    };
    let fallback_ts = file_modified_timestamp_ms(path);
    let thread_created = i64_value(value.get("created")).unwrap_or(fallback_ts);
    let thread_id = string_value(value.get("id")).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("amp")
            .to_string()
    });
    if let Some(events) = value
        .get("usageLedger")
        .and_then(|l| l.get("events"))
        .and_then(Value::as_array)
    {
        let mut out = Vec::new();
        for (index, event) in events.iter().enumerate() {
            let Some(model) = string_value(event.get("model")) else {
                continue;
            };
            let provider = infer_provider(&model).to_string();
            let tokens_value = event.get("tokens").unwrap_or(&Value::Null);
            let ts = timestamp_ms_from_value(event.get("timestamp")).unwrap_or(thread_created);
            let mut msg = UsageMessage::new(
                "amp",
                model,
                provider,
                ts,
                TokenBreakdown {
                    input: i64_value(tokens_value.get("input")).unwrap_or(0).max(0),
                    output: i64_value(tokens_value.get("output")).unwrap_or(0).max(0),
                    cache_read: i64_value(tokens_value.get("cacheReadInputTokens"))
                        .unwrap_or(0)
                        .max(0),
                    cache_write: i64_value(tokens_value.get("cacheCreationInputTokens"))
                        .unwrap_or(0)
                        .max(0),
                    reasoning: 0,
                },
                f64_value(event.get("credits")).unwrap_or(0.0),
            );
            msg.dedup_key = Some(format!("amp:{}:ledger:{}", thread_id, index));
            out.push(msg);
        }
        if !out.is_empty() {
            return out;
        }
    }
    value
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
                .filter_map(|m| {
                    let usage = m.get("usage")?;
                    let model = string_value(usage.get("model"))?;
                    let provider = infer_provider(&model).to_string();
                    let message_id = i64_value(m.get("messageId")).unwrap_or(0);
                    let mut msg = UsageMessage::new(
                        "amp",
                        model,
                        provider,
                        thread_created.saturating_add(message_id.saturating_mul(1000)),
                        TokenBreakdown {
                            input: i64_value(usage.get("inputTokens")).unwrap_or(0).max(0),
                            output: i64_value(usage.get("outputTokens")).unwrap_or(0).max(0),
                            cache_read: i64_value(usage.get("cacheReadInputTokens"))
                                .unwrap_or(0)
                                .max(0),
                            cache_write: i64_value(usage.get("cacheCreationInputTokens"))
                                .unwrap_or(0)
                                .max(0),
                            reasoning: 0,
                        },
                        f64_value(usage.get("credits")).unwrap_or(0.0),
                    );
                    msg.dedup_key = Some(format!("amp:{}:message:{}", thread_id, message_id));
                    Some(msg)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_droid() -> Vec<UsageMessage> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    collect_files(&home.join(".factory").join("sessions"), |p| {
        p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.ends_with(".settings.json"))
    })
    .into_iter()
    .filter_map(|p| parse_droid_file(&p))
    .collect()
}

fn parse_droid_file(path: &Path) -> Option<UsageMessage> {
    let data = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&data).ok()?;
    let usage = value.get("tokenUsage")?;
    let provider = string_value(value.get("providerLock")).unwrap_or_else(|| {
        string_value(value.get("model"))
            .map(|m| infer_provider(&m).to_string())
            .unwrap_or_else(|| "unknown".to_string())
    });
    let model = string_value(value.get("model"))
        .map(|m| normalize_droid_model(&m))
        .unwrap_or_else(|| default_model_for_provider(&provider));
    let ts = timestamp_ms_from_value(value.get("providerLockTimestamp"))
        .unwrap_or_else(|| file_modified_timestamp_ms(path));
    let mut msg = UsageMessage::new(
        "droid",
        model,
        provider,
        ts,
        TokenBreakdown {
            input: i64_value(usage.get("inputTokens")).unwrap_or(0).max(0),
            output: i64_value(usage.get("outputTokens")).unwrap_or(0).max(0),
            cache_read: i64_value(usage.get("cacheReadTokens")).unwrap_or(0).max(0),
            cache_write: i64_value(usage.get("cacheCreationTokens"))
                .unwrap_or(0)
                .max(0),
            reasoning: i64_value(usage.get("thinkingTokens")).unwrap_or(0).max(0),
        },
        0.0,
    );
    msg.dedup_key = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| format!("droid:{}", s));
    Some(msg)
}

fn parse_hermes() -> Vec<UsageMessage> {
    let Some(path) = hermes_db_path() else {
        return Vec::new();
    };
    let conn = match Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn.prepare(
        r#"
        SELECT id, model,
            COALESCE(input_tokens, 0),
            COALESCE(output_tokens, 0),
            COALESCE(cache_read_tokens, 0),
            COALESCE(cache_write_tokens, 0)
        FROM sessions
        WHERE model IS NOT NULL AND TRIM(model) != ''
        "#,
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let ts = file_modified_timestamp_ms(&path);
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (id, model, input, output, cache_read, cache_write) = row;
        let provider = infer_provider(&model).to_string();
        let mut msg = UsageMessage::new(
            "hermes",
            model,
            provider,
            ts,
            TokenBreakdown {
                input: input.max(0),
                output: output.max(0),
                cache_read: cache_read.max(0),
                cache_write: cache_write.max(0),
                reasoning: 0,
            },
            0.0,
        );
        msg.dedup_key = Some(format!("hermes:{}", id));
        out.push(msg);
    }
    out
}

fn collect_files(root: &Path, pred: impl Fn(&Path) -> bool + Copy) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files_inner(root, pred, &mut out);
    out.sort();
    out
}

fn collect_files_inner(root: &Path, pred: impl Fn(&Path) -> bool + Copy, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_files_inner(&path, pred, out);
        } else if file_type.is_file() && pred(&path) {
            out.push(path);
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn xdg_data_home(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"))
}

fn hermes_db_path() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HERMES_HOME") {
        let p = PathBuf::from(home).join("state.db");
        if p.is_file() {
            return Some(p);
        }
    }
    let p = home_dir()?.join(".hermes").join("state.db");
    p.is_file().then_some(p)
}

fn file_modified_timestamp_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(now_ms)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn date_from_timestamp_ms(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .or_else(|| Local.timestamp_millis_opt(timestamp_ms).earliest())
        .unwrap_or_else(Local::now)
        .format("%Y-%m-%d")
        .to_string()
}

fn timestamp_ms_from_value(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(s) = value.as_str() {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(dt.timestamp_millis());
        }
        return s.parse::<i64>().ok().map(normalize_epoch_ms);
    }
    if let Some(i) = value.as_i64() {
        return Some(normalize_epoch_ms(i));
    }
    if let Some(f) = value.as_f64() {
        if f.is_finite() {
            return Some(normalize_epoch_ms(f as i64));
        }
    }
    None
}

fn normalize_epoch_ms(raw: i64) -> i64 {
    match raw.abs() {
        100_000_000_000_000_000.. => raw / 1_000_000,
        100_000_000_000_000.. => raw / 1_000,
        100_000_000_000.. => raw,
        _ => raw.saturating_mul(1000),
    }
}

fn parse_date_to_timestamp_ms(date: &str) -> i64 {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(date) {
        return dt.timestamp_millis();
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        if let Some(dt) = date.and_hms_opt(12, 0, 0) {
            return Utc.from_utc_datetime(&dt).timestamp_millis();
        }
    }
    0
}

fn string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn i64_value(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|v| v as i64))
            .or_else(|| n.as_f64().map(|v| v as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn f64_value(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn attr_i64(attrs: &Value, names: &[&str]) -> i64 {
    names
        .iter()
        .find_map(|name| i64_value(attrs.get(*name)))
        .unwrap_or(0)
}

fn attr_string(attrs: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| string_value(attrs.get(*name)))
}

fn copilot_timestamp_ms(value: &Value) -> Option<i64> {
    for key in ["endTime", "startTime", "hrTime", "_hrTime", "time"] {
        if let Some(parts) = value.get(key).and_then(Value::as_array) {
            let sec = parts.first().and_then(|v| i64_value(Some(v)))?;
            let nanos = parts.get(1).and_then(|v| i64_value(Some(v))).unwrap_or(0);
            return Some(sec.saturating_mul(1000) + nanos / 1_000_000);
        }
    }
    timestamp_ms_from_value(value.get("timestamp"))
        .or_else(|| timestamp_ms_from_value(value.get("observedTimestamp")))
        .or_else(|| {
            i64_value(value.get("timeUnixNano")).and_then(|n| (n > 0).then_some(n / 1_000_000))
        })
}

fn parse_csv_line(line: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    for (i, c) in chars {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == ',' && !in_quotes {
            fields.push(&line[start..i]);
            start = i + 1;
        }
    }
    fields.push(&line[start..]);
    fields
}

fn clean_csv(value: &str) -> &str {
    value.trim().trim_matches('"')
}

fn parse_cost(value: &str) -> f64 {
    let cleaned = value.replace(['$', ','], "");
    let trimmed = cleaned.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("nan")
        || trimmed.eq_ignore_ascii_case("included")
        || trimmed == "-"
    {
        0.0
    } else {
        trimmed.parse().unwrap_or(0.0)
    }
}

fn normalize_model_id(id: &str) -> String {
    id.trim().to_lowercase()
}

fn normalize_droid_model(model: &str) -> String {
    let without_prefix = model.strip_prefix("custom:").unwrap_or(model);
    let mut out = String::with_capacity(without_prefix.len());
    let mut in_bracket = false;
    for c in without_prefix.chars() {
        match c {
            '[' => in_bracket = true,
            ']' => in_bracket = false,
            _ if in_bracket => {}
            '.' | '_' | ' ' => out.push('-'),
            c => out.push(c.to_ascii_lowercase()),
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

fn infer_provider(model: &str) -> &'static str {
    let model = model.to_lowercase();
    if model.contains("claude")
        || model.contains("sonnet")
        || model.contains("opus")
        || model.contains("haiku")
    {
        "anthropic"
    } else if model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
    {
        "openai"
    } else if model.contains("gemini") {
        "google"
    } else if model.contains("grok") {
        "xai"
    } else if model.contains("deepseek") {
        "deepseek"
    } else if model.contains("llama") {
        "meta"
    } else {
        "unknown"
    }
}

fn default_model_for_provider(provider: &str) -> String {
    match provider {
        "anthropic" => "claude-unknown",
        "openai" => "gpt-unknown",
        "google" => "gemini-unknown",
        "xai" => "grok-unknown",
        _ => "unknown",
    }
    .to_string()
}

#[derive(Clone, Copy)]
struct Price {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

fn estimate_cost(model: &str, provider: &str, tokens: &TokenBreakdown) -> f64 {
    let price = bundled_price(model, provider);
    let input = tokens.input as f64 * price.input;
    let output = (tokens.output + tokens.reasoning) as f64 * price.output;
    let cache_read = tokens.cache_read as f64 * price.cache_read;
    let cache_write = tokens.cache_write as f64 * price.cache_write;
    (input + output + cache_read + cache_write) / 1_000_000.0
}

fn bundled_price(model: &str, provider: &str) -> Price {
    let m = model.to_lowercase();
    if m.contains("opus") {
        return Price {
            input: 15.0,
            output: 75.0,
            cache_read: 1.5,
            cache_write: 18.75,
        };
    }
    if m.contains("sonnet") || m.contains("claude") {
        return Price {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        };
    }
    if m.contains("haiku") {
        return Price {
            input: 0.8,
            output: 4.0,
            cache_read: 0.08,
            cache_write: 1.0,
        };
    }
    if m.contains("gpt-4o") {
        return Price {
            input: 2.5,
            output: 10.0,
            cache_read: 1.25,
            cache_write: 2.5,
        };
    }
    if m.starts_with("gpt-5") || m.contains("codex") {
        return Price {
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 1.25,
        };
    }
    if m.starts_with("o3") || m.starts_with("o4") {
        return Price {
            input: 10.0,
            output: 40.0,
            cache_read: 2.5,
            cache_write: 10.0,
        };
    }
    if m.contains("gemini") && m.contains("flash") {
        return Price {
            input: 0.3,
            output: 2.5,
            cache_read: 0.075,
            cache_write: 0.3,
        };
    }
    if m.contains("gemini") {
        return Price {
            input: 1.25,
            output: 10.0,
            cache_read: 0.3125,
            cache_write: 1.25,
        };
    }
    match provider {
        "anthropic" => Price {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        },
        "openai" => Price {
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 1.25,
        },
        "google" => Price {
            input: 1.25,
            output: 10.0,
            cache_read: 0.3125,
            cache_write: 1.25,
        },
        _ => Price {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_cached_tokens_do_not_double_count_input() {
        let tokens = CodexTotals {
            input: 100,
            output: 20,
            cached: 30,
            reasoning: 5,
        }
        .into_tokens();

        assert_eq!(tokens.input, 70);
        assert_eq!(tokens.cache_read, 30);
        assert_eq!(tokens.total(), 125);
    }

    #[test]
    fn payload_shape_uses_camel_case_fields() {
        let payload = build_payload(vec![UsageMessage::new(
            "claude",
            "claude-sonnet-4",
            "anthropic",
            1_700_000_000_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 2,
                cache_write: 1,
                reasoning: 0,
            },
            0.01,
        )]);

        let value = serde_json::to_value(payload).unwrap();
        assert!(value.pointer("/meta/dateRange/start").is_some());
        assert!(value
            .pointer("/contributions/0/tokenBreakdown/cacheRead")
            .is_some());
        assert!(value
            .pointer("/contributions/0/clients/0/modelId")
            .is_some());
        assert!(value.pointer("/years/0/range/start").is_some());
    }
}
