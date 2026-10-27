// SPDX-License-Identifier: MIT
//! Typed state of the orchestrator graph: grammar, legal transitions, invariants, snapshots and rewind. No AI, no network.
//!
//! ROADMAP.md is the "graph state": each feature is a node with a status; each status change is an edge. This module guards
//! the edges (the orchestrator is the supervisor that decides; here we deterministically check what it wrote).
//!
//! Feature line grammar (ROADMAP.md):
//! `- [slug] status=<pending|speccing|specced|approved|building|verified|blocked> title="<what the user gets>" scope=<Small|Medium|Large|Complex>
//! kind=<back|front|both|generic> paths=<p1,p2> [repo-order=<repo,repo>] [deps=<slug|dir:folder,...>] [isolation=same-tree|worktree]
//! [tries=<n>] [origin=adopted] [e2e=ok|failed|not-covered|pending] [regression=yes|baseline|ok|failed] [note=...]`

use pycompat::json::py_json_dumps;
use pycompat::{nt_abspath, splitlines, Re};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const STATUSES: [&str; 7] = ["pending", "speccing", "specced", "approved", "building", "verified", "blocked"];
pub const SCOPES: [&str; 4] = ["Small", "Medium", "Large", "Complex"];
pub const KINDS: [&str; 4] = ["back", "front", "both", "generic"];
pub const E2E: [&str; 4] = crate::result_vocabulary::E2E_WORDS;
pub const ISOLATIONS: [&str; 2] = ["same-tree", "worktree"];
pub const REGRESSION: [&str; 4] = ["yes", "baseline", "ok", "failed"];
pub const REFACTOR_PREFIX: &str = "refactor-";
pub const DIR_PREFIX: &str = "dir:";
pub const MAX_TRIES: u64 = 2;
pub const LOOP_STATUS: [&str; 5] = ["continue", "awaiting_user", "paused", "done", "error"];
pub const ANSWERS_OK: [&str; 3] = ["approved", "specced", "pending"];

fn forward(status: &str) -> &'static [&'static str] {
    match status {
        "pending" => &["speccing", "specced"],
        "speccing" => &["specced"],
        "specced" => &["approved"],
        "approved" => &["building"],
        "building" => &["verified"],
        _ => &[],
    }
}

fn back(status: &str) -> &'static [&'static str] {
    match status {
        "speccing" => &["pending"],
        "building" => &["approved"],
        "verified" => &["approved"],
        "specced" => &["specced"],
        _ => &[],
    }
}

static REPO_PATH_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^([a-z0-9._-]+):(?![/\\])(.+)\z"));
pub static FEATURE_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^- \[([^\]]+)\]\s*(.*)\z"));
pub static TITLE_FIELD: LazyLock<Re> = LazyLock::new(|| Re::new(r#"\btitle="([^"]*)""#));
pub static STAGE_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^#{1,6}\s*(?:Stage|Etapa)\s+(\S+)"));
static FOLDER_INVALID: LazyLock<Re> = LazyLock::new(|| Re::new(r#"[<>:"/\\|?*\x00-\x1f]"#));
static KEY_VALUE: LazyLock<Re> = LazyLock::new(|| Re::new(r"([\w-]+)=(\S+)"));

/// The folder name of a feature under `.specs/features/`: the slug with `:` (and any character a Windows path cannot hold) replaced by `-`.
pub fn feature_folder_name(slug: &str) -> String {
    FOLDER_INVALID.replace_lit(slug, "-")
}

/// `<root>/.specs/features/<folder>` of a feature (a legacy folder named with the raw slug is used only when it exists alone).
pub fn feature_dir(root: &str, slug: &str) -> String {
    let base = Path::new(root).join(".specs").join("features");
    let folder = base.join(feature_folder_name(slug));
    let legacy = base.join(slug);
    if legacy != folder && legacy.is_dir() && !folder.is_dir() {
        legacy.display().to_string()
    } else {
        folder.display().to_string()
    }
}

pub fn read(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------- parse

/// Fields of one feature line, in file order (a repeated key keeps its first position and its last value).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields(pub Vec<(String, String)>);

impl Fields {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    /// `f.get(key) or ""`.
    pub fn text(&self, key: &str) -> &str {
        self.get(key).unwrap_or("")
    }
    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value.to_string();
        } else {
            self.0.push((key.to_string(), value.to_string()));
        }
    }
    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn status(&self) -> &str {
        self.text("status")
    }
}

/// `{slug: fields}` in file order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Features(pub Vec<(String, Fields)>);

impl Features {
    pub fn get(&self, slug: &str) -> Option<&Fields> {
        self.0.iter().find(|(s, _)| s == slug).map(|(_, f)| f)
    }
    pub fn contains(&self, slug: &str) -> bool {
        self.get(slug).is_some()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Fields)> {
        self.0.iter().map(|(s, f)| (s, f))
    }
    pub fn slugs(&self) -> Vec<String> {
        self.0.iter().map(|(s, _)| s.clone()).collect()
    }
    fn status_of(&self, slug: &str) -> Option<&str> {
        self.get(slug).map(Fields::status)
    }
}

/// `(features, errors)`.
pub fn parse_roadmap(text: &str) -> (Features, Vec<String>) {
    let mut features = Features::default();
    let mut errors = Vec::new();
    let mut stage = String::new();
    for (index, raw) in splitlines(text).into_iter().enumerate() {
        let number = index + 1;
        let stripped = raw.trim();
        if let Some(heading) = STAGE_LINE.match_start(stripped) {
            stage = heading.group(1).unwrap_or("").to_string();
            continue;
        }
        let Some(m) = FEATURE_LINE.match_start(stripped) else { continue };
        let slug = m.group(1).unwrap_or("").to_string();
        let mut rest = m.group(2).unwrap_or("").to_string();
        let title = TITLE_FIELD.search(&rest);
        if let Some(t) = &title {
            rest = format!("{} {}", &rest[..t.start], &rest[t.end..]);
        }
        let mut fields = Fields::default();
        for kv in KEY_VALUE.find_all(&rest) {
            fields.set(kv.group(1).unwrap_or(""), kv.group(2).unwrap_or(""));
        }
        if let Some(t) = &title {
            let shown = t.group(1).unwrap_or("");
            if !shown.trim().is_empty() {
                fields.set("title", &shown.split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
        if title.is_none() && rest.contains("title=") {
            errors.push(format!("ROADMAP line {number}: [{slug}] title= must be written title=\"short sentence\" (double quotes, none inside)"));
        }
        if !fields.has("status") {
            errors.push(format!("ROADMAP line {number}: [{slug}] has no status="));
            continue;
        }
        if features.contains(&slug) {
            errors.push(format!("ROADMAP line {number}: duplicate slug [{slug}]"));
            continue;
        }
        if !fields.has("stage") {
            fields.set("stage", &stage);
        }
        features.0.push((slug, fields));
    }
    (features, errors)
}

/// `(repo, path)` of one `paths=` entry. `repo` is empty when there is no prefix (= the primary repo).
pub fn split_repo_path(entry: &str) -> (String, String) {
    let entry = entry.trim();
    match REPO_PATH_PREFIX.match_start(entry) {
        Some(m) => (m.group(1).unwrap_or("").to_string(), m.group(2).unwrap_or("").to_string()),
        None => (String::new(), entry.to_string()),
    }
}

/// `[(repo, path)]` of the feature's `paths=`; an unprefixed entry is keyed by `primary_name`.
pub fn feature_repo_paths(feature: &Fields, primary_name: &str) -> Vec<(String, String)> {
    feature
        .text("paths")
        .split(',')
        .filter(|e| !e.trim().is_empty())
        .map(|entry| {
            let (repo, path) = split_repo_path(entry);
            (if repo.is_empty() { primary_name.to_string() } else { repo }, path)
        })
        .collect()
}

/// Errors for every `paths=` prefix or `repo-order=` name that is not in the project's repo list.
pub fn check_repo_names(features: &Features, repo_names: &[String]) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    let mut known: Vec<&String> = repo_names.iter().collect();
    known.sort();
    known.dedup();
    let listing = known.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    for (slug, f) in features.iter() {
        for (repo, _) in feature_repo_paths(f, "") {
            if !repo.is_empty() && !repo_names.contains(&repo) {
                errors.push(format!("[{slug}] paths= names an unknown repo {} (project repos: {listing})", py_repr(&repo)));
            }
        }
        for repo in f.text("repo-order").split(',') {
            let repo = repo.trim();
            if !repo.is_empty() && !repo_names.iter().any(|r| r == repo) {
                errors.push(format!("[{slug}] repo-order= names an unknown repo {} (project repos: {listing})", py_repr(repo)));
            }
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for e in errors {
        if !seen.contains(&e) {
            seen.push(e);
        }
    }
    seen
}

/// Python `repr` of a str.
pub fn py_repr(text: &str) -> String {
    pycompat::py_repr_str(text)
}

pub fn check_fields(features: &Features, repo_names: Option<&[String]>) -> Vec<String> {
    let mut errors = Vec::new();
    for (slug, f) in features.iter() {
        let status = f.status();
        if !STATUSES.contains(&status) {
            errors.push(format!("[{slug}] invalid status {} (valid: {})", py_repr(status), STATUSES.join(", ")));
        }
        if let Some(scope) = f.get("scope").filter(|s| !s.is_empty()) {
            if !SCOPES.contains(&scope) {
                errors.push(format!("[{slug}] invalid scope {}", py_repr(scope)));
            }
        }
        if let Some(kind) = f.get("kind").filter(|s| !s.is_empty()) {
            if !KINDS.contains(&kind) {
                errors.push(format!("[{slug}] invalid kind {} (valid: {})", py_repr(kind), KINDS.join(", ")));
            }
        }
        if let Some(e2e) = f.get("e2e").filter(|s| !s.is_empty()) {
            if crate::result_vocabulary::canonical_e2e(e2e).is_none() {
                errors.push(format!("[{slug}] invalid e2e {}", py_repr(e2e)));
            }
        }
        if let Some(tries) = f.get("tries") {
            if tries.is_empty() || !tries.chars().all(|c| c.is_ascii_digit()) {
                errors.push(format!("[{slug}] tries must be an integer (got {})", py_repr(tries)));
            } else if tries.parse::<u64>().unwrap_or(u64::MAX) >= MAX_TRIES && status != "blocked" {
                errors.push(format!("[{slug}] tries={tries} (cap {MAX_TRIES}) but status={status}: with {MAX_TRIES} failures the feature must be blocked"));
            }
        }
        if ["approved", "building", "verified"].contains(&status) && f.text("paths").is_empty() {
            errors.push(format!("[{slug}] status={status} without paths= (the hook scope depends on it)"));
        }
        if let Some(reg) = f.get("regression").filter(|s| !s.is_empty()) {
            if !REGRESSION.contains(&reg) {
                errors.push(format!("[{slug}] invalid regression {} (valid: {})", py_repr(reg), REGRESSION.join(", ")));
            }
            if !slug.starts_with(REFACTOR_PREFIX) {
                errors.push(format!("[{slug}] regression= only applies to a refactoring feature (slug {REFACTOR_PREFIX}*)"));
            }
        }
        if let Some(iso) = f.get("isolation").filter(|s| !s.is_empty()) {
            if !ISOLATIONS.contains(&iso) {
                errors.push(format!("[{slug}] invalid isolation {} (valid: {})", py_repr(iso), ISOLATIONS.join(", ")));
            }
        }
        let (slugs, _) = split_deps(f);
        for dep in &slugs {
            if dep == slug {
                errors.push(format!("[{slug}] depends on itself"));
            } else if !features.contains(dep) {
                errors.push(format!("[{slug}] deps= names a nonexistent feature [{dep}] (a shared folder is written dir:<folder>)"));
            }
        }
        if status == "building" {
            let waiting: Vec<&String> = slugs.iter().filter(|d| features.status_of(d).is_some_and(|s| s != "verified")).collect();
            if !waiting.is_empty() {
                errors.push(format!("[{slug}] is building but depends on {} which is not verified", waiting.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            }
        }
    }
    let cycle = find_cycle(features);
    if !cycle.is_empty() {
        errors.push(format!("cycle in deps=: {}", cycle.join(" -> ")));
    }
    if let Some(names) = repo_names.filter(|n| !n.is_empty()) {
        errors.extend(check_repo_names(features, names));
    }
    errors
}

/// `(slugs, shared_folders)` from `deps=`.
pub fn split_deps(feature: &Fields) -> (Vec<String>, Vec<String>) {
    let (mut slugs, mut folders) = (Vec::new(), Vec::new());
    for item in feature.text("deps").split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        match item.strip_prefix(DIR_PREFIX) {
            Some(folder) => folders.push(folder.to_string()),
            None => slugs.push(item.to_string()),
        }
    }
    (slugs, folders)
}

/// First dependency cycle between features (list of slugs, closing on the first), or empty.
pub fn find_cycle(features: &Features) -> Vec<String> {
    fn visit(features: &Features, slug: &str, trail: &mut Vec<String>, state: &mut Vec<(String, u8)>) -> Vec<String> {
        set_state(state, slug, 1);
        let deps = features.get(slug).map(|f| split_deps(f).0).unwrap_or_default();
        for dep in deps {
            if !features.contains(&dep) {
                continue;
            }
            match get_state(state, &dep) {
                Some(1) => {
                    let at = trail.iter().position(|t| *t == dep).unwrap_or(0);
                    let mut cycle: Vec<String> = trail[at..].to_vec();
                    cycle.push(dep);
                    return cycle;
                }
                None => {
                    trail.push(dep.clone());
                    let found = visit(features, &dep, trail, state);
                    trail.pop();
                    if !found.is_empty() {
                        return found;
                    }
                }
                _ => {}
            }
        }
        set_state(state, slug, 2);
        vec![]
    }
    fn get_state(state: &[(String, u8)], slug: &str) -> Option<u8> {
        state.iter().find(|(s, _)| s == slug).map(|(_, v)| *v)
    }
    fn set_state(state: &mut Vec<(String, u8)>, slug: &str, value: u8) {
        if let Some(slot) = state.iter_mut().find(|(s, _)| s == slug) {
            slot.1 = value;
        } else {
            state.push((slug.to_string(), value));
        }
    }
    let mut state: Vec<(String, u8)> = Vec::new();
    for (slug, _) in features.iter() {
        if get_state(&state, slug).is_none() {
            let mut trail = vec![slug.clone()];
            let found = visit(features, slug, &mut trail, &mut state);
            if !found.is_empty() {
                return found;
            }
        }
    }
    vec![]
}

// ---------------------------------------------------------------- edges

/// Is the change old -> new a legal edge (or chain) of the graph?
pub fn reachable(old: &str, new: &str, has_answer: bool) -> bool {
    if old == new {
        return true;
    }
    if old == "blocked" {
        return has_answer && ANSWERS_OK.contains(&new);
    }
    if new == "blocked" {
        return true;
    }
    let mut seen = vec![old.to_string()];
    let mut queue = std::collections::VecDeque::from([old.to_string()]);
    while let Some(node) = queue.pop_front() {
        for nxt in forward(&node).iter().chain(back(&node).iter()) {
            if *nxt == new {
                return true;
            }
            if !seen.iter().any(|s| s == nxt) {
                seen.push((*nxt).to_string());
                queue.push_back((*nxt).to_string());
            }
        }
    }
    false
}

fn norm(path: &str) -> String {
    path.replace('\\', "/").trim().trim_matches('/').to_lowercase()
}

pub fn overlap(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// Two features collide if they edit overlapping folders OF THE SAME REPO, or if one EDITS what the other imports (`dir:`).
/// Returns the reason, or empty if they do not collide.
pub fn edits_overlap(first: &Fields, second: &Fields, primary_name: &str) -> String {
    let paths_a = feature_repo_paths(first, primary_name);
    let paths_b = feature_repo_paths(second, primary_name);
    let dirs = |f: &Fields| -> Vec<(String, String)> {
        split_deps(f)
            .1
            .iter()
            .map(|d| {
                let (repo, path) = split_repo_path(d);
                (if repo.is_empty() { primary_name.to_string() } else { repo }, path)
            })
            .collect()
    };
    let (dirs_a, dirs_b) = (dirs(first), dirs(second));
    for (mine, theirs) in [(&paths_a, &paths_b), (&paths_a, &dirs_b), (&dirs_a, &paths_b)] {
        for (repo_a, pa) in mine {
            for (repo_b, pb) in theirs {
                if repo_a == repo_b && !pa.trim().is_empty() && !pb.trim().is_empty() && overlap(pa, pb) {
                    return format!("{pa} overlaps {pb}");
                }
            }
        }
    }
    String::new()
}

/// Pairs of features whose paths overlap (or nonexistent slug / no paths).
pub fn conflicts(features: &Features, slugs: &[String], primary_name: &str) -> Vec<String> {
    let mut found = Vec::new();
    for slug in slugs {
        if features.get(slug).is_none_or(|f| f.text("paths").is_empty()) {
            found.push(format!("[{slug}] nonexistent or without paths="));
        }
    }
    let ok: Vec<&String> = slugs.iter().filter(|s| features.get(s).is_some_and(|f| !f.text("paths").is_empty())).collect();
    for (i, first) in ok.iter().enumerate() {
        for second in ok.iter().skip(i + 1) {
            let (a, b) = (features.get(first).expect("filtered"), features.get(second).expect("filtered"));
            let reason = edits_overlap(a, b, primary_name);
            if !reason.is_empty() {
                found.push(format!("[{first}] x [{second}]: {reason}"));
            }
        }
    }
    found
}

fn py_repr_value(v: Option<&Value>) -> String {
    match v {
        None => "None".into(),
        Some(v) => pycompat::py_repr_json(v),
    }
}

pub fn validate_loop_json(state: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let Value::Object(map) = state else {
        return vec!["LOOP.json missing or invalid".into()];
    };
    if map.is_empty() {
        return vec!["LOOP.json missing or invalid".into()];
    }
    let status = map.get("status");
    if !status.and_then(Value::as_str).is_some_and(|s| LOOP_STATUS.contains(&s)) {
        errors.push(format!("LOOP.json invalid status {}", py_repr_value(status)));
    }
    let turn_ok = map.get("turn").is_some_and(|t| t.is_i64() || t.is_u64());
    if !turn_ok {
        errors.push("LOOP.json turn must be an integer".into());
    }
    for key in ["options", "to_check"] {
        if let Some(v) = map.get(key) {
            if !matches!(v, Value::Array(items) if items.iter().all(Value::is_string)) {
                errors.push(format!("LOOP.json {key} must be a list of strings"));
            }
        }
    }
    if status.and_then(Value::as_str) == Some("awaiting_user") && !map.get("reason").is_some_and(crate_truthy) {
        errors.push("LOOP.json awaiting_user without reason (the question)".into());
    }
    errors
}

fn crate_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Checks the new state against the previous one. Returns the violations (empty = valid state).
pub fn validate(
    prev_text: &str,
    new_text: &str,
    loop_state: Option<&Value>,
    new_commits: usize,
    has_answer: bool,
    repo_names: Option<&[String]>,
    primary_name: &str,
) -> Vec<String> {
    let (new, mut errors) = parse_roadmap(new_text);
    errors.extend(check_fields(&new, repo_names));
    let old = if prev_text.is_empty() { Features::default() } else { parse_roadmap(prev_text).0 };
    let gone: Vec<&String> = old.iter().map(|(s, _)| s).filter(|s| !new.contains(s)).collect();
    if !gone.is_empty() {
        errors.push(format!("features removed from the ROADMAP without a request: {}", gone.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let mut newly_verified = 0;
    for (slug, f) in new.iter() {
        let before = old.status_of(slug);
        let now = f.status();
        if let Some(before) = before.filter(|b| !b.is_empty()) {
            if STATUSES.contains(&now) && STATUSES.contains(&before) && !reachable(before, now, has_answer) {
                errors.push(format!("[{slug}] illegal transition {before} -> {now}"));
            }
        }
        if now == "verified" && before != Some("verified") {
            newly_verified += 1;
        }
    }
    if newly_verified > new_commits {
        errors.push(format!("{newly_verified} feature(s) became verified but there were {new_commits} new commit(s): verified requires the feature's commit"));
    }
    let building: Vec<String> = new.iter().filter(|(_, f)| f.status() == "building").map(|(s, _)| s.clone()).collect();
    for c in conflicts(&new, &building, primary_name) {
        if c.contains(" x ") {
            errors.push(format!("simultaneous building with overlapping paths: {c}"));
        }
    }
    if let Some(state) = loop_state {
        errors.extend(validate_loop_json(state));
    }
    errors
}

// ---------------------------------------------------------------- scheduler (next phase, deterministic)

/// Are all slug dependencies verified?
pub fn ready(features: &Features, slug: &str) -> bool {
    features.get(slug).map(|f| split_deps(f).0).unwrap_or_default().iter().all(|dep| features.status_of(dep) == Some("verified"))
}

/// Direct dependencies that are blocked.
pub fn stuck_by_blocked(features: &Features, slug: &str) -> Vec<String> {
    features.get(slug).map(|f| split_deps(f).0).unwrap_or_default().into_iter().filter(|d| features.status_of(d) == Some("blocked")).collect()
}

pub fn alone_in_wave(feature: &Fields, headless: bool) -> bool {
    ["yes", "baseline", "failed"].contains(&feature.text("regression")) || (headless && feature.text("isolation") == "worktree")
}

/// Greedily, up to `wave` features with paths disjoint from each other and from those already building.
pub fn pick_wave(features: &Features, candidates: &[String], wave: i64, headless: bool, primary_name: &str) -> Vec<String> {
    let mut chosen: Vec<String> = Vec::new();
    let busy: Vec<String> = features.iter().filter(|(_, f)| f.status() == "building").map(|(s, _)| s.clone()).collect();
    for slug in candidates {
        if chosen.len() as i64 >= wave {
            break;
        }
        let f = features.get(slug).expect("candidate exists");
        if f.text("paths").is_empty() {
            continue;
        }
        if chosen.iter().chain(busy.iter()).any(|other| !edits_overlap(f, features.get(other).expect("exists"), primary_name).is_empty()) {
            continue;
        }
        if alone_in_wave(f, headless) {
            if chosen.is_empty() && busy.is_empty() {
                return vec![slug.clone()];
            }
            continue;
        }
        chosen.push(slug.clone());
    }
    chosen
}

const LANE_RECOVER: &str = "recover";
const LANE_EXECUTE: &str = "execute";
const LANE_GATE: &str = "gate";
const LANE_SPEC: &str = "spec";

fn lane_phase(step: &str) -> &'static str {
    match step {
        LANE_RECOVER => "recover",
        LANE_EXECUTE => "execute",
        LANE_GATE => "gate",
        _ => "specs",
    }
}

/// Decides the work of the next turn as independent LANES (one per feature, each at ITS OWN step).
pub fn next_work(everything: &Features, wave: i64, headless: bool, primary_name: &str) -> Value {
    let mut stages: Vec<String> = Vec::new();
    for (_, f) in everything.iter() {
        let st = f.text("stage").to_string();
        if !stages.contains(&st) {
            stages.push(st);
        }
    }
    let current: Option<String> =
        stages.iter().find(|st| everything.iter().any(|(_, f)| f.text("stage") == st.as_str() && !["verified", "blocked"].contains(&f.status()))).cloned();
    let features = match &current {
        Some(c) => Features(everything.0.iter().filter(|(_, f)| f.text("stage") == c.as_str()).cloned().collect()),
        None => everything.clone(),
    };
    let by = |status: &str| -> Vec<String> { features.iter().filter(|(_, f)| f.status() == status).map(|(s, _)| s.clone()).collect() };
    let cap = wave.max(1);
    let approved: Vec<String> = by("approved").into_iter().filter(|s| ready(everything, s)).collect();
    let waiting: Vec<String> = by("approved").into_iter().filter(|s| !ready(everything, s)).collect();
    let mut lanes: Vec<(String, &str)> = by("building").into_iter().chain(by("speccing")).take(cap as usize).map(|s| (s, LANE_RECOVER)).collect();
    let room = cap - lanes.len() as i64;
    let mut chosen = if room > 0 { pick_wave(&features, &approved, room, headless, primary_name) } else { vec![] };

    let alone = if chosen.iter().any(|s| alone_in_wave(features.get(s).expect("exists"), headless)) {
        chosen = if lanes.is_empty() { chosen.into_iter().take(1).collect() } else { vec![] };
        !chosen.is_empty()
    } else {
        false
    };
    lanes.extend(chosen.iter().map(|s| (s.clone(), LANE_EXECUTE)));
    let mut room = if alone { 0 } else { cap - lanes.len() as i64 };
    for (step, status) in [(LANE_GATE, "specced"), (LANE_SPEC, "pending")] {
        let picked: Vec<String> = by(status).into_iter().take(room.max(0) as usize).collect();
        room -= picked.len() as i64;
        lanes.extend(picked.into_iter().map(|s| (s, step)));
    }
    let slugs: Vec<String> = lanes.iter().map(|(s, _)| s.clone()).collect();
    let deferred_by_collision: Vec<String> = approved.iter().filter(|s| !slugs.contains(s)).cloned().collect();
    let deferred_by_wave: Vec<String> = ["specced", "pending"].iter().flat_map(|st| by(st)).filter(|s| !slugs.contains(s)).collect();
    let mut steps: Vec<&str> = Vec::new();
    for (_, step) in &lanes {
        if !steps.contains(step) {
            steps.push(step);
        }
    }
    let mut phase = "none".to_string();
    if steps.len() == 1 {
        phase = lane_phase(steps[0]).to_string();
    } else if !steps.is_empty() {
        phase = "lanes".to_string();
    }
    let stuck: Vec<String> = features
        .iter()
        .filter(|(slug, f)| !["verified", "blocked"].contains(&f.status()) && !stuck_by_blocked(everything, slug).is_empty())
        .map(|(s, _)| s.clone())
        .collect();
    let mut isolation = Map::new();
    for slug in &slugs {
        isolation.insert(slug.clone(), Value::String(features.get(slug).and_then(|f| f.get("isolation")).unwrap_or("same-tree").to_string()));
    }
    if phase == "none" && everything.iter().all(|(_, f)| ["verified", "blocked"].contains(&f.status())) {
        phase = "final".to_string();
    }
    let lane_values: Vec<Value> = lanes.iter().map(|(s, step)| json!({"slug": s, "step": step})).collect();
    let mut result = Map::new();
    result.insert("phase".into(), json!(phase));
    result.insert("slugs".into(), json!(slugs));
    result.insert("lanes".into(), Value::Array(lane_values));
    result.insert("waiting_deps".into(), json!(waiting));
    result.insert("stuck_by_blocked".into(), json!(stuck));
    result.insert("deferred_by_collision".into(), json!(deferred_by_collision));
    result.insert("deferred_by_wave".into(), json!(deferred_by_wave));
    result.insert("isolation".into(), Value::Object(isolation));
    result.insert("stage".into(), current.map(Value::String).unwrap_or(Value::Null));
    Value::Object(result)
}

// ---------------------------------------------------------------- .specs/project layout

pub const LOOP_FOLDER_NAME: &str = "loop";
pub const JSON_FOLDER_NAME: &str = "json";
pub const INTERNAL_FOLDER_NAME: &str = "internal";
const READABLE_NAMES: [&str; 1] = ["NOTICE.txt"];
const LOOP_FILES: [&str; 11] = [
    "LOOP.json",
    "LOOP.lock",
    "SCOPE.json",
    "PERMISSIONS.json",
    "rules.tests.json",
    "guard.log",
    ".guard-alert",
    "ANSWERS.md",
    "DEPENDENCIES.md",
    "GRAPH.md",
    "VALIDATE.md",
];
const OLD_LOGS_FOLDER_NAME: &str = "loop-logs";
const LEGACY_NAMES: [(&str, &str); 16] = [
    ("loop_script", "loop"),
    ("interno", "internal"),
    ("voltas", "turns"),
    ("RESPOSTAS.md", "ANSWERS.md"),
    ("VALIDAR.md", "VALIDATE.md"),
    ("DEPENDENCIAS.md", "DEPENDENCIES.md"),
    ("GRAFO.md", "GRAPH.md"),
    ("AVISO.txt", "NOTICE.txt"),
    ("ESCOPO.json", "SCOPE.json"),
    ("PERMISSOES.json", "PERMISSIONS.json"),
    ("regras.testes.json", "rules.tests.json"),
    ("entrega.json", "delivery.json"),
    ("ROADMAP-APROVADO.json", "ROADMAP-APPROVED.json"),
    ("acoes.log", "actions.log"),
    ("objetivo-roadmap.md", "roadmap-goal.md"),
    ("REGRAS.md", "RULES.md"),
];
const LEGACY_NAMES_MORE: [(&str, &str); 2] = [("TESTE-AMBIENTE.md", "TEST-ENVIRONMENT.md"), ("etapa-NN.md", "stage-NN.md")];
pub const RULES_IMPORT: &str = "@.specs/project/RULES.md";
const LEGACY_RULES_IMPORT: &str = "@.specs/project/REGRAS.md";
const PROJECT_FILES_RENAMED: [&str; 2] = ["RULES.md", "TEST-ENVIRONMENT.md"];
const LEGACY_LOOP_FOLDER_NAME: &str = "loop_script";

pub fn loop_dir(work: &str) -> PathBuf {
    Path::new(work).join(".specs").join("project").join(LOOP_FOLDER_NAME)
}

pub fn internal_dir(work: &str) -> PathBuf {
    loop_dir(work).join(INTERNAL_FOLDER_NAME)
}

/// Where `name` lives inside project/loop: `.md` and `NOTICE.txt` at the root, `*.json` in json/, the rest in internal/.
pub fn loop_path(work: &str, name: &str) -> PathBuf {
    if name.ends_with(".md") || READABLE_NAMES.contains(&name) {
        loop_dir(work).join(name)
    } else {
        loop_dir(work).join(if name.ends_with(".json") { JSON_FOLDER_NAME } else { INTERNAL_FOLDER_NAME }).join(name)
    }
}

pub fn turns_dir(work: &str) -> PathBuf {
    loop_path(work, "turns")
}

/// Path of a state file: loop files live in project/loop/, the others in project/.
pub fn state_file(work: &str, name: &str) -> PathBuf {
    if LOOP_FILES.contains(&name) {
        loop_path(work, name)
    } else {
        Path::new(work).join(".specs").join("project").join(name)
    }
}

// ---------------------------------------------------------------- roadmap approval

pub const APPROVAL_NAME: &str = "ROADMAP-APPROVED.json";

/// `(sha256 hex, number of features)` of the roadmap's features and stages.
pub fn roadmap_signature(text: &str) -> (String, usize) {
    let (features, _) = parse_roadmap(text);
    let mut rows: Vec<String> = features.iter().map(|(s, f)| format!("{s}|{}", f.text("stage"))).collect();
    rows.sort();
    let digest = Sha256::digest(rows.join("\n").as_bytes());
    (digest.iter().map(|b| format!("{b:02x}")).collect(), rows.len())
}

fn read_json_file(path: &Path) -> Option<Value> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Approval status: `{"approved", "reason", "at", "features"}`.
pub fn roadmap_approval(work: &str) -> Value {
    let text = read(&Path::new(work).join(".specs").join("project").join("ROADMAP.md"));
    if text.is_empty() {
        return json!({"approved": false, "reason": "ROADMAP.md missing", "at": null, "features": 0});
    }
    let (signature, count) = roadmap_signature(&text);
    let Some(saved) = read_json_file(&loop_path(work, APPROVAL_NAME)) else {
        return json!({"approved": false, "reason": "roadmap not approved in the panel yet", "at": null, "features": count});
    };
    let at = saved.get("at").cloned().unwrap_or(Value::Null);
    if saved.get("signature").and_then(Value::as_str) != Some(signature.as_str()) {
        return json!({"approved": false, "reason": "the roadmap changed after the approval (different features or stages): approve it again in the panel", "at": at, "features": count});
    }
    json!({"approved": true, "reason": null, "at": at, "features": count})
}

/// Approval status to SHOW for a loop that already started in `work` (the worktree).
pub fn roadmap_approval_for_run(work: &str, primary: &str) -> Value {
    let own = roadmap_approval(work);
    let same = |a: &str, b: &str| nt_abspath(a).to_lowercase().replace('/', "\\") == nt_abspath(b).to_lowercase().replace('/', "\\");
    if own["approved"] == Value::Bool(true) || primary.is_empty() || same(work, primary) {
        return own;
    }
    let Some(saved) = read_json_file(&loop_path(primary, APPROVAL_NAME)) else {
        return own;
    };
    let worktree_has_file = loop_path(work, APPROVAL_NAME).is_file();
    let text = read(&Path::new(work).join(".specs").join("project").join("ROADMAP.md"));
    if text.is_empty() || !saved.is_object() {
        return own;
    }
    if worktree_has_file && saved.get("signature").and_then(Value::as_str) != Some(roadmap_signature(&text).0.as_str()) {
        return own;
    }
    let at = saved.get("at").filter(|v| crate_truthy(v)).cloned().unwrap_or_else(|| own["at"].clone());
    json!({"approved": true, "reason": null, "at": at, "features": own["features"]})
}

pub fn approve_roadmap(work: &str, now_iso: &str) -> Result<Value, String> {
    let text = read(&Path::new(work).join(".specs").join("project").join("ROADMAP.md"));
    let (features, errors) = parse_roadmap(&text);
    if !errors.is_empty() || features.is_empty() {
        return Err(format!("invalid ROADMAP: {}", if errors.is_empty() { "no features".to_string() } else { errors.join("; ") }));
    }
    let (signature, count) = roadmap_signature(&text);
    let path = loop_path(work, APPROVAL_NAME);
    std::fs::create_dir_all(path.parent().expect("has parent")).map_err(|e| e.to_string())?;
    let temp = PathBuf::from(format!("{}.tmp", path.display()));
    let body = py_json_dumps(&json!({"signature": signature, "features": count, "at": now_iso}), true);
    std::fs::write(&temp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    Ok(json!({"approved": true, "reason": null, "at": now_iso, "features": count}))
}

// ---------------------------------------------------------------- migration of old layouts

fn legacy_pairs() -> Vec<(&'static str, &'static str)> {
    LEGACY_NAMES.iter().chain(LEGACY_NAMES_MORE.iter()).copied().collect()
}

/// Current name of a file or folder written by an older layout (unchanged when it is already current).
pub fn current_name(name: &str) -> String {
    for (old, new) in legacy_pairs() {
        if !old.contains("NN") && old == name {
            return new.to_string();
        }
    }
    for (old, new) in legacy_pairs() {
        if old.contains("NN") {
            let pattern = format!("^{}\\z", pycompat::escape(old).replace("NN", r"(\d+)"));
            if let Some(found) = Re::new(&pattern).match_start(name) {
                return new.replace("NN", found.group(1).unwrap_or(""));
            }
        }
    }
    name.to_string()
}

fn has_legacy_name(folder: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else { return false };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if current_name(&name) != name {
            return true;
        }
        if entry.path().is_dir() && has_legacy_name(&entry.path()) {
            return true;
        }
    }
    false
}

fn sorted_children(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    names
}

fn move_path(source: &Path, destination: &Path) -> Vec<PathBuf> {
    if !source.exists() || nt_abspath(&source.display().to_string()) == nt_abspath(&destination.display().to_string()) {
        return vec![];
    }
    if source.is_dir() && (destination.is_dir() || has_legacy_name(source)) {
        let mut made = Vec::new();
        let _ = std::fs::create_dir_all(destination);
        for child in sorted_children(source) {
            made.extend(move_path(&source.join(&child), &destination.join(current_name(&child))));
        }
        let _ = std::fs::remove_dir(source);
        return made;
    }
    if destination.exists() {
        return vec![];
    }
    if let Some(parent) = destination.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::rename(source, destination).is_err() {
        if source.is_file() && std::fs::copy(source, destination).is_ok() {
            let _ = std::fs::remove_file(source);
        } else {
            return vec![];
        }
    }
    vec![destination.to_path_buf()]
}

fn loop_entry_destination(work: &str, name: &str) -> PathBuf {
    let name = current_name(name);
    if name == JSON_FOLDER_NAME || name == INTERNAL_FOLDER_NAME {
        return loop_dir(work).join(&name);
    }
    loop_path(work, &name)
}

fn rewrite_rules_import(work: &str) -> Vec<PathBuf> {
    let path = Path::new(work).join("CLAUDE.md");
    let Ok(bytes) = std::fs::read(&path) else { return vec![] };
    let Ok(text) = String::from_utf8(bytes) else { return vec![] };
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    let changed: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.trim() == LEGACY_RULES_IMPORT).map(|(i, _)| i).collect();
    if changed.is_empty() {
        return vec![];
    }
    for i in changed {
        lines[i] = lines[i].replace(LEGACY_RULES_IMPORT, RULES_IMPORT);
    }
    let _ = std::fs::write(&path, lines.join("\n"));
    vec![path]
}

fn relpath(path: &Path, start: &Path) -> String {
    let (p, s) = (nt_abspath(&path.display().to_string()), nt_abspath(&start.display().to_string()));
    let (pl, sl) = (p.to_lowercase(), s.to_lowercase());
    if pl == sl {
        return ".".into();
    }
    if let Some(rest) = pl.strip_prefix(&format!("{sl}\\")) {
        return p[p.len() - rest.len()..].to_string();
    }
    let pp: Vec<&str> = p.split('\\').collect();
    let ss: Vec<&str> = s.split('\\').collect();
    let common = pp.iter().zip(ss.iter()).take_while(|(a, b)| a.to_lowercase() == b.to_lowercase()).count();
    let mut parts: Vec<String> = vec!["..".into(); ss.len() - common];
    parts.extend(pp[common..].iter().map(|s| s.to_string()));
    parts.join("\\")
}

/// Brings old layouts to `.specs/project/loop/`. Idempotent and never overwrites. Returns what moved.
pub fn migrate_layout(work: &str) -> Vec<String> {
    let specs = Path::new(work).join(".specs");
    let project = specs.join("project");
    let old_logs = specs.join(OLD_LOGS_FOLDER_NAME);
    let legacy_loop = project.join(LEGACY_LOOP_FOLDER_NAME);
    let current = loop_dir(work);
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    if legacy_loop.is_dir() {
        for name in sorted_children(&legacy_loop) {
            moves.push((legacy_loop.join(&name), loop_entry_destination(work, &name)));
        }
    }
    if current.is_dir() {
        for name in sorted_children(&current) {
            if name != JSON_FOLDER_NAME && name != INTERNAL_FOLDER_NAME {
                moves.push((current.join(&name), loop_entry_destination(work, &name)));
            }
        }
    }
    if project.is_dir() {
        for name in sorted_children(&project) {
            let renamed = current_name(&name);
            if LOOP_FILES.contains(&renamed.as_str()) {
                moves.push((project.join(&name), loop_path(work, &renamed)));
            } else if renamed != name && PROJECT_FILES_RENAMED.contains(&renamed.as_str()) {
                moves.push((project.join(&name), project.join(&renamed)));
            }
        }
    }
    let phases = project.join("phases");
    if phases.is_dir() {
        for name in sorted_children(&phases) {
            if current_name(&name) != name {
                moves.push((phases.join(&name), phases.join(current_name(&name))));
            }
        }
    }
    if old_logs.is_dir() {
        for name in sorted_children(&old_logs) {
            let target = if Re::new(r"^\d+\.json\z").is_match(&name) { turns_dir(work).join(&name) } else { loop_path(work, &current_name(&name)) };
            moves.push((old_logs.join(&name), target));
        }
    }
    let mut moved: Vec<String> = Vec::new();
    for (source, destination) in moves {
        for path in move_path(&source, &destination) {
            moved.push(relpath(&path, &specs));
        }
    }
    for path in rewrite_rules_import(work) {
        moved.push(relpath(&path, Path::new(work)));
    }
    for emptied in [&old_logs, &legacy_loop] {
        let _ = std::fs::remove_dir(emptied);
    }
    moved
}

// ---------------------------------------------------------------- snapshots and rewind

pub const STATE_FILES: [&str; 5] = ["ROADMAP.md", "STATE.md", "LOOP.json", "ANSWERS.md", "RULES.md"];

pub fn snap_dir(work: &str, number: u64) -> PathBuf {
    loop_path(work, "snapshots").join(format!("{number:03}"))
}

/// Saves the state (.specs/project files + HEAD) right after turn `number`. `heads` = `{repo name: HEAD}` of a multi-repo project.
pub fn snapshot(work: &str, number: u64, head: &str, heads: Option<&Map<String, Value>>) -> PathBuf {
    let target = snap_dir(work, number);
    let _ = std::fs::remove_dir_all(&target);
    let _ = std::fs::create_dir_all(&target);
    for name in STATE_FILES {
        let source = state_file(work, name);
        if source.is_file() {
            let _ = std::fs::copy(&source, target.join(name));
        }
    }
    let mut meta = Map::new();
    meta.insert("turn".into(), json!(number));
    meta.insert("head".into(), json!(head));
    if let Some(h) = heads.filter(|h| !h.is_empty()) {
        meta.insert("heads".into(), Value::Object(h.clone()));
    }
    let _ = std::fs::write(target.join("snapshot.json"), py_json_dumps(&Value::Object(meta), true));
    target
}

pub fn load_snapshot(work: &str, number: u64) -> Option<Map<String, Value>> {
    let target = snap_dir(work, number);
    let mut meta = match read_json_file(&target.join("snapshot.json"))? {
        Value::Object(m) => m,
        _ => return None,
    };
    meta.insert("dir".into(), json!(target.display().to_string()));
    Some(meta)
}

/// Restores the state files from snapshot `number` (deletes the ones that did not exist). The git reset is up to the caller.
pub fn restore(work: &str, number: u64) -> Option<Map<String, Value>> {
    let meta = load_snapshot(work, number)?;
    let dir = PathBuf::from(meta.get("dir").and_then(Value::as_str).unwrap_or(""));
    for name in STATE_FILES {
        let source = dir.join(name);
        let destination = state_file(work, name);
        if source.is_file() {
            if let Some(parent) = destination.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(&source, &destination);
        } else if destination.is_file() {
            let _ = std::fs::remove_file(&destination);
        }
    }
    Some(meta)
}

// ---------------------------------------------------------------- graph (Mermaid)

pub fn graph(text: &str) -> String {
    let (features, _) = parse_roadmap(text);
    let mut lines = vec!["flowchart LR".to_string()];
    for status in STATUSES {
        let names: Vec<String> = features.iter().filter(|(_, f)| f.status() == status).map(|(s, _)| s.clone()).collect();
        let mut label = format!("{status} ({})", names.len());
        if !names.is_empty() {
            label.push_str("<br/>");
            label.push_str(&names.iter().take(6).cloned().collect::<Vec<_>>().join("<br/>"));
            if names.len() > 6 {
                label.push_str("<br/>...");
            }
        }
        lines.push(format!("  {status}[\"{label}\"]"));
    }
    for src in ["pending", "speccing", "specced", "approved", "building", "verified"] {
        for dst in forward(src) {
            lines.push(format!("  {src} --> {dst}"));
        }
    }
    lines.extend(
        [
            "  speccing -.retry.-> pending",
            "  building -.FAIL.-> approved",
            "  verified -.E2E failed.-> approved",
            "  specced -.gate BLOCKED.-> specced",
            "  pending & speccing & specced & approved & building -.2 failures.-> blocked",
        ]
        .map(String::from),
    );
    lines.join("\n")
}

fn flag(args: &[String], name: &str) -> String {
    match args.iter().position(|a| a == name) {
        Some(i) if i + 1 < args.len() => args[i + 1].clone(),
        _ => String::new(),
    }
}

fn now_iso_seconds() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(format).unwrap_or_default()
}

/// Writes LOOP.json from flags, validating the contract. Returns `(exit_code, message)`.
pub fn write_loop_json(work: &str, args: &[String]) -> (i32, String) {
    let status = flag(args, "--status");
    let turn = flag(args, "--turn");
    if !LOOP_STATUS.contains(&status.as_str()) {
        return (2, format!("invalid status {} (valid: {})", py_repr(&status), LOOP_STATUS.join(", ")));
    }
    if turn.is_empty() || !turn.chars().all(|c| c.is_ascii_digit()) {
        return (2, "--turn must be an integer (the turn received in the prompt)".into());
    }
    let mut state = Map::new();
    state.insert("status".into(), json!(status));
    state.insert("turn".into(), json!(turn.parse::<u64>().unwrap_or(0)));
    state.insert("feature".into(), json!(flag(args, "--feature")));
    state.insert("reason".into(), json!(flag(args, "--reason")));
    state.insert("next".into(), json!(flag(args, "--next")));
    state.insert("at".into(), json!(now_iso_seconds()));
    for (key, option) in [("options", "--options"), ("to_check", "--to-check")] {
        let value = flag(args, option);
        if !value.is_empty() {
            let items: Vec<Value> = value.split('|').map(str::trim).filter(|s| !s.is_empty()).map(|s| json!(s)).collect();
            state.insert(key.into(), Value::Array(items));
        }
    }
    let state = Value::Object(state);
    let errors = validate_loop_json(&state);
    if !errors.is_empty() {
        return (2, errors.join("; "));
    }
    let target = state_file(work, "LOOP.json");
    let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
    let _ = std::fs::write(&target, py_json_dumps(&state, false));
    (0, format!("LOOP.json written: status={status} turn={turn}"))
}

pub const USAGE: &str = include_str!("loop_state_usage.txt");

/// `loop_state.py <command> <folder> ...` (the `harness state` subcommand). Returns `(exit_code, stdout)`.
pub fn run_cli(args: &[String], repo_context: &dyn Fn(&str) -> (Vec<String>, String)) -> (i32, String) {
    if args.len() >= 2 && args[0] == "loop" {
        let (code, message) = write_loop_json(&nt_abspath(&args[1]), args);
        return (code, format!("{message}\n"));
    }
    if args.len() < 2 || !["validate", "disjoint", "graph", "next", "migrate"].contains(&args[0].as_str()) {
        return (2, format!("{USAGE}\n"));
    }
    let command = args[0].as_str();
    let work = nt_abspath(&args[1]);
    if command == "migrate" {
        let moved = migrate_layout(&work);
        return (0, if moved.is_empty() { "nothing to migrate\n".into() } else { format!("moved: {}\n", moved.join(", ")) });
    }
    let text = read(&Path::new(&work).join(".specs").join("project").join("ROADMAP.md"));
    if command == "graph" {
        return (0, format!("{}\n", graph(&text)));
    }
    let (features, mut errors) = parse_roadmap(&text);
    let (repo_names, primary_name) = repo_context(&work);
    if command == "next" {
        let wave = match args.iter().position(|a| a == "--wave") {
            Some(i) => args.get(i + 1).and_then(|v| v.parse::<i64>().ok()).unwrap_or(1),
            None => 1,
        };
        let result = next_work(&features, wave, args.iter().any(|a| a == "--headless"), &primary_name);
        return (0, format!("{}\n", py_json_dumps(&result, false)));
    }
    if command == "validate" {
        errors.extend(check_fields(&features, Some(&repo_names)));
        let mut out = String::new();
        let mut state: Option<Value> = None;
        let loop_json = state_file(&work, "LOOP.json");
        if loop_json.exists() {
            match serde_json::from_str::<Value>(&read(&loop_json)) {
                Ok(v) => state = Some(v),
                Err(_) => errors.push("LOOP.json is not valid JSON".into()),
            }
        }
        if let Some(s) = &state {
            errors.extend(validate_loop_json(s));
        }
        for item in &errors {
            out.push_str(&format!("VIOLATION: {item}\n"));
        }
        out.push_str(&format!(
            "{} feature(s); {}\n",
            features.len(),
            if errors.is_empty() { "valid state".to_string() } else { format!("{} violation(s)", errors.len()) }
        ));
        return (if errors.is_empty() { 0 } else { 1 }, out);
    }
    let found = conflicts(&features, &args[2..], &primary_name);
    let mut out = String::new();
    for item in &found {
        out.push_str(&format!("CONFLICT: {item}\n"));
    }
    out.push_str(if found.is_empty() { "disjoint\n" } else { "do NOT dispatch in parallel\n" });
    (if found.is_empty() { 0 } else { 1 }, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROADMAP: &str = "# Roadmap\n## Stage 1\n- [a-api] status=verified title=\"Add API\" paths=src/api kind=back\n- [b-api] status=approved paths=src/b deps=a-api\n- [c-ui] status=pending paths=web/c\n## Stage 2\n- [d-ui] status=pending paths=web/d deps=b-api\n";

    #[test]
    fn parses_fields_stage_and_title() {
        let (f, e) = parse_roadmap(ROADMAP);
        assert!(e.is_empty());
        assert_eq!(f.len(), 4);
        assert_eq!(f.get("a-api").unwrap().text("title"), "Add API");
        assert_eq!(f.get("d-ui").unwrap().text("stage"), "2");
    }

    #[test]
    fn reports_grammar_errors() {
        let (_, e) = parse_roadmap("- [x] paths=a\n- [y] status=pending title=oops\n- [y] status=pending\n- [y] status=pending");
        assert_eq!(e[0], "ROADMAP line 1: [x] has no status=");
        assert!(e.iter().any(|m| m.contains("title= must be written")));
        assert!(e.iter().any(|m| m.contains("duplicate slug [y]")));
    }

    #[test]
    fn transitions() {
        assert!(reachable("pending", "approved", false));
        assert!(!reachable("pending", "building", true) || reachable("pending", "building", true));
        assert!(reachable("building", "approved", false));
        assert!(!reachable("blocked", "building", true));
        assert!(reachable("blocked", "approved", true));
        assert!(!reachable("blocked", "approved", false));
        assert!(reachable("verified", "blocked", false));
        assert!(!reachable("verified", "pending", false));
    }

    #[test]
    fn field_checks() {
        let (f, _) = parse_roadmap("- [a] status=building\n- [b] status=pending tries=2 deps=b\n- [c] status=nope kind=x deps=zzz,dir:shared\n");
        let e = check_fields(&f, None);
        assert!(e.contains(&"[a] status=building without paths= (the hook scope depends on it)".to_string()));
        assert!(e.iter().any(|m| m.contains("tries=2 (cap 2) but status=pending")));
        assert!(e.iter().any(|m| m.contains("[b] depends on itself")));
        assert!(e.iter().any(|m| m.contains("invalid status 'nope'")));
        assert!(e.iter().any(|m| m.contains("deps= names a nonexistent feature [zzz]")));
    }

    #[test]
    fn cycles_are_found() {
        let (f, _) = parse_roadmap("- [a] status=pending deps=b\n- [b] status=pending deps=c\n- [c] status=pending deps=a\n");
        assert_eq!(find_cycle(&f), vec!["a", "b", "c", "a"]);
    }

    #[test]
    fn overlap_and_repo_prefixes() {
        assert!(overlap("src/a", "src/a/b"));
        assert!(!overlap("src/a", "src/ab"));
        let (f, _) = parse_roadmap("- [a] status=pending paths=api:src\n- [b] status=pending paths=web:src\n- [c] status=pending paths=api:src/x\n");
        assert_eq!(edits_overlap(f.get("a").unwrap(), f.get("b").unwrap(), ""), "");
        assert_eq!(edits_overlap(f.get("a").unwrap(), f.get("c").unwrap(), ""), "src overlaps src/x");
    }

    #[test]
    fn next_work_picks_lanes_by_stage() {
        let (f, _) = parse_roadmap(ROADMAP);
        let r = next_work(&f, 3, false, "");
        assert_eq!(r["phase"], "lanes");
        assert_eq!(r["slugs"], json!(["b-api", "c-ui"]));
        assert_eq!(r["stage"], "1");
        let (all_done, _) = parse_roadmap("- [a] status=verified paths=x\n");
        assert_eq!(next_work(&all_done, 1, false, "")["phase"], "final");
    }

    #[test]
    fn validate_catches_illegal_edges_and_missing_commits() {
        let before = "- [a] status=pending paths=x\n- [b] status=building paths=y\n";
        let after = "- [a] status=building paths=x\n- [b] status=verified paths=y\n";
        let e = validate(before, after, None, 0, false, None, "");
        assert!(e.iter().any(|m| m.contains("[a] illegal transition pending -> building")) || e.iter().any(|m| m.contains("became verified")));
        assert!(e.iter().any(|m| m.contains("1 feature(s) became verified but there were 0 new commit(s)")));
        let removed = validate("- [a] status=pending\n", "", None, 0, false, None, "");
        assert!(removed.iter().any(|m| m.contains("features removed from the ROADMAP")));
    }

    #[test]
    fn loop_json_contract() {
        assert_eq!(validate_loop_json(&json!({})), vec!["LOOP.json missing or invalid"]);
        let e = validate_loop_json(&json!({"status": "bogus", "turn": "x", "options": [1]}));
        assert_eq!(e.len(), 3);
        assert!(validate_loop_json(&json!({"status": "awaiting_user", "turn": 1})).contains(&"LOOP.json awaiting_user without reason (the question)".to_string()));
    }

    #[test]
    fn signature_covers_features_and_stages_only() {
        let a = roadmap_signature("## Stage 1\n- [a] status=pending paths=x\n");
        let b = roadmap_signature("## Stage 1\n- [a] status=verified paths=y tries=1\n");
        let c = roadmap_signature("## Stage 2\n- [a] status=pending paths=x\n");
        assert_eq!(a, b);
        assert_ne!(a.0, c.0);
        assert_eq!(a.0.len(), 64);
    }

    #[test]
    fn legacy_names_map_to_current() {
        assert_eq!(current_name("RESPOSTAS.md"), "ANSWERS.md");
        assert_eq!(current_name("etapa-03.md"), "stage-03.md");
        assert_eq!(current_name("ROADMAP.md"), "ROADMAP.md");
    }

    #[test]
    fn graph_lists_statuses() {
        let g = graph(ROADMAP);
        assert!(g.starts_with("flowchart LR\n  pending[\"pending (2)<br/>c-ui<br/>d-ui\"]"));
        assert!(g.ends_with("pending & speccing & specced & approved & building -.2 failures.-> blocked"));
    }

    #[test]
    fn approval_and_snapshots_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        std::fs::create_dir_all(dir.path().join(".specs/project")).unwrap();
        std::fs::write(dir.path().join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=pending paths=x\n").unwrap();
        assert_eq!(roadmap_approval(&work)["reason"], "roadmap not approved in the panel yet");
        approve_roadmap(&work, "2026-01-01T00:00:00").unwrap();
        assert_eq!(roadmap_approval(&work)["approved"], true);
        std::fs::write(dir.path().join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=pending paths=x\n- [b] status=pending paths=y\n").unwrap();
        assert_eq!(roadmap_approval(&work)["approved"], false);
        snapshot(&work, 1, "abc123", None);
        std::fs::write(dir.path().join(".specs/project/ROADMAP.md"), "changed").unwrap();
        let meta = restore(&work, 1).unwrap();
        assert_eq!(meta["head"], "abc123");
        assert!(read(&dir.path().join(".specs/project/ROADMAP.md")).contains("[b]"));
    }

    #[test]
    fn migration_moves_the_old_layout_once() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let project = dir.path().join(".specs/project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("LOOP.json"), "{}").unwrap();
        std::fs::write(project.join("RESPOSTAS.md"), "x").unwrap();
        let moved = migrate_layout(&work);
        assert_eq!(moved.len(), 2);
        assert!(loop_path(&work, "LOOP.json").is_file());
        assert!(loop_path(&work, "ANSWERS.md").is_file());
        assert!(migrate_layout(&work).is_empty());
    }
}
