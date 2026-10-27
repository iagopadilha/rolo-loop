// SPDX-License-Identifier: MIT
//! ROADMAP view for the panel: stages, features (with dependency clarity, summary and wave), counts and graph.
//!
//! The parser is the one in the engine's `loop_state` (never rewritten). Used by `/state` (current ROADMAP) and by `/turn/<n>` (the snapshot
//! ROADMAP).

use super::App;
use crate::repositories::loop_read_formatting::{clip_redacted_text, to_text_list};
use engine_state::loop_state::{self, Fields, FEATURE_LINE, STAGE_LINE, TITLE_FIELD};
use pycompat::{escape, splitlines, Re};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

const CLOSED_STATUSES: [&str; 2] = ["verified", "blocked"];
const IN_PROGRESS_STATUSES: [&str; 3] = ["building", "speccing", "approved"];
const SIZES: [&str; 4] = ["Small", "Medium", "Large", "Complex"];
const GOAL_LIMIT: usize = 600;
const STAGE_DESCRIPTION_LIMIT: usize = 600;
const STAGE_CLOSING_LIMIT: usize = 1200;
const FEATURE_NAME_LIMIT: usize = 90;
const SPEC_TITLE_LINES: usize = 15;
const STAGE_CLOSING_SECTIONS: [(&str, &str); 6] = [
    ("Delivered", "delivered"),
    ("User decisions", "decisions"),
    ("Lessons", "lessons"),
    ("Unverified", "unverified"),
    ("Blocked", "blocked"),
    ("For the next stage", "next_stage"),
];
const SPEC_SIZE_HEAD_LENGTH: usize = 3000;
const WAVE_REASON_DISJOINT: &str = "disjoint paths, no dependency between them";
const WAVE_REASON_TOGETHER: &str = "dispatched together in the same turn";
const WAVE_REASON_ALONE: &str = "alone in the turn";
const WAVE_REASON_WAITING: &str = "waiting on: ";
const WAVE_REASON_NO_OPEN_DEPENDENCY: &str = "no open dependency";
const NO_STAGE_TITLE: &str = "No stage";
const SOURCE_SPEC: &str = "spec.md";
const SOURCE_ROADMAP: &str = "roadmap";
const SOURCE_NONE: &str = "none";

static STAGE_TITLE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^#{1,6}\s*(Stage\s+\S+.*)\z"));
static HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#{1,6}\s+(.*?)\s*#*\s*\z"));
static ANY_HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#{1,6}\s"));
static SPEC_GOAL_TITLES: LazyLock<[Re; 2]> = LazyLock::new(|| [Re::ci(r"^(vision|goal|objective|summary|overview)\b"), Re::ci(r"^(problem|context|background)\b")]);
static METADATA_TITLE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^metadata\b"));
static REQUIREMENT_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\|\s*([A-Z][A-Z0-9]{0,5}-\d+[a-z]?)\s*\|"));
static SPEC_SIZE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\bSCOPE\b\W{0,6}(Small|Medium|Large|Complex)\b"));
static COMMIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"commit\s+([0-9a-f]{7,40})"));
static FIELD_PAIR: LazyLock<Re> = LazyLock::new(|| Re::new(r"\b\w+=\S+"));

pub fn list_stage_titles(text: &str) -> Vec<String> {
    splitlines(text).into_iter().filter_map(|line| STAGE_TITLE.match_start(line.trim()).map(|m| m[1].trim().to_string())).collect()
}

/// `[(title, body)]` of the `#`..`######` headings; the text before the 1st heading comes with title ''.
pub fn split_markdown_sections(text: &str) -> Vec<(String, String)> {
    let (mut sections, mut title, mut body): (Vec<(String, String)>, String, Vec<&str>) = (vec![], String::new(), vec![]);
    for line in splitlines(text) {
        if let Some(found) = HEADING.match_start(line) {
            sections.push((title, body.join("\n")));
            title = found[1].trim().to_string();
            body = vec![];
        } else {
            body.push(line);
        }
    }
    sections.push((title, body.join("\n")));
    sections
}

pub fn extract_first_paragraph(body: &str) -> String {
    let mut paragraph: Vec<&str> = Vec::new();
    for raw in splitlines(body) {
        let line = raw.trim();
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if ["|", "```", "- **", "<!--"].iter().any(|p| line.starts_with(p)) {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(line);
    }
    paragraph.join(" ")
}

pub fn are_paths_overlapping(first: &str, second: &str) -> bool {
    let (first, second) = (first.trim_matches(['/', '\\']), second.trim_matches(['/', '\\']));
    first == second || first.starts_with(&format!("{second}/")) || second.starts_with(&format!("{first}/"))
}

/// `{stage label: text between the stage heading and the 1st feature}` of the ROADMAP.
pub fn extract_stage_descriptions(text: &str) -> HashMap<String, String> {
    let mut found: HashMap<String, String> = HashMap::new();
    let (mut label, mut lines, mut closed): (Option<String>, Vec<&str>, bool) = (None, vec![], true);
    for raw in splitlines(text) {
        let line = raw.trim();
        let heading = STAGE_LINE.match_start(line);
        if heading.is_some() || ANY_HEADING.is_match(line) {
            if let (Some(label), false) = (&label, lines.is_empty()) {
                found.insert(label.clone(), lines.join("\n"));
            }
            label = heading.map(|h| h[1].to_string());
            lines = vec![];
            closed = false;
        } else if label.is_some() && !closed {
            if FEATURE_LINE.match_start(line).is_some() {
                closed = true;
            } else if !line.is_empty() {
                lines.push(line);
            }
        }
    }
    if let (Some(label), false) = (&label, lines.is_empty()) {
        found.entry(label.clone()).or_insert_with(|| lines.join("\n"));
    }
    found
}

fn str_of<'a>(feature: &'a Map<String, Value>, key: &str) -> &'a str {
    feature.get(key).and_then(Value::as_str).unwrap_or("")
}

fn list_of(feature: &Map<String, Value>, key: &str) -> Vec<Value> {
    feature.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// depends_on/dep_dirs/unblocks/waiting_on/blocked_by (only slugs go into depends_on; dir:<folder> goes into dep_dirs).
fn add_dependency_clarity(features: &mut [Map<String, Value>], parsed: &loop_state::Features) {
    let status_by_id: HashMap<String, Value> = features.iter().map(|f| (str_of(f, "id").to_string(), f["status"].clone())).collect();
    let mut unblocked: HashMap<String, Vec<String>> = features.iter().map(|f| (str_of(f, "id").to_string(), vec![])).collect();
    for feature in features.iter_mut() {
        let id = str_of(feature, "id").to_string();
        let (slugs, folders) = loop_state::split_deps(parsed.get(&id).unwrap());
        feature.insert(
            "depends_on".into(),
            json!(slugs.iter().map(|d| json!({"id": d, "status": status_by_id.get(d).cloned().unwrap_or(Value::Null)})).collect::<Vec<_>>()),
        );
        feature.insert("dep_dirs".into(), json!(folders));
        for dependency in &slugs {
            if let Some(list) = unblocked.get_mut(dependency) {
                if !list.contains(&id) {
                    list.push(id.clone());
                }
            }
        }
    }
    for feature in features.iter_mut() {
        let depends = list_of(feature, "depends_on");
        let ids = |keep: &dyn Fn(&Value) -> bool| depends.iter().filter(|d| keep(&d["status"])).map(|d| d["id"].clone()).collect::<Vec<_>>();
        let (waiting, blocked) = (ids(&|s| s != "verified"), ids(&|s| s == "blocked"));
        let id = str_of(feature, "id").to_string();
        feature.insert("unblocks".into(), json!(unblocked.get(&id).cloned().unwrap_or_default()));
        feature.insert("waiting_on".into(), json!(waiting));
        feature.insert("blocked_by".into(), json!(blocked));
    }
}

fn path_list(fields: &Fields) -> Vec<String> {
    fields.text("paths").split(',').filter(|p| !p.is_empty()).map(String::from).collect()
}

/// features[].wave: last wave that named the feature, who went along and why; not run yet = upcoming, with what it waits on.
fn add_wave_summary(features: &mut [Map<String, Value>], parsed: &loop_state::Features, waves: &[(u64, Vec<String>)]) {
    let mut last: HashMap<&str, (u64, &Vec<String>)> = HashMap::new();
    for (number, slugs) in waves {
        for slug in slugs {
            last.insert(slug, (*number, slugs));
        }
    }
    let ids: HashSet<String> = features.iter().map(|f| str_of(f, "id").to_string()).collect();
    for feature in features.iter_mut() {
        let slug = str_of(feature, "id").to_string();
        let status = str_of(feature, "status").to_string();
        let wave = if let Some((number, slugs)) = last.get(slug.as_str()) {
            let together: Vec<String> = slugs.iter().filter(|o| **o != slug && ids.contains(*o)).cloned().collect();
            let reason = if !together.is_empty() {
                let members: Vec<&String> = std::iter::once(&slug).chain(together.iter()).collect();
                let independent = !members.iter().any(|a| members.iter().any(|b| a != b && loop_state::split_deps(parsed.get(a).unwrap()).0.contains(b)));
                let paths: Vec<Vec<String>> = members.iter().map(|m| path_list(parsed.get(m).unwrap())).collect();
                let disjoint = paths.iter().all(|p| !p.is_empty())
                    && !(0..members.len()).any(|i| (i + 1..members.len()).any(|j| paths[i].iter().any(|x| paths[j].iter().any(|y| are_paths_overlapping(x, y)))));
                if independent && disjoint {
                    WAVE_REASON_DISJOINT.to_string()
                } else {
                    WAVE_REASON_TOGETHER.to_string()
                }
            } else {
                match feature.get("regression").and_then(Value::as_str).filter(|r| !r.is_empty()) {
                    Some(regression) => format!("runs alone in the wave (regression={regression})"),
                    None => WAVE_REASON_ALONE.to_string(),
                }
            };
            json!({"turn": number, "together_with": together, "reason": reason, "upcoming": false})
        } else if !CLOSED_STATUSES.contains(&status.as_str()) {
            let waiting: Vec<String> = list_of(feature, "waiting_on").iter().map(|w| w.as_str().unwrap_or("").to_string()).collect();
            let reason = if waiting.is_empty() { WAVE_REASON_NO_OPEN_DEPENDENCY.to_string() } else { format!("{WAVE_REASON_WAITING}{}", waiting.join(", ")) };
            json!({"turn": null, "together_with": [], "upcoming": true, "reason": reason})
        } else {
            Value::Null
        };
        feature.insert("wave".into(), wave);
    }
}

/// Python `str.capitalize`.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

pub struct RoadmapViewService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn roadmap_view(&self) -> RoadmapViewService<'_> {
        RoadmapViewService { app: self }
    }
}

impl RoadmapViewService<'_> {
    /// `(stages, features, counts, graph)` from the text of a ROADMAP; empty when there are no features.
    pub fn build_roadmap_view(&self, work_dir: &str, text: &str, up_to_turn: Option<u64>) -> (Vec<Value>, Vec<Value>, Value, Value) {
        let (parsed, _) = loop_state::parse_roadmap(text);
        let mut labels: Vec<String> = Vec::new();
        for (_, fields) in parsed.iter() {
            let label = fields.text("stage").to_string();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        let titles = list_stage_titles(text);
        let number_by_label: HashMap<&str, i64> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                (
                    label.as_str(),
                    if !label.is_empty() && label.chars().all(|c| c.is_ascii_digit()) { label.parse().unwrap_or(index as i64 + 1) } else { index as i64 + 1 },
                )
            })
            .collect();
        let mut stages: Vec<Map<String, Value>> = Vec::new();
        for label in &labels {
            let title = if label.is_empty() {
                None
            } else {
                Re::try_build(&format!(r"Stage\s+{}\b", escape(label)), true).and_then(|re| titles.iter().find(|t| re.match_start(t).is_some()).cloned())
            };
            let members: Vec<&String> = parsed.iter().filter(|(_, f)| f.text("stage") == label).map(|(s, _)| s).collect();
            let mut stage = Map::new();
            stage.insert("n".into(), json!(number_by_label[label.as_str()]));
            stage.insert("title".into(), json!(title.unwrap_or_else(|| if label.is_empty() { NO_STAGE_TITLE.to_string() } else { format!("Stage {label}") })));
            stage.insert("features".into(), json!(members));
            stages.push(stage);
        }
        let slugs = parsed.slugs();
        let commits = self.read_commits_from_state(work_dir, &slugs);
        let mut features: Vec<Map<String, Value>> = Vec::new();
        for (slug, fields) in parsed.iter() {
            let (dep_slugs, folders) = loop_state::split_deps(fields);
            let tries = fields.text("tries");
            let status = fields.get("status");
            let commit = match fields.get("commit").filter(|c| !c.is_empty()) {
                Some(c) => Some(c.to_string()),
                None if status == Some("verified") => commits.get(slug).cloned(),
                None => None,
            };
            let mut feature = Map::new();
            feature.insert("id".into(), json!(slug));
            feature.insert("name".into(), json!(self.read_feature_name(work_dir, slug, Some(fields))));
            feature.insert("status".into(), json!(status));
            feature.insert("stage".into(), json!(number_by_label.get(fields.text("stage"))));
            feature.insert("deps".into(), json!(dep_slugs.iter().cloned().chain(folders.iter().map(|f| format!("{}{f}", loop_state::DIR_PREFIX))).collect::<Vec<_>>()));
            feature.insert("paths".into(), json!(path_list(fields)));
            feature.insert("kind".into(), json!(fields.get("kind").filter(|k| !k.is_empty()).unwrap_or("?")));
            feature.insert("tries".into(), json!(if !tries.is_empty() && tries.chars().all(|c| c.is_ascii_digit()) { tries.parse::<i64>().unwrap_or(0) } else { 0 }));
            feature.insert("note".into(), json!(fields.get("note")));
            feature.insert("commit".into(), json!(commit));
            feature.insert("regression".into(), json!(fields.get("regression")));
            feature.insert("e2e".into(), json!(fields.get("e2e")));
            features.push(feature);
        }
        add_dependency_clarity(&mut features, &parsed);
        let waves = self.list_snapshot_waves(work_dir, up_to_turn);
        for feature in features.iter_mut() {
            let slug = str_of(feature, "id").to_string();
            let summary = self.build_feature_summary(work_dir, &slug, parsed.get(&slug).unwrap(), text);
            feature.insert("summary".into(), summary);
        }
        add_wave_summary(&mut features, &parsed, &waves);
        self.add_stage_summaries(work_dir, &mut stages, &features, text, &waves, &labels);
        let mut counts = Map::new();
        for status in loop_state::STATUSES {
            counts.insert(status.to_string(), json!(features.iter().filter(|f| str_of(f, "status") == status).count()));
        }
        counts.insert("total".into(), json!(features.len()));
        let edges: Vec<Value> =
            parsed.iter().flat_map(|(slug, fields)| loop_state::split_deps(fields).0.into_iter().filter(|d| parsed.contains(d)).map(move |d| json!([d, slug]))).collect();
        (
            stages.into_iter().map(Value::Object).collect(),
            features.into_iter().map(Value::Object).collect(),
            Value::Object(counts),
            json!({"nodes": slugs, "edges": edges}),
        )
    }

    /// Readable name of a feature: the `title="..."` of its ROADMAP line (what the user approved), else the first heading of its spec.md (without
    /// the `Spec:` / slug prefix), else the slug.
    pub fn read_feature_name(&self, work_dir: &str, slug: &str, fields: Option<&Fields>) -> String {
        let title = fields.map(|f| f.text("title")).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
        if !title.is_empty() {
            return title.chars().take(FEATURE_NAME_LIMIT).collect();
        }
        for line in self.app.loop_files.read_feature_spec_head(work_dir, slug, SPEC_TITLE_LINES) {
            if let Some(heading) = line.strip_prefix("# ") {
                let heading = heading.trim();
                let prefix = Re::try_build(&format!(r"^(Spec(ification)?\s*[:\-—]\s*)?({}\s*[:\-—]\s*)?", escape(slug)), true);
                let rest = match prefix.and_then(|re| re.search(heading)) {
                    Some(m) => &heading[m.end..],
                    None => heading,
                };
                let name: String = rest.chars().take(FEATURE_NAME_LIMIT).collect();
                return if name.is_empty() { slug.to_string() } else { name };
            }
        }
        slug.to_string()
    }

    /// Commit hash of each feature, when STATE.md names `<slug> ... commit <hash>` on the same line.
    pub fn read_commits_from_state(&self, work_dir: &str, slugs: &[String]) -> HashMap<String, String> {
        let mut found: HashMap<String, String> = HashMap::new();
        for line in splitlines(&self.app.loop_files.read_state_file(work_dir, "STATE.md")) {
            if let Some(m) = COMMIT.search(line) {
                for slug in slugs {
                    if line.contains(slug.as_str()) {
                        found.entry(slug.clone()).or_insert_with(|| m[1].chars().take(7).collect());
                    }
                }
            }
        }
        found
    }

    /// goal, size, requirements and source (spec.md | roadmap | none). No data = null.
    pub fn build_feature_summary(&self, work_dir: &str, slug: &str, fields: &Fields, roadmap_text: &str) -> Value {
        let size = fields.get("scope").filter(|s| SIZES.contains(s));
        let spec = self.app.loop_files.read_feature_spec(work_dir, slug);
        if !spec.trim().is_empty() {
            let sections = split_markdown_sections(&spec);
            let mut goal: Option<String> = None;
            'wanted: for wanted in SPEC_GOAL_TITLES.iter() {
                for (title, body) in &sections {
                    if !title.is_empty() && wanted.match_start(title).is_some() {
                        let paragraph = extract_first_paragraph(body);
                        if !paragraph.is_empty() {
                            goal = Some(paragraph);
                            break 'wanted;
                        }
                    }
                }
            }
            if goal.is_none() {
                for (title, body) in sections.iter().skip(1) {
                    if !title.is_empty() && METADATA_TITLE.match_start(title).is_none() {
                        let paragraph = extract_first_paragraph(body);
                        if !paragraph.is_empty() {
                            goal = Some(paragraph);
                            break;
                        }
                    }
                }
            }
            let head: String = spec.chars().take(SPEC_SIZE_HEAD_LENGTH).collect();
            let spec_size = SPEC_SIZE.search(&head);
            let requirements: BTreeSet<String> = splitlines(&spec).into_iter().filter_map(|l| REQUIREMENT_LINE.match_start(l.trim()).map(|m| m[1].to_string())).collect();
            return json!({
                "goal": goal.map(|g| clip_redacted_text(&g, GOAL_LIMIT)),
                "size": spec_size.map(|m| capitalize(&m[1])).or_else(|| size.map(String::from)),
                "requirements": if requirements.is_empty() { Value::Null } else { json!(requirements.len()) },
                "source": SOURCE_SPEC,
            });
        }
        let mut description: Option<String> = None;
        for raw in splitlines(roadmap_text) {
            if let Some(line) = FEATURE_LINE.match_start(raw.trim()) {
                if line[1] == *slug {
                    let cleaned = FIELD_PAIR.replace_lit(&TITLE_FIELD.replace_lit(&line[2], ""), "");
                    let cleaned = cleaned.trim_matches([' ', '-', ':', ';', '|']);
                    description = if cleaned.is_empty() { None } else { Some(cleaned.to_string()) };
                }
            }
        }
        json!({
            "goal": description.as_ref().map(|d| clip_redacted_text(d, GOAL_LIMIT)), "size": size, "requirements": null,
            "source": if description.is_some() || size.is_some() { SOURCE_ROADMAP } else { SOURCE_NONE },
        })
    }

    /// `[(turn, [slugs])]` from the `feature` field of the LOOP.json of each snapshots/NNN (ascending order).
    pub fn list_snapshot_waves(&self, work_dir: &str, up_to_turn: Option<u64>) -> Vec<(u64, Vec<String>)> {
        let mut waves = Vec::new();
        for number in self.app.loop_files.list_snapshot_numbers(work_dir) {
            if up_to_turn.is_some_and(|up_to| number > up_to) {
                break;
            }
            let Some(loop_json) = self.app.loop_files.read_cached_snapshot_loop_json(work_dir, number) else { continue };
            let slugs = to_text_list(loop_json.get("feature").unwrap_or(&Value::Null));
            if !slugs.is_empty() {
                waves.push((number, slugs));
            }
        }
        waves
    }

    pub fn read_stage_closing(&self, work_dir: &str, number: i64) -> Value {
        let text = self.app.loop_files.read_stage_closing(work_dir, number.max(0) as u64);
        if text.trim().is_empty() {
            return Value::Null;
        }
        let mut closing: Map<String, Value> = STAGE_CLOSING_SECTIONS.iter().map(|(_, key)| (key.to_string(), Value::Null)).collect();
        for (title, body) in split_markdown_sections(&text) {
            if let Some((_, key)) = STAGE_CLOSING_SECTIONS.iter().find(|(name, _)| *name == title) {
                if !body.trim().is_empty() {
                    closing.insert(key.to_string(), json!(clip_redacted_text(&body, STAGE_CLOSING_LIMIT)));
                }
            }
        }
        Value::Object(closing)
    }

    fn add_stage_summaries(
        &self,
        work_dir: &str,
        stages: &mut [Map<String, Value>],
        features: &[Map<String, Value>],
        text: &str,
        waves: &[(u64, Vec<String>)],
        labels: &[String],
    ) {
        let by_id: HashMap<&str, &Map<String, Value>> = features.iter().map(|f| (str_of(f, "id"), f)).collect();
        let descriptions = extract_stage_descriptions(text);
        for (stage, label) in stages.iter_mut().zip(labels) {
            let member_ids: Vec<String> = list_of(stage, "features").iter().map(|s| s.as_str().unwrap_or("").to_string()).collect();
            let members: Vec<&Map<String, Value>> = member_ids.iter().filter_map(|s| by_id.get(s.as_str()).copied()).collect();
            let current = members
                .iter()
                .find(|f| IN_PROGRESS_STATUSES.contains(&str_of(f, "status")))
                .or_else(|| members.iter().find(|f| !CLOSED_STATUSES.contains(&str_of(f, "status"))));
            let stage_waves: Vec<Value> = waves
                .iter()
                .filter(|(_, slugs)| slugs.iter().any(|s| member_ids.contains(s)))
                .map(|(number, slugs)| json!({"turn": number, "features": slugs.iter().filter(|s| member_ids.contains(*s)).collect::<Vec<_>>()}))
                .collect();
            let count = |status: &str| members.iter().filter(|f| str_of(f, "status") == status).count();
            let n = stage["n"].as_i64().unwrap_or(0);
            let summary = json!({
                "description": descriptions.get(label).map(|d| clip_redacted_text(d, STAGE_DESCRIPTION_LIMIT)),
                "progress": {"verified": count("verified"), "blocked": count("blocked"), "total": members.len()},
                "current_feature": current.map(|f| f["id"].clone()), "waves": stage_waves, "closing": self.read_stage_closing(work_dir, n),
            });
            stage.insert("summary".into(), summary);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROADMAP: &str = "## Stage 1 base\nFoundation text.\n- [a] status=verified paths=src/a scope=Small title=\"Feature A\"\n- [b] status=pending deps=a paths=src/b\n## Stage 2 later\n- [c] status=pending deps=a,dir:shared paths=src/b/x regression=yes\n";

    #[test]
    fn view_has_stages_dependencies_counts_and_graph() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let work = dir.path().join("w");
        std::fs::create_dir_all(&work).unwrap();
        let (stages, features, counts, graph) = app.roadmap_view().build_roadmap_view(&work.display().to_string(), ROADMAP, None);
        assert_eq!(stages.len(), 2);
        assert_eq!(
            (stages[0]["title"].clone(), stages[0]["n"].clone(), stages[0]["summary"]["progress"].clone()),
            (json!("Stage 1 base"), json!(1), json!({"verified": 1, "blocked": 0, "total": 2}))
        );
        assert_eq!(stages[0]["summary"]["description"], "Foundation text.");
        assert_eq!(stages[0]["summary"]["current_feature"], "b");
        assert_eq!((features[0]["name"].clone(), features[0]["unblocks"].clone()), (json!("Feature A"), json!(["b", "c"])));
        assert_eq!((features[2]["deps"].clone(), features[2]["waiting_on"].clone()), (json!(["a", "dir:shared"]), json!([])));
        assert_eq!(features[1]["wave"]["reason"], "no open dependency");
        assert_eq!(features[0]["summary"], json!({"goal": null, "size": "Small", "requirements": null, "source": "roadmap"}));
        assert_eq!((counts["verified"].clone(), counts["pending"].clone(), counts["total"].clone()), (json!(1), json!(2), json!(3)));
        assert_eq!(graph["edges"], json!([["a", "b"], ["a", "c"]]));
    }

    #[test]
    fn spec_gives_goal_size_and_requirements() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let work = dir.path().join("w");
        let feature_dir = work.join(".specs/features/a");
        std::fs::create_dir_all(&feature_dir).unwrap();
        std::fs::write(
            feature_dir.join("spec.md"),
            "# Spec: a - Nice name\nSCOPE: Large\n\n## Goal\nDo the thing well.\n\n| REQ-1 | x |\n| REQ-1 | y |\n| REQ-2 | z |\n",
        )
        .unwrap();
        let (_, features, _, _) = app.roadmap_view().build_roadmap_view(&work.display().to_string(), "- [a] status=pending\n", None);
        assert_eq!(features[0]["name"], "Nice name");
        assert_eq!(features[0]["summary"], json!({"goal": "Do the thing well.", "size": "Large", "requirements": 2, "source": "spec.md"}));
    }
}
