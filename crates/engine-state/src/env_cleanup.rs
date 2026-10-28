// SPDX-License-Identifier: MIT
//! What a run left in the test environment, in one place (backlog Z3).
//!
//! The E2E creates test data (records, users, tokens, scheduled tasks). The run writes one bullet for each into
//! `.specs/project/loop/CLEANUP.md`, with how to remove it:
//!
//! ```text
//! - [open] test order created by the checkout check | remove: delete it from the Orders screen | feature: checkout
//! - [done] test user created for the login check | remove: deleted through the admin API
//! ```
//!
//! The engine NEVER runs a removal itself (the file is written by an agent; running its text would be a command channel): it lists the
//! open items, and they join the manual checks so the user sees "leave the environment as found" at the end. Text is redacted before it
//! leaves, so a value written by mistake is never shown.

use crate::loop_io::{atomic_write_text, now_iso, read};
use crate::loop_state::loop_path;
use crate::secret_redaction::redact_default;
use serde_json::{json, Value};
use std::path::Path;

const FILE: &str = "CLEANUP.md";
const MAX_ITEMS: usize = 60;
const MAX_TEXT: usize = 300;

/// One bullet of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub open: bool,
    pub what: String,
    pub remove: String,
    pub feature: String,
}

fn clip(text: &str) -> String {
    redact_default(text.trim()).chars().take(MAX_TEXT).collect()
}

pub fn parse(text: &str) -> Vec<Item> {
    let mut items = Vec::new();
    for line in text.lines() {
        let Some(body) = line.trim().strip_prefix("- [") else { continue };
        let Some((state, rest)) = body.split_once(']') else { continue };
        let open = match state.trim().to_ascii_lowercase().as_str() {
            "open" => true,
            "done" => false,
            _ => continue,
        };
        let mut parts = rest.split('|');
        let what = clip(parts.next().unwrap_or(""));
        if what.is_empty() {
            continue;
        }
        let mut item = Item { open, what, remove: String::new(), feature: String::new() };
        for part in parts {
            if let Some((key, value)) = part.split_once(':') {
                match key.trim().to_ascii_lowercase().as_str() {
                    "remove" => item.remove = clip(value),
                    "feature" => item.feature = clip(value),
                    _ => {}
                }
            }
        }
        items.push(item);
        if items.len() >= MAX_ITEMS {
            break;
        }
    }
    items
}

/// Everything the run recorded, open and done.
pub fn items(work: &str) -> Vec<Item> {
    parse(&read(&loop_path(work, FILE)))
}

/// Appends an open item (what was created and how to remove it). Returns false when the file could not be written.
pub fn record(work: &str, what: &str, remove: &str, feature: &str) -> bool {
    let path = loop_path(work, FILE);
    let mut text = read(&path);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let clean = |s: &str| clip(s).replace(['|', '\n', '\r'], " ");
    let mut line = format!("- [open] {}", clean(what));
    if !remove.trim().is_empty() {
        line.push_str(&format!(" | remove: {}", clean(remove)));
    }
    if !feature.trim().is_empty() {
        line.push_str(&format!(" | feature: {}", clean(feature)));
    }
    text.push_str(&line);
    text.push_str(&format!("  <!-- {} -->\n", now_iso()));
    atomic_write_text(&path, &text).is_ok()
}

/// `{open, done, items: [{what, remove, feature}]}` of the items still open; `done` is only a count.
pub fn report(work: &str) -> Value {
    let all = items(work);
    let open: Vec<&Item> = all.iter().filter(|i| i.open).collect();
    json!({
        "open": open.len(),
        "done": all.len() - open.len(),
        "items": open.iter().map(|i| json!({"what": i.what, "remove": i.remove, "feature": i.feature})).collect::<Vec<_>>(),
    })
}

/// The open items as manual-check rows (`{feature, what, reason, source: "cleanup"}`), so the end of the run lists them.
pub fn manual_rows(work: &str) -> Vec<Value> {
    items(work)
        .into_iter()
        .filter(|i| i.open)
        .map(|i| {
            let reason = if i.remove.is_empty() { "the run left it in the test environment".to_string() } else { format!("left by the run; to remove: {}", i.remove) };
            json!({"feature": if i.feature.is_empty() { Value::Null } else { json!(i.feature) }, "what": i.what, "reason": reason, "source": "cleanup"})
        })
        .collect()
}

/// True when the file exists (so a view can say "nothing recorded" instead of "all clean").
pub fn recorded(work: &str) -> bool {
    Path::new(&loop_path(work, FILE)).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_and_done_items_with_their_fields() {
        let list = parse("# list\n- [open] test order | remove: delete it from the list | feature: checkout\n- [done] test user\n- [maybe] ignored\nplain\n- [open] \n");
        assert_eq!(list.len(), 2);
        assert!(list[0].open && list[0].feature == "checkout" && list[0].remove == "delete it from the list");
        assert!(!list[1].open);
    }

    #[test]
    fn record_appends_and_the_report_counts_open_items_only() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        assert!(!recorded(&work));
        assert!(record(&work, "scheduled task for the report check", "remove it in the scheduler | with a pipe", "reports"));
        assert!(record(&work, "test token", "", ""));
        assert!(recorded(&work));
        let report = report(&work);
        assert_eq!(report["open"], 2);
        assert_eq!(report["items"][0]["remove"], "remove it in the scheduler   with a pipe");
        assert_eq!(report["items"][1]["feature"], "");
        let rows = manual_rows(&work);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["source"], "cleanup");
        assert!(rows[1]["reason"].as_str().unwrap().contains("test environment"));
    }

    #[test]
    fn a_done_item_is_not_open_and_a_secret_is_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let path = loop_path(&work, FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "- [done] removed\n- [open] user with password=hunter2hunter2 | remove: delete the user\n").unwrap();
        let report = report(&work);
        assert_eq!((report["open"].clone(), report["done"].clone()), (json!(1), json!(1)));
        assert!(!report["items"][0]["what"].as_str().unwrap().contains("hunter2"));
    }
}
