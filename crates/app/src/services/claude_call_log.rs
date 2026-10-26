// SPDX-License-Identifier: MIT
//! Which command and sign-in each `claude -p` call of the panel used (chat, map, roadmap, validation, follow-up...).
//!
//! `ClaudeClient::run_prompt` hands every finished call to [`ClaudeCallLogService::record_call`]; one line per call goes to the project's call log
//! (see `repositories::small_stores`): time, kind, command, sign-in method, account/e-mail, cost, ok, and the 5-hour/weekly usage windows the
//! call's own output reported (`usage_windows`, `limit_status`; empty when the output had no rate limit event). Recording never fails and never
//! waits for `auth status`: the sign-in method comes from the account cache the loop panel already keeps warm (empty before it is read).

use super::claude_accounts::{command_name, method_code, ClaudeAccountService};
use crate::repositories::small_stores;
use engine_state::{claude_binaries, loop_io, usage_windows};
use serde_json::{json, Value};
use std::sync::Arc;

pub const UNKNOWN_KIND: &str = "panel";
pub const PUBLIC_CALL_LIMIT: usize = 50;

pub struct ClaudeCallLogService {
    pub account_service: Option<Arc<ClaudeAccountService>>,
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

impl ClaudeCallLogService {
    /// `account_service` tells the sign-in of a command; `None` = the method stays unknown.
    pub fn new(account_service: Option<Arc<ClaudeAccountService>>) -> Self {
        ClaudeCallLogService { account_service }
    }

    /// One log line from the result of `run_prompt` (binary, account, cost_usd, ok); never a secret or the reply text.
    pub fn build_entry(&self, kind: &str, result: &Value) -> Value {
        let binary = text(result.get("binary"));
        let command = command_name(if binary.is_empty() { claude_binaries::DEFAULT_BINARY_NAME } else { &binary });
        let auth = self.find_command_auth(&command);
        let cc_account = text(result.get("account"));
        let email = if cc_account.is_empty() { text(auth.get("email")) } else { String::new() };
        let cost = result.get("cost_usd").filter(|v| v.is_number()).cloned().unwrap_or(Value::Null);
        let rate_limit = result.get("raw").and_then(|r| r.get("rate_limit")).filter(|v| engine_state::notify_truthy(Some(v))).cloned().unwrap_or(json!({}));
        json!({
            "at": loop_io::now_iso(), "kind": if kind.is_empty() { UNKNOWN_KIND } else { kind }, "claude": command,
            "auth_method": method_code(&text(auth.get("method"))), "auth_email": email, "auth_plan": text(auth.get("plan")),
            "cc_account": cc_account, "cost_usd": cost, "ok": engine_state::notify_truthy(result.get("ok")),
            "usage_windows": usage_windows::with_local_reset(&usage_windows::windows_of(&rate_limit)),
            "limit_status": text(rate_limit.get("status")),
        })
    }

    fn find_command_auth(&self, command: &str) -> Value {
        let Some(service) = &self.account_service else { return json!({}) };
        let rows = service.get_rows(false);
        service.find_row(&rows, command).and_then(|row| row.get("auth").cloned()).filter(|a| a.is_object()).unwrap_or(json!({}))
    }

    /// Stores one call under `folder` (the folder the command ran in). Never fails.
    pub fn record_call(&self, folder: &str, kind: &str, result: &Value) {
        let _ = small_stores::append_call(folder, &self.build_entry(kind, result));
    }

    /// The newest `limit` calls of one or more folders (the repository and its worktree), newest first.
    pub fn list_calls(&self, folders: &[String], limit: usize) -> Vec<Value> {
        let mut seen: Vec<(String, String, String, String)> = Vec::new();
        let mut calls: Vec<Value> = Vec::new();
        for folder in folders {
            for entry in small_stores::read_calls(folder, small_stores::CALL_LOG_MAX_LINES) {
                let key = (text(entry.get("at")), text(entry.get("kind")), text(entry.get("claude")), entry.get("cost_usd").map(|v| v.to_string()).unwrap_or_default());
                if !seen.contains(&key) {
                    seen.push(key);
                    calls.push(entry);
                }
            }
        }
        calls.sort_by(|a, b| text(b.get("at")).cmp(&text(a.get("at"))));
        calls.truncate(limit);
        calls
    }

    /// Every kept call (not cut to the public limit), for the totals.
    pub fn list_all_calls(&self, folders: &[String]) -> Vec<Value> {
        self.list_calls(folders, usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_record_cost_windows_and_dedupe() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().display().to_string();
        let accounts = Arc::new(ClaudeAccountService::new(Arc::new(|| vec![json!({"name": "claude", "auth": {"method": "login", "email": "a@b.c", "plan": "pro"}})])));
        accounts.refresh();
        let service = ClaudeCallLogService::new(Some(accounts));
        let result = json!({"binary": "claude", "ok": true, "cost_usd": 0.5, "account": "", "raw": {"rate_limit": {"status": "allowed", "windows": {"five_hour": {"pct": 10.0, "reset_at": 100}}}}});
        service.record_call(&folder, "chat", &result);
        service.record_call(&folder, "", &json!({"ok": false}));
        let calls = service.list_calls(&[folder.clone(), folder.clone()], PUBLIC_CALL_LIMIT);
        assert_eq!(calls.len(), 2);
        let chat = calls.iter().find(|c| c["kind"] == "chat").unwrap();
        assert_eq!((chat["auth_method"].clone(), chat["auth_email"].clone(), chat["cost_usd"].clone(), chat["limit_status"].clone()), (json!("login"), json!("a@b.c"), json!(0.5), json!("allowed")));
        assert_eq!(chat["usage_windows"]["five_hour"]["pct"], 10.0);
        let panel = calls.iter().find(|c| c["kind"] == "panel").unwrap();
        assert_eq!((panel["ok"].clone(), panel["cost_usd"].clone(), panel["claude"].clone()), (json!(false), Value::Null, json!("claude")));
        assert_eq!(service.list_calls(&[folder], 1).len(), 1);
    }
}
