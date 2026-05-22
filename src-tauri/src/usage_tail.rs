// Incremental tailer for Claude Code (and future CLIs) usage JSONL.
//
// Replaces the 3-minute `tokscale graph` subprocess polling cycle as the
// source of truth for the animation/rate signal. Patterned on tokscale's
// own SourceMessageCache strategy — track (file, offset, mtime), re-read
// only growth, dedup per (messageId, requestId) using per-field max.
//
// Phase 1: Claude Code only. Codex/Gemini/etc parsers can be added under
// the same UsageEvent + bucketing pipeline.

use chrono::DateTime;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CLIENT_CLAUDE: &str = "claude-code";
const EVENT_WINDOW_SECS: i64 = 3600; // keep 1h history for rate + trace
const COLD_SCAN_LOOKBACK_SECS: i64 = 6 * 3600; // first-tick: only parse files mtime'd within 6h

#[derive(Debug, Clone, Serialize)]
pub struct UsageEvent {
    pub ts_ms: i64,
    pub client: String,
    pub agent: String,
    pub model: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl UsageEvent {
    fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceBucket {
    pub client: String,
    pub agent: String,
    pub model: String,
    pub tokens: i64,
    pub messages: u32,
    pub tokens_per_min: f32,
}

#[derive(Debug, Default)]
struct FileState {
    offset: u64,
    mtime_ms: i64,
}

pub struct UsageTailer {
    files: Mutex<HashMap<PathBuf, FileState>>,
    events: Mutex<VecDeque<UsageEvent>>,
    // dedup: `<messageId>:<requestId>` → index in `events` (first occurrence).
    // Streaming writes the same key multiple times with growing token counts;
    // we keep per-field max to converge on the final values.
    seen: Mutex<HashMap<String, usize>>,
    cold: Mutex<bool>,
}

impl UsageTailer {
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            seen: Mutex::new(HashMap::new()),
            cold: Mutex::new(true),
        }
    }

    /// Walk Claude Code roots, parse growth since last tick, push events.
    /// Returns the number of new events added.
    pub fn tick(&self) -> usize {
        let mut added = 0;
        let is_cold = {
            let mut c = self.cold.lock();
            let was = *c;
            *c = false;
            was
        };
        let cold_cutoff_ms = now_ms() - COLD_SCAN_LOOKBACK_SECS * 1000;

        for root in claude_roots() {
            let mut stack: Vec<PathBuf> = vec![root];
            while let Some(dir) = stack.pop() {
                let entries = match fs::read_dir(&dir) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let ft = match entry.file_type() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    if ft.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let size = meta.len();
                    let mtime_ms = system_time_to_ms(meta.modified().ok());
                    let prev = self.files.lock().get(&path).map(|f| (f.offset, f.mtime_ms));

                    // Cold-start optimization: stale files are stamped at EOF
                    // without parsing so subsequent ticks only see fresh growth.
                    if is_cold && prev.is_none() && mtime_ms < cold_cutoff_ms {
                        self.files.lock().insert(
                            path.clone(),
                            FileState {
                                offset: size,
                                mtime_ms,
                            },
                        );
                        continue;
                    }

                    let (start_offset, _prev_mtime) = prev.unwrap_or((0, 0));
                    if size == start_offset {
                        // No growth — refresh mtime so subsequent stat calls
                        // don't keep re-evaluating the same condition.
                        self.files.lock().insert(
                            path.clone(),
                            FileState {
                                offset: size,
                                mtime_ms,
                            },
                        );
                        continue;
                    }
                    if size < start_offset {
                        // File truncated/rotated — reparse from start.
                        added += self.read_growth(&path, 0, size, mtime_ms);
                    } else {
                        added += self.read_growth(&path, start_offset, size, mtime_ms);
                    }
                }
            }
        }

        self.trim_events();
        added
    }

    fn read_growth(&self, path: &Path, start: u64, end: u64, mtime_ms: i64) -> usize {
        let mut added = 0;
        let mut file = match fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return 0,
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return 0;
        }
        let to_read = end.saturating_sub(start) as usize;
        let mut buf = Vec::with_capacity(to_read.min(8 * 1024 * 1024));
        if file.take(to_read as u64).read_to_end(&mut buf).is_err() {
            return 0;
        }

        // Last partial line (no trailing \n yet) is left unconsumed.
        let mut consumed: u64 = 0;
        for line in buf.split(|&b| b == b'\n') {
            // Lines without a trailing \n surface here as the last element;
            // we detect this by checking we've exhausted the buffer.
            consumed += line.len() as u64;
            let has_terminator = consumed < buf.len() as u64;
            if !has_terminator {
                // Partial line — don't consume.
                break;
            }
            consumed += 1; // account for the \n
            let trimmed = trim_ascii(line);
            if trimmed.is_empty() {
                continue;
            }
            if self.parse_and_record(path, trimmed) {
                added += 1;
            }
        }

        let new_offset = start + consumed;
        self.files.lock().insert(
            path.to_path_buf(),
            FileState {
                offset: new_offset,
                mtime_ms,
            },
        );
        added
    }

    fn parse_and_record(&self, path: &Path, raw: &[u8]) -> bool {
        let value: serde_json::Value = match serde_json::from_slice(raw) {
            Ok(v) => v,
            Err(_) => return false,
        };

        let entry_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if entry_type != "assistant" && entry_type != "user" {
            // Only assistant entries (and sometimes synthetic user-side billing)
            // carry usage. Filter early to avoid wasted lookups.
            // In practice Claude Code only emits usage on assistant entries.
            if entry_type != "assistant" {
                return false;
            }
        }

        let message = match value.get("message") {
            Some(m) => m,
            None => return false,
        };
        let usage = match message.get("usage") {
            Some(u) => u,
            None => return false,
        };

        let input = usage
            .get("input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cache_write = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        if input + output + cache_read + cache_write <= 0 {
            return false;
        }

        let model = message
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let model = normalize_model(&model);

        let ts_ms = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339_ms)
            .unwrap_or_else(now_ms);

        let is_sidechain = value
            .get("isSidechain")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let agent_id = value.get("agentId").and_then(|v| v.as_str());

        let agent = if is_sidechain {
            // Subagent_type resolution from the parent transcript is left to
            // a later phase; surface agent_id (or path-derived id) so the
            // trace UI still gets a stable per-subagent bucket.
            match agent_id {
                Some(id) if !id.is_empty() => format!("subagent:{}", id),
                _ => sidechain_label_from_path(path),
            }
        } else {
            "main".to_string()
        };

        let msg_id = message.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let req_id = value
            .get("requestId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let dedup_key = if !msg_id.is_empty() {
            format!("{}:{}", msg_id, req_id)
        } else {
            String::new()
        };

        let mut events = self.events.lock();
        let mut seen = self.seen.lock();
        if !dedup_key.is_empty() {
            if let Some(&idx) = seen.get(&dedup_key) {
                if let Some(existing) = events.get_mut(idx) {
                    existing.input = existing.input.max(input);
                    existing.output = existing.output.max(output);
                    existing.cache_read = existing.cache_read.max(cache_read);
                    existing.cache_write = existing.cache_write.max(cache_write);
                    return false;
                }
            }
        }

        let event = UsageEvent {
            ts_ms,
            client: CLIENT_CLAUDE.to_string(),
            agent,
            model,
            input,
            output,
            cache_read,
            cache_write,
        };
        let idx = events.len();
        events.push_back(event);
        if !dedup_key.is_empty() {
            seen.insert(dedup_key, idx);
        }
        true
    }

    fn trim_events(&self) {
        let cutoff = now_ms() - EVENT_WINDOW_SECS * 1000;
        let mut events = self.events.lock();
        let mut seen = self.seen.lock();
        let before = events.len();
        events.retain(|e| e.ts_ms >= cutoff);
        if events.len() == before {
            return;
        }
        // Indices into `events` shifted; rebuild empty. Streaming repeats
        // that bridge a trim boundary may record a second event, which is
        // acceptable since both share the same ts and either is in or out
        // of the rate windows.
        seen.clear();
    }

    /// Tokens emitted in the last 60s. Used by the animation signal.
    pub fn rate_per_min(&self) -> f32 {
        self.window_total(60) as f32
    }

    /// Average tokens-per-minute over the given window. window_secs == 60
    /// returns the same value as rate_per_min(); longer windows smooth out
    /// bursts so the tray title shows a stable number during quiet periods.
    pub fn rate_in_window(&self, window_secs: i64) -> f32 {
        if window_secs <= 0 {
            return 0.0;
        }
        let total = self.window_total(window_secs) as f32;
        let window_min = window_secs as f32 / 60.0;
        total / window_min
    }

    fn window_total(&self, secs: i64) -> i64 {
        let cutoff = now_ms() - secs * 1000;
        let events = self.events.lock();
        // Events aren't strictly ordered by ts_ms (we push in file-walk
        // order, not by timestamp), so iterate all and filter — early-break
        // would silently drop bursts past an out-of-order older entry.
        events
            .iter()
            .filter(|e| e.ts_ms >= cutoff)
            .map(|e| e.total())
            .sum()
    }

    /// Per-(client, agent, model) breakdown of the last `window_secs` of
    /// activity. Used by the trace UI.
    pub fn trace(&self, window_secs: i64) -> Vec<TraceBucket> {
        let cutoff = now_ms() - window_secs * 1000;
        let events = self.events.lock();
        let mut groups: HashMap<(String, String, String), (i64, u32)> = HashMap::new();
        for e in events.iter() {
            if e.ts_ms < cutoff {
                continue;
            }
            let key = (e.client.clone(), e.agent.clone(), e.model.clone());
            let slot = groups.entry(key).or_insert((0, 0));
            slot.0 += e.total();
            slot.1 += 1;
        }
        let window_min = (window_secs as f32 / 60.0).max(1.0 / 60.0);
        let mut out: Vec<TraceBucket> = groups
            .into_iter()
            .map(|((client, agent, model), (tokens, messages))| TraceBucket {
                client,
                agent,
                model,
                tokens,
                messages,
                tokens_per_min: tokens as f32 / window_min,
            })
            .collect();
        out.sort_by(|a, b| b.tokens.cmp(&a.tokens));
        out
    }
}

fn claude_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(&home).join(".claude").join("projects");
        if p.is_dir() {
            out.push(p);
        }
    }
    out
}

fn system_time_to_ms(t: Option<SystemTime>) -> i64 {
    match t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => d.as_millis() as i64,
        None => 0,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&b| !b.is_ascii_whitespace()).unwrap_or(s.len());
    let end = s.iter().rposition(|&b| !b.is_ascii_whitespace()).map(|i| i + 1).unwrap_or(0);
    if start >= end {
        &[]
    } else {
        &s[start..end]
    }
}

fn normalize_model(id: &str) -> String {
    let mut name = id.to_lowercase();
    if name.len() > 9 {
        let tail = &name[name.len() - 8..];
        if tail.chars().all(|c| c.is_ascii_digit()) && name.as_bytes()[name.len() - 9] == b'-' {
            name.truncate(name.len() - 9);
        }
    }
    name
}

fn sidechain_label_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("subagent");
    if let Some(rest) = stem.strip_prefix("agent-") {
        format!("subagent:{}", rest)
    } else {
        format!("subagent:{}", stem)
    }
}
