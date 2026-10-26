// SPDX-License-Identifier: MIT
//! Coverage of an import: every requirement/decision/task (or, without IDs, every section) of the source document must appear in the coverage
//! table with a destination or an explicit `dropped: <reason>`. Pure functions, no I/O.
//!
//! Grammar of the coverage text: `## <slug>` headings, each followed by a Markdown table `| Item | Kind | Destination |`. An item can be a range
//! (`ORIG-01..03` -> ORIG-01, ORIG-02, ORIG-03; zero padding is kept). Rows before any heading go under "".

use pycompat::{splitlines, Re};
use serde_json::{json, Value};
use std::sync::LazyLock;

static HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^##\s+(.+?)\s*#*\s*\z"));
static SEPARATOR_CELL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^:?-{2,}:?\z"));
static RANGE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(.*?)(\d+)\s*\.\.\s*([^\d]*)(\d+)\z"));
const HEADER_ITEM_WORDS: [&str; 2] = ["item", "items"];
const DROPPED_PREFIX: &str = "dropped:";
const MAX_RANGE_SIZE: u64 = 1000;
const ITEM_COLUMN: usize = 0;
const DESTINATION_COLUMN: usize = 2;

/// `{slug: [(item, destination)]}` in file order.
pub type Coverage = Vec<(String, Vec<(String, String)>)>;

pub fn split_table_cells(line: &str) -> Vec<String> {
    let mut inner = line.trim();
    inner = inner.strip_prefix('|').unwrap_or(inner);
    inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

pub fn is_separator_row(cells: &[String]) -> bool {
    !cells.is_empty() && cells.iter().filter(|c| !c.is_empty()).all(|c| SEPARATOR_CELL.match_start(c).is_some()) && cells.iter().any(|c| !c.is_empty())
}

/// `ORIG-01..03` -> `['ORIG-01','ORIG-02','ORIG-03']`; anything that is not a valid range comes back as `[item]`.
pub fn expand_item_range(item: &str) -> Vec<String> {
    let Some(m) = RANGE.match_start(item) else { return vec![item.to_string()] };
    let (first_text, last_text) = (m.group(2).unwrap_or(""), m.group(4).unwrap_or(""));
    let (Ok(first), Ok(last)) = (first_text.parse::<u64>(), last_text.parse::<u64>()) else { return vec![item.to_string()] };
    if last < first || last - first >= MAX_RANGE_SIZE {
        return vec![item.to_string()];
    }
    let width = if first_text.starts_with('0') { first_text.len() } else { 0 };
    (first..=last).map(|n| format!("{}{:0>width$}", m.group(1).unwrap_or(""), n, width = width)).collect()
}

fn strip_marks(text: &str) -> String {
    text.trim_matches(['`', '*', ' ']).trim().to_string()
}

/// `{slug: {item: destination}}` from the coverage text (header and separator rows ignored, ranges expanded).
pub fn parse_coverage(text: &str) -> Coverage {
    let mut coverage: Coverage = Vec::new();
    let mut slug = String::new();
    for line in splitlines(text) {
        let stripped = line.trim();
        if let Some(h) = HEADING.match_start(stripped) {
            slug = h.group(1).unwrap_or("").trim().trim_matches('`').to_string();
            continue;
        }
        if !stripped.starts_with('|') {
            continue;
        }
        let cells = split_table_cells(stripped);
        if is_separator_row(&cells) || cells.len() <= ITEM_COLUMN {
            continue;
        }
        let item = strip_marks(&cells[ITEM_COLUMN]);
        if item.is_empty() || HEADER_ITEM_WORDS.contains(&item.to_lowercase().as_str()) {
            continue;
        }
        let destination = cells.get(DESTINATION_COLUMN).map(|d| strip_marks(d)).unwrap_or_default();
        for expanded in expand_item_range(&item) {
            let index = match coverage.iter().position(|(s, _)| *s == slug) {
                Some(i) => i,
                None => {
                    coverage.push((slug.clone(), vec![]));
                    coverage.len() - 1
                }
            };
            let table = &mut coverage[index].1;
            match table.iter_mut().find(|(i, _)| *i == expanded) {
                Some(slot) => slot.1 = destination.clone(),
                None => table.push((expanded, destination.clone())),
            }
        }
    }
    coverage
}

pub fn merge_destinations(coverage: &Coverage) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = Vec::new();
    for (_, table) in coverage {
        for (item, destination) in table {
            match merged.iter_mut().find(|(i, _)| i == item) {
                Some(slot) => slot.1 = destination.clone(),
                None => merged.push((item.clone(), destination.clone())),
            }
        }
    }
    merged
}

/// Identifiers the source holds: requirements, decisions and tasks; without IDs, the sections that are present.
pub fn collect_expected_items(items: &Value) -> Vec<String> {
    let mut identifiers: Vec<String> = Vec::new();
    for key in ["requirements", "decisions", "tasks"] {
        for value in items.get(key).and_then(Value::as_array).into_iter().flatten() {
            let text = value.as_str().map(String::from).unwrap_or_else(|| value.to_string()).trim().to_string();
            if !text.is_empty() {
                identifiers.push(text);
            }
        }
    }
    if !identifiers.is_empty() {
        return identifiers;
    }
    items.get("sections").and_then(Value::as_object).map(|s| s.iter().filter(|(_, present)| engine_state::notify_truthy(Some(present))).map(|(name, _)| name.clone()).collect()).unwrap_or_default()
}

/// `{"covered": [...], "dropped": [{"item","reason"}], "missing": [...]}` for the source's items against the merged tables.
pub fn check_coverage(items: &Value, coverage: &Coverage) -> Value {
    let destinations = merge_destinations(coverage);
    let (mut covered, mut dropped, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    for item in collect_expected_items(items) {
        let destination = destinations.iter().find(|(i, _)| *i == item).map(|(_, d)| d.trim().to_string()).unwrap_or_default();
        if destination.is_empty() {
            missing.push(json!(item));
        } else if destination.to_lowercase().starts_with(DROPPED_PREFIX) {
            dropped.push(json!({"item": item, "reason": destination[DROPPED_PREFIX.len()..].trim()}));
        } else {
            covered.push(json!(item));
        }
    }
    json!({"covered": covered, "dropped": dropped, "missing": missing})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_tables_and_checks() {
        assert_eq!(expand_item_range("ORIG-01..03"), vec!["ORIG-01", "ORIG-02", "ORIG-03"]);
        assert_eq!(expand_item_range("R-9..11"), vec!["R-9", "R-10", "R-11"]);
        assert_eq!(expand_item_range("R-3..1"), vec!["R-3..1"]);
        assert_eq!(expand_item_range("plain"), vec!["plain"]);
        let text = "| Item | Kind | Destination |\n|---|---|---|\n| `A-1` | req | f1 |\n## feat-a\n| Item | Kind | Destination |\n| --- | --- | --- |\n| A-2..3 | req | spec |\n| A-4 | req | dropped: not needed |\n| D-1 | dec | |\n";
        let coverage = parse_coverage(text);
        assert_eq!(coverage.iter().map(|(s, t)| (s.as_str(), t.len())).collect::<Vec<_>>(), vec![("", 1), ("feat-a", 4)]);
        let items = json!({"requirements": ["A-1", "A-2", "A-3", "A-4", "A-5"], "decisions": ["D-1"]});
        let result = check_coverage(&items, &coverage);
        assert_eq!(result["covered"], json!(["A-1", "A-2", "A-3"]));
        assert_eq!(result["dropped"], json!([{"item": "A-4", "reason": "not needed"}]));
        assert_eq!(result["missing"], json!(["A-5", "D-1"]));
        assert_eq!(collect_expected_items(&json!({"sections": {"Goals": true, "Edge Cases": false}})), vec!["Goals"]);
    }
}
