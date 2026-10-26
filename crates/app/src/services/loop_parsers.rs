// SPDX-License-Identifier: MIT
//! Pure text and time helpers of the loop read side: which turn was running at a moment (`TurnTimeline`), the sections of the launch log
//! (`parse_launch_log`) and the failed tests of a verifier's return (`parse_failed_tests`). Nothing is read here.

use crate::repositories::loop_read_formatting::{clip_redacted_text, parse_iso_date};
use engine_state::loop_io::{local_from_epoch, local_to_epoch};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use time::{Date, Duration, PrimitiveDateTime, Time};

// ------------------------------------------------------------------------------------------------ turn timeline
/// Clock/second-resolution slack before a window opens.
pub const TURN_START_TOLERANCE_SECONDS: f64 = 2.0;
pub const TURN_END_TOLERANCE_SECONDS: f64 = 1.0;

/// Epoch seconds of a local ISO time (`2026-10-19T22:55:19`, extra tail ignored); anything else = `None`.
pub fn parse_epoch(text: &str) -> Option<f64> {
    let head: String = text.trim().chars().take(19).collect();
    let moment = parse_iso_date(&head)?;
    let naive = time::OffsetDateTime::from_unix_timestamp(moment.seconds.floor() as i64).ok()?;
    Some(local_to_epoch(PrimitiveDateTime::new(naive.date(), naive.time())) as f64)
}

/// Which loop turn was running at a given moment: maps a timestamped event (a guard line, a launch-log line) to a turn number.
///
/// A turn's window is `[start, end]`: the end is the mtime of `turns/NNN.json` (written when the turn finishes); the start is, in order of trust, the
/// `[loop] turn N | HH:MM:SS` line of the launch log, the end minus the `duration_ms` of the turn log, or the end of the previous turn. A window without
/// a known start is never used (unknown stays unknown). The turn in progress has an open end.
pub struct TurnTimeline {
    /// Newest turn first, so overlaps go to the later one.
    windows: Vec<(i64, (Option<f64>, Option<f64>))>,
}

impl TurnTimeline {
    pub fn new(windows: BTreeMap<i64, (Option<f64>, Option<f64>)>) -> Self {
        TurnTimeline { windows: windows.into_iter().rev().collect() }
    }

    /// `ends`: `{turn: finished epoch}`; `starts`: `{turn: started epoch}` (launch log); `durations`: `{turn: seconds}`; `running_turn`: number of the turn
    /// in progress (open end) or `None`.
    pub fn build(ends: &BTreeMap<i64, f64>, starts: &BTreeMap<i64, f64>, durations: &BTreeMap<i64, f64>, running_turn: Option<i64>) -> Self {
        let mut numbers: Vec<i64> = ends.keys().copied().collect();
        if let Some(running) = running_turn.filter(|r| *r != 0) {
            if !numbers.contains(&running) {
                numbers.push(running);
            }
        }
        numbers.sort();
        let mut windows = BTreeMap::new();
        for number in numbers {
            let end = ends.get(&number).copied();
            let start = if let Some(started) = starts.get(&number) {
                Some(*started)
            } else if let (Some(end), Some(duration)) = (end, durations.get(&number).filter(|d| **d != 0.0)) {
                Some(end - duration)
            } else {
                ends.get(&(number - 1)).copied()
            };
            windows.insert(number, (start, end));
        }
        Self::new(windows)
    }

    /// Number of the turn whose window holds `epoch`, or `None` (before the first turn, between turns, unknown time).
    pub fn turn_at(&self, epoch: Option<f64>) -> Option<i64> {
        let epoch = epoch?;
        for (number, (start, end)) in &self.windows {
            let Some(start) = start else { continue };
            if epoch < start - TURN_START_TOLERANCE_SECONDS {
                continue;
            }
            if end.is_none_or(|e| epoch <= e + TURN_END_TOLERANCE_SECONDS) {
                return Some(*number);
            }
        }
        None
    }
}

// ------------------------------------------------------------------------------------------------ launch log
pub const LAUNCH_MARKER: &str = "[loop-launch]";
pub static LAUNCH_FATAL_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::ci(r"traceback|did not start"));
pub static LAUNCH_PROBLEM_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::ci(r"traceback|error|failed|exception|PROBLEM|did not start"));
static TURN_START_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\[loop\] turn (\d+) \| (\d{2}):(\d{2}):(\d{2})"));
/// A turn line earlier than the previous one by more than this crossed midnight.
const ROLLOVER_SLACK_SECONDS: i64 = 60;

fn seconds_of_day(hours: &str, minutes: &str, seconds: &str) -> i64 {
    hours.parse::<i64>().unwrap_or(0) * 3600 + minutes.parse::<i64>().unwrap_or(0) * 60 + seconds.parse::<i64>().unwrap_or(0)
}

/// `{"marker", "time": iso|None, "lines": [{"text", "turn"}], "turn_starts": {turn: epoch}, "first_turn"}`.
pub struct Launch {
    pub marker: String,
    pub time: Option<String>,
    pub lines: Vec<(String, Option<i64>)>,
    pub turn_starts: BTreeMap<i64, f64>,
    pub first_turn: Option<i64>,
}

impl Launch {
    pub fn to_json(&self) -> Value {
        json!({
            "marker": self.marker, "time": self.time,
            "lines": self.lines.iter().map(|(text, turn)| json!({"text": text, "turn": turn})).collect::<Vec<_>>(),
            "turn_starts": self.turn_starts.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<Map<String, Value>>(),
            "first_turn": self.first_turn,
        })
    }
}

fn parse_launch(marker: &str, body: &str) -> Launch {
    let token = marker.split_whitespace().next().unwrap_or("");
    let launch_epoch = parse_epoch(token);
    let launch_time = launch_epoch.map(|_| token.chars().take(19).collect::<String>());
    let (mut lines, mut turn_starts, mut current, mut first_turn): (Vec<(String, Option<i64>)>, BTreeMap<i64, f64>, Option<i64>, Option<i64>) = (vec![], BTreeMap::new(), None, None);
    let mut day_offset = 0i64;
    let (mut previous, mut base_date): (Option<i64>, Option<Date>) = (None, None);
    if let Some(epoch) = launch_epoch {
        if let Some(started) = local_from_epoch(epoch as i64) {
            previous = Some(seconds_of_day(&started.hour().to_string(), &started.minute().to_string(), &started.second().to_string()));
            base_date = Some(started.date());
        }
    }
    for raw in pycompat::splitlines(body) {
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(found) = TURN_START_LINE.match_start(text) {
            let number: i64 = found[1].parse().unwrap_or(0);
            current = Some(number);
            first_turn = first_turn.or(current);
            if let (Some(_), Some(date)) = (launch_epoch, base_date) {
                let seconds = seconds_of_day(&found[2], &found[3], &found[4]);
                if previous.is_some_and(|p| seconds < p - ROLLOVER_SLACK_SECONDS) {
                    day_offset += 1;
                }
                previous = Some(seconds);
                let day = date + Duration::days(day_offset);
                let moment = PrimitiveDateTime::new(day, Time::MIDNIGHT) + Duration::seconds(seconds);
                turn_starts.insert(number, local_to_epoch(moment) as f64);
            }
        }
        lines.push((text.to_string(), current));
    }
    Launch { marker: marker.trim().to_string(), time: launch_time, lines, turn_starts, first_turn }
}

/// Launches of the log, oldest first. A log without any marker is one launch with an empty marker.
pub fn parse_launch_log(text: &str) -> Vec<Launch> {
    if text.is_empty() {
        return vec![];
    }
    let chunks: Vec<&str> = text.split(LAUNCH_MARKER).collect();
    let mut launches = Vec::new();
    if !chunks[0].trim().is_empty() {
        launches.push(parse_launch("", chunks[0]));
    }
    for chunk in &chunks[1..] {
        let (marker, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
        launches.push(parse_launch(marker, body));
    }
    launches
}

/// `{turn: start epoch}`: the latest launch that printed the turn wins (a rewound turn runs again).
pub fn collect_turn_starts(launches: &[Launch]) -> BTreeMap<i64, f64> {
    let mut starts = BTreeMap::new();
    for launch in launches {
        starts.extend(launch.turn_starts.iter().map(|(k, v)| (*k, *v)));
    }
    starts
}

// ------------------------------------------------------------------------------------------------ failed tests
const FAILED_TESTS_LIMIT: usize = 20;
const NAME_LIMIT: usize = 200;
const DETAIL_LIMIT: usize = 300;
static PYTEST_SUMMARY: LazyLock<Re> = LazyLock::new(|| Re::new(r"^FAILED\s+(\S+?)(?:\s+-\s+(.*))?\z"));
static PYTEST_SECTION: LazyLock<Re> = LazyLock::new(|| Re::new(r"^_{3,}\s+(.+?)\s+_{3,}\z"));
static UNITTEST_HEADER: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:FAIL|ERROR):\s+(\S+)\s+\(([\w.]+)\)"));
static JEST_HEADER: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*[●✕×]\s+(.+?)(?:\s+\(\d+\s*ms\))?\z"));
static ASSERTION: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:E\s+)?(\w*Error\b.*)\z"));
static PYTEST_ASSERT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:E\s+)?(?:\w*Error:\s*)?assert\s+(.+?)\s+==\s+(.+)\z"));
static UNITTEST_DIFFERENCE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?:AssertionError:\s*)?(.+?)\s+!=\s+(.+)\z"));
static EXPECTED_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^\W*expected(?:\s+value)?\s*[:=]\s*(.+)\z"));
static GOT_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^\W*(?:received|actual|got)(?:\s+value)?\s*[:=]\s*(.+)\z"));
static EXPECTED_GOT_INLINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"expected\s+(.+?),?\s+(?:but\s+)?(?:got|received|was|found)\s+(.+)\z"));

fn new_entry(name: &str) -> Map<String, Value> {
    Map::from_iter([("name".to_string(), json!(clip_redacted_text(name, NAME_LIMIT))), ("why".to_string(), Value::Null), ("expected".to_string(), Value::Null), ("got".to_string(), Value::Null)])
}

/// Sets a field once (the first reading wins) from a text already known to be non-empty.
fn fill(entry: &mut Map<String, Value>, key: &str, value: &str) {
    let text = clip_redacted_text(value, DETAIL_LIMIT);
    if !text.is_empty() && entry[key].is_null() {
        entry.insert(key.to_string(), json!(text));
    }
}

/// Reads one line that follows a test header and fills `why`, `expected` and `got` of that test.
fn read_detail_line(entry: &mut Map<String, Value>, line: &str) {
    let stripped = line.trim();
    let expected_got = EXPECTED_GOT_INLINE.search(stripped);
    let pytest_assert = PYTEST_ASSERT.match_start(stripped);
    let difference = if stripped.contains("AssertionError") && stripped.contains("!=") { UNITTEST_DIFFERENCE.search(stripped) } else { None };
    if let Some(found) = expected_got {
        fill(entry, "expected", &found[1]);
        fill(entry, "got", &found[2]);
    } else if let Some(found) = pytest_assert {
        fill(entry, "got", &found[1]);
        fill(entry, "expected", &found[2]);
    } else if let Some(found) = difference {
        fill(entry, "got", &found[1]);
        fill(entry, "expected", &found[2]);
    } else {
        for (key, pattern) in [("expected", &*EXPECTED_LINE), ("got", &*GOT_LINE)] {
            if let Some(found) = pattern.match_start(stripped) {
                fill(entry, key, &found[1]);
            }
        }
    }
    if let Some(found) = ASSERTION.match_start(stripped) {
        fill(entry, "why", &found[1]);
    }
}

fn find_entry(entries: &[Map<String, Value>], name: &str) -> Option<usize> {
    let short = name.rsplit("::").next().unwrap_or(name);
    entries.iter().position(|entry| {
        let existing = entry["name"].as_str().unwrap_or("");
        existing == name || existing.ends_with(&format!("::{short}")) || name.ends_with(&format!("::{existing}"))
    })
}

/// `[{"name","why","expected","got"}]` in the order found, at most 20; empty when nothing is parseable.
pub fn parse_failed_tests(text: &str) -> Vec<Value> {
    let mut entries: Vec<Map<String, Value>> = Vec::new();
    let mut current: Option<usize> = None;
    let mut orphan: Option<Map<String, Value>> = None; // an entry past the limit still receives its detail lines, like the Python object
    for raw in pycompat::splitlines(text) {
        let line = raw.trim_end();
        let stripped = line.trim();
        let summary = PYTEST_SUMMARY.match_start(stripped);
        let unittest_header = UNITTEST_HEADER.match_start(stripped);
        let section = PYTEST_SECTION.match_start(stripped);
        let jest = if summary.is_none() && unittest_header.is_none() && section.is_none() { JEST_HEADER.match_start(line) } else { None };
        let name: Option<String> = if let Some(s) = &summary {
            Some(s[1].to_string())
        } else if let Some(u) = &unittest_header {
            Some(format!("{}.{}", &u[2], &u[1]))
        } else if let Some(s) = &section {
            Some(s[1].to_string())
        } else {
            jest.as_ref().filter(|j| j[1].chars().count() > 2).map(|j| j[1].to_string())
        };
        if let Some(name) = name {
            let clipped = clip_redacted_text(&name, NAME_LIMIT);
            let existing = find_entry(&entries, &clipped);
            let mut fresh: Option<Map<String, Value>> = None;
            match existing {
                Some(index) => {
                    current = Some(index);
                    orphan = None;
                }
                None => {
                    let entry = new_entry(&name);
                    if entries.len() < FAILED_TESTS_LIMIT {
                        entries.push(entry);
                        current = Some(entries.len() - 1);
                        orphan = None;
                    } else {
                        fresh = Some(entry);
                        current = None;
                    }
                }
            }
            if fresh.is_some() {
                orphan = fresh;
            }
            if let Some(detail) = summary.as_ref().and_then(|s| s.group(2)).filter(|d| !d.is_empty()) {
                if let Some(index) = current {
                    read_detail_line(&mut entries[index], detail);
                    fill(&mut entries[index], "why", detail);
                } else if let Some(entry) = orphan.as_mut() {
                    read_detail_line(entry, detail);
                    fill(entry, "why", detail);
                }
            }
            continue;
        }
        if let Some(index) = current {
            read_detail_line(&mut entries[index], line);
        } else if let Some(entry) = orphan.as_mut() {
            read_detail_line(entry, line);
        }
    }
    entries.into_iter().map(Value::Object).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_are_found_from_their_windows() {
        let ends: BTreeMap<i64, f64> = [(1, 100.0), (2, 200.0)].into();
        let starts: BTreeMap<i64, f64> = [(2, 120.0)].into();
        let timeline = TurnTimeline::build(&ends, &starts, &BTreeMap::new(), Some(3));
        assert_eq!((timeline.turn_at(Some(150.0)), timeline.turn_at(Some(119.0)), timeline.turn_at(Some(250.0)), timeline.turn_at(None)), (Some(2), Some(2), Some(3), None));
        assert_eq!(timeline.turn_at(Some(50.0)), None); // turn 1 has no known start
        let durations: BTreeMap<i64, f64> = [(1, 40.0)].into();
        let timeline = TurnTimeline::build(&ends, &BTreeMap::new(), &durations, None);
        assert_eq!((timeline.turn_at(Some(70.0)), timeline.turn_at(Some(55.0)), timeline.turn_at(Some(150.0))), (Some(1), None, Some(2)));
        assert!(parse_epoch("2026-10-19T22:55:19 extra").is_some() && parse_epoch("nope").is_none());
    }

    #[test]
    fn launch_logs_are_split_and_turn_lines_attributed() {
        let text = "old line\n[loop-launch] 2026-10-19T22:55:19 loop_start\n[loop] error before any turn\n[loop] turn 4 | 22:56:00\nTraceback: x\n[loop] turn 5 | 22:57:30\n[loop-launch] 2026-10-19T23:59:00 resume\n[loop] turn 6 | 00:00:30\n";
        let launches = parse_launch_log(text);
        assert_eq!(launches.len(), 3);
        assert_eq!((launches[0].marker.as_str(), launches[0].time.clone()), ("", None));
        assert_eq!(launches[1].time.as_deref(), Some("2026-10-19T22:55:19"));
        assert_eq!(launches[1].lines.iter().map(|(t, n)| (t.starts_with("[loop] error"), *n)).next(), Some((true, None)));
        assert_eq!(launches[1].lines[2].1, Some(4));
        assert_eq!(launches[1].first_turn, Some(4));
        let starts = collect_turn_starts(&launches);
        assert_eq!(starts.keys().copied().collect::<Vec<_>>(), vec![4, 5, 6]);
        assert!(starts[&6] > starts[&5] + 3600.0 * 0.0 && starts[&6] - starts[&5] > 80_000.0 - 3600.0 * 24.0 + 80_000.0 * 0.0 || starts[&6] > starts[&5]);
        assert!(LAUNCH_FATAL_PATTERN.is_match("Traceback (most recent)") && LAUNCH_PROBLEM_PATTERN.is_match("1 PROBLEM") && !LAUNCH_PROBLEM_PATTERN.is_match("all good"));
        assert!(parse_launch_log("").is_empty());
    }

    #[test]
    fn failed_tests_are_read_from_pytest_unittest_and_jest_output() {
        let text = "FAILED tests/test_a.py::test_one - AssertionError: assert 1 == 2\n_____ test_two _____\nE   AssertionError: boom\nE   assert 3 == 4\nFAIL: test_three (pkg.mod.Case)\nexpected: 5\nreceived: 6\n  \u{25cf} login works \u{2192} (12 ms)\n";
        let tests = parse_failed_tests(text);
        let names: Vec<&str> = tests.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names[..3], ["tests/test_a.py::test_one", "test_two", "pkg.mod.Case.test_three"]);
        assert_eq!((tests[0]["got"].clone(), tests[0]["expected"].clone(), tests[0]["why"].clone()), (json!("1"), json!("2"), json!("AssertionError: assert 1 == 2")));
        assert_eq!((tests[1]["why"].clone(), tests[1]["got"].clone()), (json!("AssertionError: boom"), json!("3")));
        assert_eq!((tests[2]["expected"].clone(), tests[2]["got"].clone()), (json!("5"), json!("6")));
        assert!(parse_failed_tests("all passed").is_empty());
        let many: String = (0..30).map(|i| format!("FAILED t::test_{i}\n")).collect();
        assert_eq!(parse_failed_tests(&many).len(), 20);
    }
}
