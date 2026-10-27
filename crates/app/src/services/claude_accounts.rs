// SPDX-License-Identifier: MIT
//! Which Claude account the loop is using, for the panel: command, auth method, e-mail, organization, plan, `cc` account name.
//!
//! `claude <command> auth status` takes seconds (the `cc` picker asks every account for its usage), so the rows are cached and the panel poll
//! never waits for them: a stale cache is refreshed by one background thread while the poll gets the last rows (or none yet). The diagnostics
//! screen (`GET /api/app/info`) may wait, since the user asked for that screen.

use engine_state::claude_binaries;
use engine_state::loop_io;
use pycompat::{json::py_json_dumps, Re};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

pub const CACHE_TTL_SECONDS: f64 = 300.0;
pub const DEFAULT_COMMAND: &str = claude_binaries::DEFAULT_BINARY_NAME;
static COMMAND_EXTENSION: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\.(cmd|bat|exe|py)\z"));
static CC_ACCOUNT_LABEL: LazyLock<Re> = LazyLock::new(|| Re::new(r"cc account +(\S+)"));
static EMAIL: LazyLock<Re> = LazyLock::new(|| Re::new(r"\S+@\S+"));
static ORG_PLAN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\(([^)]*)\)\s*\z"));
const KNOWN_METHODS: [&str; 3] = ["login", "token", "API key"];
pub const MAIN_ROLE: &str = "main";
/// Label word (what `auth status` gives) -> code kept in files.
const METHOD_CODES: [(&str, &str); 3] = [("login", "login"), ("token", "token"), ("API key", "api_key")];
pub const METHOD_WORDS: [(&str, &str); 4] =
    [("login", "subscription · browser login"), ("token", "subscription · sign-in token"), ("api_key", "API key · billed per token"), ("", "sign-in unknown")];
const UNKNOWN_COMMAND: &str = "unknown";
const EXHAUSTED_PCT: f64 = 100.0;
const WINDOW_LABELS: [(&str, &str); 2] = [("five_hour", "5-hour"), ("seven_day", "weekly")];
const WINDOW_ORDER: [&str; 2] = ["five_hour", "seven_day"];

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) if engine_state::notify_truthy(Some(v)) => py_json_dumps(v, false),
        _ => String::new(),
    }
}

/// `claude`, `cc`: a command path/name without its folder and extension ('' when empty).
pub fn command_name(path_or_name: &str) -> String {
    let base = path_or_name.replace('\\', "/");
    let base = base.rsplit('/').next().unwrap_or("");
    COMMAND_EXTENSION.replace_lit(base, "")
}

/// Fields of a label written by the loop ("login dev@example.com (Acme, team)", "token, cc account claude2") ->
/// `{"method", "email", "org", "plan", "cc_account"}`; unknown parts are ''.
pub fn parse_account_label(label: &str) -> Map<String, Value> {
    let mut text = label.trim().to_string();
    let mut fields: Map<String, Value> = ["method", "email", "org", "plan", "cc_account"].iter().map(|k| (k.to_string(), json!(""))).collect();
    if text.is_empty() {
        return fields;
    }
    if let Some(extra) = ORG_PLAN.search(&text) {
        let mut parts: Vec<String> = extra[1].split(',').map(|p| p.trim().to_string()).collect();
        parts.push(String::new());
        fields.insert("org".into(), json!(parts[0]));
        fields.insert("plan".into(), json!(parts[1]));
        text = text[..extra.start].trim().to_string();
    }
    for method in KNOWN_METHODS {
        if text.starts_with(method) {
            fields.insert("method".into(), json!(method));
            break;
        }
    }
    if let Some(email) = EMAIL.search(&text) {
        fields.insert("email".into(), json!(email.text().trim_matches(',')));
    }
    if let Some(account) = CC_ACCOUNT_LABEL.search(&text) {
        fields.insert("cc_account".into(), json!(&account[1]));
    }
    fields
}

/// Code of a sign-in method given as a label word ("login", "token", "API key") or already as a code ("api_key"); '' when unknown.
pub fn method_code(method: &str) -> String {
    let text = method.trim();
    if let Some((_, code)) = METHOD_CODES.iter().find(|(word, _)| *word == text) {
        return code.to_string();
    }
    if !text.is_empty() && METHOD_WORDS.iter().any(|(code, _)| *code == text) {
        text.to_string()
    } else {
        String::new()
    }
}

fn field_str(record: &Value, key: &str) -> String {
    text_of(record.get(key))
}

/// Structured sign-in of one turn record (or the meta): `{"auth_method", "auth_email", "auth_plan", "cc_account"}`, never a secret. The loop
/// writes these fields next to `claude_account`; a turn written before that is read from the `claude_account` label.
pub fn build_turn_auth(record: &Value) -> Value {
    if record.get("auth_method").is_some() || record.get("cc_account").is_some() {
        return json!({"auth_method": method_code(&field_str(record, "auth_method")), "auth_email": field_str(record, "auth_email"), "auth_plan": field_str(record, "auth_plan"), "cc_account": field_str(record, "cc_account")});
    }
    let parsed = parse_account_label(&field_str(record, "claude_account"));
    let get = |k: &str| parsed.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    json!({"auth_method": method_code(&get("method")), "auth_email": get("email"), "auth_plan": get("plan"), "cc_account": get("cc_account")})
}

/// The command, sign-in and usage fields a turn row carries. The usage fields are empty for a turn written before the loop read them.
pub fn build_turn_claude(record: &Value) -> Value {
    let mut row = Map::new();
    row.insert("claude".into(), json!(field_str(record, "claude")));
    row.insert("claude_account".into(), json!(field_str(record, "claude_account")));
    if let Value::Object(auth) = build_turn_auth(record) {
        row.extend(auth);
    }
    row.insert("usage_windows".into(), clean_windows(record.get("usage_windows")));
    row.insert("limit_status".into(), json!(field_str(record, "limit_status")));
    Value::Object(row)
}

/// A number as float; `None` for anything else (booleans included).
pub fn to_float(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(v) if v.is_number() => v.as_f64(),
        _ => None,
    }
}

/// Windows as the loop wrote them (`{key: {"pct", "reset_at", "reset_local"}}`) -> the same shape with only valid entries; `{}` when none.
pub fn clean_windows(windows: Option<&Value>) -> Value {
    let mut cleaned = Map::new();
    if let Some(Value::Object(map)) = windows {
        for (key, window) in map {
            let Some(pct) = window.as_object().and_then(|w| to_float(w.get("pct"))) else { continue };
            let reset_at = to_float(window.get("reset_at"));
            let reset_value = match reset_at {
                Some(r) if r != 0.0 => json!(r as i64),
                _ => Value::Null,
            };
            cleaned.insert(key.clone(), json!({"pct": pct, "reset_at": reset_value, "reset_local": text_of(window.get("reset_local"))}));
        }
    }
    Value::Object(cleaned)
}

/// `[{key, label, pct, reset_at, reset_local, exhausted}]`: 5-hour first, weekly second, any other window after them.
pub fn build_window_rows(windows: Option<&Value>) -> Vec<Value> {
    let cleaned = clean_windows(windows);
    let map = cleaned.as_object().cloned().unwrap_or_default();
    let mut keys: Vec<String> = WINDOW_ORDER.iter().filter(|k| map.contains_key(**k)).map(|k| k.to_string()).collect();
    let mut others: Vec<String> = map.keys().filter(|k| !WINDOW_ORDER.contains(&k.as_str())).cloned().collect();
    others.sort();
    keys.extend(others);
    keys.iter()
        .map(|key| {
            let window = map[key].as_object().cloned().unwrap_or_default();
            let label = WINDOW_LABELS.iter().find(|(k, _)| k == key).map(|(_, l)| l.to_string()).unwrap_or_else(|| key.replace('_', " "));
            let pct = window.get("pct").and_then(Value::as_f64).unwrap_or(0.0);
            let mut row = window;
            row.insert("key".into(), json!(key));
            row.insert("label".into(), json!(label));
            row.insert("exhausted".into(), json!(pct >= EXHAUSTED_PCT));
            Value::Object(row)
        })
        .collect()
}

/// Epoch seconds of an ISO time written by the loop (a time without zone is local time); `None` when unreadable.
pub fn parse_read_at(text: &str) -> Option<f64> {
    let moment = crate::repositories::loop_read_formatting::parse_iso_date(text)?;
    if moment.naive {
        let naive = time::OffsetDateTime::from_unix_timestamp(moment.seconds.floor() as i64).ok()?;
        let epoch = loop_io::local_to_epoch(time::PrimitiveDateTime::new(naive.date(), naive.time()));
        return Some(epoch as f64 + moment.seconds.fract());
    }
    Some(moment.seconds)
}

/// "cc:claudep" -> ("cc", "claudep"); "claude" -> ("claude", "").
pub fn split_usage_key(key: &str) -> (String, String) {
    match key.split_once(':') {
        Some((command, account)) => (command.to_string(), account.to_string()),
        None => (key.to_string(), String::new()),
    }
}

pub use pycompat::format_g;

/// "5-hour 60 % (reset 14:30)" / "weekly 33 % (reset Mon 27/10 09:00)": the local reset text is shown as the loop wrote it.
pub fn format_window_line(window: &Value) -> String {
    let mut reset = String::new();
    if let Some(reset_at) = window.get("reset_at").filter(|v| engine_state::notify_truthy(Some(v))).and_then(Value::as_f64) {
        if let Some(moment) = loop_io::local_from_epoch(reset_at as i64) {
            let same_day = moment.date() == loop_io::local_now().date();
            let clock = if same_day || window["key"] == "five_hour" {
                moment.format(time::macros::format_description!("[hour]:[minute]")).unwrap_or_default()
            } else {
                moment.format(time::macros::format_description!("[weekday repr:short] [day]/[month] [hour]:[minute]")).unwrap_or_default()
            };
            reset = format!(" (reset {clock})");
        }
    }
    let pct = window.get("pct").and_then(Value::as_f64).unwrap_or(0.0);
    format!("{} {} %{reset}", window["label"].as_str().unwrap_or(""), format_g((pct * 10.0).round_ties_even() / 10.0))
}

/// One doctor line for a usage-view row: the windows joined by " · ", with "exhausted" on the ones at 100 %; "usage unknown" without data.
pub fn format_usage_line(row: &Value) -> String {
    let windows = row["windows"].as_array().cloned().unwrap_or_default();
    if windows.is_empty() {
        return "usage unknown".into();
    }
    windows.iter().map(|w| format!("{}{}", format_window_line(w), if w["exhausted"] == true { " EXHAUSTED" } else { "" })).collect::<Vec<_>>().join(" · ")
}

#[derive(Default)]
struct UsageRow {
    turns: i64,
    calls: i64,
    cost_usd: f64,
}

fn add_usage_row(rows: &mut BTreeMap<String, UsageRow>, key: String, field: &str, cost: f64) {
    let row = rows.entry(key).or_default();
    if field == "turns" {
        row.turns += 1
    } else {
        row.calls += 1
    }
    row.cost_usd += cost;
}

fn round4(value: f64) -> f64 {
    (value * 10000.0).round_ties_even() / 10000.0
}

/// Turns (loop) and panel calls summed per sign-in method and per command: `{"by_method", "by_command", "turns", "calls", "cost_usd"}`. A row
/// carries `claude` (command), `auth_method` and `cost_usd`; rows without a command count as "unknown".
pub fn build_usage_totals(turn_rows: &[Value], call_rows: &[Value]) -> Value {
    let (mut by_method, mut by_command): (BTreeMap<String, UsageRow>, BTreeMap<String, UsageRow>) = (BTreeMap::new(), BTreeMap::new());
    for (field, rows) in [("turns", turn_rows), ("calls", call_rows)] {
        for row in rows {
            let cost = to_float(row.get("cost_usd")).unwrap_or(0.0);
            add_usage_row(&mut by_method, method_code(&field_str(row, "auth_method")), field, cost);
            let command = command_name(&field_str(row, "claude"));
            add_usage_row(&mut by_command, if command.is_empty() { UNKNOWN_COMMAND.to_string() } else { command }, field, cost);
        }
    }
    let finish = |rows: &BTreeMap<String, UsageRow>, key_name: &str| -> Vec<Value> {
        let mut sorted: Vec<(&String, &UsageRow)> = rows.iter().collect();
        sorted.sort_by(|a, b| b.1.cost_usd.partial_cmp(&a.1.cost_usd).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(b.0)));
        sorted.into_iter().map(|(key, row)| json!({key_name: key, "turns": row.turns, "calls": row.calls, "cost_usd": round4(row.cost_usd)})).collect()
    };
    let methods = finish(&by_method, "method");
    let sum = |field: &str| methods.iter().map(|m| m[field].as_i64().unwrap_or(0)).sum::<i64>();
    json!({"by_method": methods, "by_command": finish(&by_command, "command"), "turns": sum("turns"), "calls": sum("calls"),
           "cost_usd": round4(by_method.values().map(|r| r.cost_usd).sum::<f64>())})
}

/// The newest turn/call row of `key` that has usage windows, as `{windows, status, read_at}`; `None` if none.
fn newest_usage_entry(rows: &[Value], key: &str) -> Option<Value> {
    for row in rows.iter().rev() {
        let (command, account) = (command_name(&field_str(row, "claude")), field_str(row, "cc_account"));
        let row_key = if account.is_empty() { command } else { format!("{command}:{account}") };
        if row_key == key && clean_windows(row.get("usage_windows")).as_object().is_some_and(|m| !m.is_empty()) {
            let read_at = if engine_state::notify_truthy(row.get("at")) { row.get("at").cloned() } else { row.get("read_at").cloned() }.unwrap_or(json!(""));
            return Some(json!({"windows": row["usage_windows"], "status": row.get("limit_status").cloned().unwrap_or(Value::Null), "read_at": read_at}));
        }
    }
    None
}

/// The usage of every command/account, from what the loop already wrote (never queries anything, never waits):
/// `[{key, command, cc_account, windows, status, read_at, age_seconds}]`. Source order per key: `meta.claude_usage`, else the newest turn, else the
/// newest panel call (newest first). A configured command never seen gets empty windows ("usage unknown"). Configured commands come first.
pub fn build_usage_view(meta: &Value, configured_rows: &[Value], turn_rows: &[Value], call_rows: &[Value], now: f64) -> Vec<Value> {
    let stored: Map<String, Value> = meta.get("claude_usage").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut keys: Vec<String> = Vec::new();
    for row in configured_rows {
        let name = command_name(&field_str(row, "name"));
        if !name.is_empty() {
            keys.push(name);
        }
    }
    let mut stored_keys: Vec<String> = stored.keys().filter(|k| !keys.contains(k)).cloned().collect();
    stored_keys.sort();
    keys.extend(stored_keys);
    for rows in [turn_rows, call_rows] {
        for row in rows.iter().rev() {
            let (command, account) = (command_name(&field_str(row, "claude")), field_str(row, "cc_account"));
            let key = if account.is_empty() { command } else { format!("{command}:{account}") };
            if !key.is_empty() && !keys.contains(&key) && clean_windows(row.get("usage_windows")).as_object().is_some_and(|m| !m.is_empty()) {
                keys.push(key);
            }
        }
    }
    let all = keys.clone();
    keys.retain(|key| stored.contains_key(key) || !all.iter().any(|other| other.starts_with(&format!("{key}:")))); // `cc` is shown per picked account
    let reversed_calls: Vec<Value> = call_rows.iter().rev().cloned().collect();
    let mut view = Vec::new();
    for key in keys {
        let mut entry = stored.get(&key).filter(|e| e.is_object()).cloned();
        if !entry.as_ref().is_some_and(|e| clean_windows(e.get("windows")).as_object().is_some_and(|m| !m.is_empty())) {
            entry = newest_usage_entry(turn_rows, &key).or_else(|| newest_usage_entry(&reversed_calls, &key)).or(Some(json!({})));
        }
        let entry = entry.unwrap_or_else(|| json!({}));
        let (command, account) = split_usage_key(&key);
        let read_epoch = parse_read_at(&text_of(entry.get("read_at")));
        view.push(json!({
            "key": key, "command": command, "cc_account": account, "windows": build_window_rows(entry.get("windows")),
            "status": text_of(entry.get("status")), "read_at": text_of(entry.get("read_at")),
            "age_seconds": read_epoch.map(|r| ((now - r) as i64).max(0)),
        }));
    }
    view
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

type Describe = dyn Fn() -> Vec<Value> + Send + Sync;

struct AccountCache {
    rows: Option<Vec<Value>>,
    read_at: f64,
    refreshing: bool,
}

pub struct ClaudeAccountService {
    describe_commands: Arc<Describe>,
    ttl_seconds: f64,
    cache: Arc<Mutex<AccountCache>>,
}

impl ClaudeAccountService {
    /// `describe_commands()` -> the rows of `claude_binaries::describe`.
    pub fn new(describe_commands: Arc<Describe>) -> Self {
        Self::with_ttl(describe_commands, CACHE_TTL_SECONDS)
    }

    pub fn with_ttl(describe_commands: Arc<Describe>, ttl_seconds: f64) -> Self {
        ClaudeAccountService { describe_commands, ttl_seconds, cache: Arc::new(Mutex::new(AccountCache { rows: None, read_at: 0.0, refreshing: false })) }
    }

    /// The service of a configured command list.
    pub fn for_commands(primary: &str, fallbacks: &[String]) -> Self {
        let (primary, fallbacks) = (primary.to_string(), fallbacks.to_vec());
        Self::new(Arc::new(move || claude_binaries::describe(&primary, &fallbacks)))
    }

    fn is_fresh(&self) -> bool {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.rows.is_some() && now_epoch() - cache.read_at < self.ttl_seconds
    }

    fn refresh_into(describe: &Arc<Describe>, cache: &Arc<Mutex<AccountCache>>) {
        let rows = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| describe())).ok(); // the panel must never fail because `auth status` did
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rows) = rows {
            cache.rows = Some(rows);
            cache.read_at = now_epoch();
        }
        cache.refreshing = false;
    }

    /// Runs the (slow) describe now and stores it; a failure keeps the previous rows.
    pub fn refresh(&self) {
        Self::refresh_into(&self.describe_commands, &self.cache);
    }

    /// Cached rows (`[]` when none yet). `wait` = compute now when stale; otherwise a stale cache starts ONE background refresh.
    pub fn get_rows(&self, wait: bool) -> Vec<Value> {
        if !self.is_fresh() {
            if wait {
                self.refresh();
            } else {
                let start = {
                    let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                    let start = !cache.refreshing;
                    cache.refreshing = true;
                    start
                };
                if start {
                    let (describe, cache) = (self.describe_commands.clone(), self.cache.clone());
                    let _ = std::thread::Builder::new().name("claude-account-refresh".into()).spawn(move || Self::refresh_into(&describe, &cache));
                }
            }
        }
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).rows.clone().unwrap_or_default()
    }

    /// Row of `command` (compared by name without extension); the main row (first) when `command` is empty.
    pub fn find_row(&self, rows: &[Value], command: &str) -> Option<Value> {
        if command.is_empty() {
            return rows.first().cloned();
        }
        rows.iter().find(|row| command_name(&field_str(row, "name")) == command).cloned()
    }

    pub fn build_usage_view(&self, meta: &Value, turn_rows: &[Value], call_rows: &[Value], wait: bool) -> Vec<Value> {
        build_usage_view(meta, &self.get_rows(wait), turn_rows, call_rows, now_epoch())
    }

    /// `{command, role, method, email, org, plan, cc_account, switches, label}` of the account in use. The per-turn label in meta (what the loop
    /// really ran) wins; the diagnostics row fills what it lacks and gives the command before turn 1.
    pub fn build_current_account(&self, meta: &Value) -> Value {
        let rows = self.get_rows(false);
        let command = command_name(&field_str(meta, "claude"));
        let row = self.find_row(&rows, &command);
        let row_value = row.clone().unwrap_or(json!({}));
        let auth = row_value.get("auth").cloned().unwrap_or(json!({}));
        let auth_text = |key: &str| text_of(auth.get(key));
        let label = field_str(meta, "claude_account");
        let parsed = parse_account_label(&label);
        let parsed_text = |key: &str| parsed.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let same_account = parsed_text("email").is_empty() || parsed_text("email") == auth_text("email"); // the row only completes the same account
        let or = |first: String, second: String| if first.is_empty() { second } else { first };
        let role = match row_value.get("role") {
            Some(v) if engine_state::notify_truthy(Some(v)) => text_of(Some(v)),
            _ if command == DEFAULT_COMMAND => MAIN_ROLE.to_string(),
            _ => String::new(),
        };
        let method = or(parsed_text("method"), auth_text("method"));
        json!({
            "command": or(command.clone(), command_name(&field_str(&row_value, "name"))),
            "role": role,
            "method": method,
            "email": or(parsed_text("email"), auth_text("email")),
            "org": or(parsed_text("org"), if same_account { auth_text("org") } else { String::new() }),
            "plan": or(parsed_text("plan"), if same_account { auth_text("plan") } else { String::new() }),
            "cc_account": parsed_text("cc_account"),
            "auth_method": method_code(&method),
            "switches": if engine_state::notify_truthy(meta.get("claude_switches")) { meta["claude_switches"].clone() } else { json!(0) },
            "label": or(label, field_str(&row_value, "label")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_methods_and_windows() {
        assert_eq!(command_name("C:\\bin\\cc.CMD"), "cc");
        assert_eq!(command_name(""), "");
        let parsed = parse_account_label("login dev@example.com (Acme, team)");
        assert_eq!(
            (parsed["method"].clone(), parsed["email"].clone(), parsed["org"].clone(), parsed["plan"].clone()),
            (json!("login"), json!("dev@example.com"), json!("Acme"), json!("team"))
        );
        assert_eq!(parse_account_label("token, cc account claude2")["cc_account"], "claude2");
        assert_eq!(
            (method_code("API key"), method_code("api_key"), method_code("x"), method_code("")),
            ("api_key".into(), "api_key".into(), String::new(), String::new())
        );
        let auth = build_turn_auth(&json!({"claude_account": "token dev@example.com"}));
        assert_eq!((auth["auth_method"].clone(), auth["auth_email"].clone()), (json!("token"), json!("dev@example.com")));
        let rows = build_window_rows(Some(&json!({"seven_day": {"pct": 100, "reset_at": 5.0}, "x_y": {"pct": 1}, "five_hour": {"pct": "bad"}})));
        assert_eq!(rows.iter().map(|r| r["key"].as_str().unwrap()).collect::<Vec<_>>(), ["seven_day", "x_y"]);
        assert_eq!(
            (rows[0]["exhausted"].clone(), rows[0]["label"].clone(), rows[1]["label"].clone(), rows[1]["reset_at"].clone()),
            (json!(true), json!("weekly"), json!("x y"), Value::Null)
        );
        assert_eq!((format_g(60.0), format_g(33.3), format_g(0.5), format_g(1234567.0)), ("60".into(), "33.3".into(), "0.5".into(), "1.23457e+06".into()));
        assert_eq!(format_usage_line(&json!({"windows": []})), "usage unknown");
        assert_eq!(format_usage_line(&json!({"windows": [{"key": "five_hour", "label": "5-hour", "pct": 100.0, "exhausted": true}]})), "5-hour 100 % EXHAUSTED");
    }

    #[test]
    fn totals_and_usage_view() {
        let turns = vec![
            json!({"claude": "claude", "auth_method": "login", "cost_usd": 1.5}),
            json!({"claude": "cc", "auth_method": "token", "cost_usd": 0.25}),
            json!({"auth_method": "login"}),
        ];
        let totals = build_usage_totals(&turns, &[json!({"claude": "cc.cmd", "cost_usd": 1})]);
        assert_eq!(totals["by_method"][0]["method"], "login");
        assert_eq!((totals["turns"].clone(), totals["calls"].clone(), totals["cost_usd"].clone()), (json!(3), json!(1), json!(2.75)));
        assert_eq!(totals["by_command"].as_array().unwrap().iter().map(|c| c["command"].as_str().unwrap()).collect::<Vec<_>>(), ["claude", "cc", "unknown"]);
        let meta = json!({"claude_usage": {"claude": {"windows": {"five_hour": {"pct": 40.0, "reset_at": 1}}, "status": "allowed", "read_at": "2026-01-01T10:00:00Z"}}});
        let view = build_usage_view(&meta, &[json!({"name": "claude"}), json!({"name": "cc"})], &[], &[], 1_800_000_000.0);
        assert_eq!(view.len(), 2);
        assert_eq!((view[0]["windows"][0]["pct"].clone(), view[0]["status"].clone()), (json!(40.0), json!("allowed")));
        assert!(view[0]["age_seconds"].as_i64().unwrap() > 0 && view[1]["age_seconds"].is_null() && view[1]["windows"] == json!([]));
    }

    #[test]
    fn rows_are_cached_and_a_failure_keeps_them() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let service = ClaudeAccountService::new(Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            vec![json!({"name": "claude", "role": "main", "auth": {"method": "login", "email": "a@b.c", "org": "O", "plan": "pro"}, "label": "L"})]
        }));
        assert_eq!(service.get_rows(true).len(), 1);
        assert_eq!(service.get_rows(true).len(), 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let current = service.build_current_account(&json!({"claude": "claude", "claude_switches": 2}));
        assert_eq!(
            (current["method"].clone(), current["email"].clone(), current["role"].clone(), current["switches"].clone(), current["label"].clone()),
            (json!("login"), json!("a@b.c"), json!("main"), json!(2), json!("L"))
        );
        let mismatch = service.build_current_account(&json!({"claude": "claude", "claude_account": "login other@x.y (Z, max)"}));
        assert_eq!((mismatch["org"].clone(), mismatch["plan"].clone()), (json!("Z"), json!("max")));
        let empty = ClaudeAccountService::new(Arc::new(|| panic!("boom")));
        assert!(empty.get_rows(true).is_empty());
        assert!(service.find_row(&service.get_rows(false), "").is_some() && service.find_row(&service.get_rows(false), "nope").is_none());
    }
}
