// SPDX-License-Identifier: MIT
//! The E2E gate measures evidence, not labels (backlog G6, G9, C4, VA1-VA6, V1-V4, Z1-Z4).
//!
//! A feature line may carry, and the feature's `E2E.md` (`.specs/features/<slug>/E2E.md`) may repeat as `key: value` lines:
//!
//! | ROADMAP field | E2E.md line | meaning |
//! |---|---|---|
//! | `evidence=a,b` | `evidence: a, b` | paths (inside the project) of result/log artifacts of the run |
//! | `no-surface=why_words` | `no-surface: why` | the stated reason for `e2e=not-covered` (`_` is a space in the field) |
//! | `blocked-call=call` | `blocked-call: call` | the call the guard refused, when a layer could not run |
//! | `needs-credential=NAME` | `credential: NAME` | credential NAMES the layer needs (never a value) |
//! | `credential-owner=who` | `credential-owner: who` | who must give it |
//! | | `if-declined: text` | what happens if the user declines |
//!
//! Rules (all conservative, all pluggable through [`EvidenceRule`] and [`SurfaceConfig`]):
//! * `e2e=ok` needs evidence: at least one listed artifact that exists, is a file and is not empty.
//! * `e2e=not-covered` needs a stated reason AND proof of no surface: the kind is one of `no_surface_kinds` (default `generic` only), the
//!   feature has `paths=`, and no path touches a surface marker (routes, controllers, pages, screens, components, entrypoints...).
//! * A recorded blocked call or a missing credential stops the loop with `awaiting_user` before the label is looked at; the stop offers
//!   "continue without this layer". That choice (read back from ANSWERS.md) moves the layer to the manual list.

use crate::credential_check;
use crate::loop_io::{load_json, read, save_json};
use crate::loop_state::{feature_dir, feature_folder_name, loop_path, parse_roadmap, state_file, Fields};
use crate::result_vocabulary::E2eResult;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The option that lets the engine go on without a layer (written to ANSWERS.md when the user picks it).
pub const SKIP_LAYER_OPTION: &str = "continue without this layer";
/// Appended to a stop reason so the answer can be tied back to the feature: `[layer-stop:<slug>]`.
const STOP_MARK: &str = "[layer-stop:";
const MIN_REASON_WORDS: usize = 3;

pub const DEFAULT_SURFACE_MARKERS: &[&str] = &[
    "route",
    "routes",
    "controller",
    "controllers",
    "api",
    "endpoint",
    "endpoints",
    "handler",
    "handlers",
    "page",
    "pages",
    "view",
    "views",
    "screen",
    "screens",
    "component",
    "components",
    "ui",
    "frontend",
    "web",
    "server",
    "cli",
    "bin",
    "cmd",
    "main",
    "program",
    "startup",
    "app",
    "tsx",
    "jsx",
    "vue",
    "svelte",
    "html",
    "cshtml",
];

// ---------------------------------------------------------------- inputs

/// What the feature's E2E.md says (all optional).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct E2eFile {
    pub evidence: Vec<String>,
    pub no_surface: String,
    pub blocked_call: String,
    pub credentials: Vec<String>,
    pub owner: String,
    pub if_declined: String,
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',').map(|p| p.trim().trim_matches('`').to_string()).filter(|p| !p.is_empty()).collect()
}

/// A credential NAME: letters, digits and underscore only. Anything else (a pasted value, a token with dashes) is dropped, so a value
/// written by mistake can never be echoed into a stop message.
fn valid_credential_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && name.chars().any(|c| c.is_ascii_alphabetic())
}

pub fn parse_e2e_file(text: &str) -> E2eFile {
    let mut file = E2eFile::default();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '>']).trim();
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "evidence" => file.evidence.extend(split_list(value)),
            "no-surface" => file.no_surface = value.to_string(),
            "blocked-call" => file.blocked_call = value.to_string(),
            "credential" => file.credentials.extend(split_list(value).into_iter().filter(|n| valid_credential_name(n))),
            "credential-owner" => file.owner = value.to_string(),
            "if-declined" => file.if_declined = value.to_string(),
            _ => {}
        }
    }
    file
}

/// Everything the rules look at for one feature.
#[derive(Clone, Debug)]
pub struct FeatureInput {
    pub work: String,
    pub slug: String,
    pub fields: Fields,
    pub file: E2eFile,
}

impl FeatureInput {
    pub fn load(work: &str, slug: &str, fields: &Fields) -> Self {
        let text = read(&Path::new(&feature_dir(work, slug)).join("E2E.md"));
        FeatureInput { work: work.to_string(), slug: slug.to_string(), fields: fields.clone(), file: parse_e2e_file(&text) }
    }
    pub fn result(&self) -> Option<E2eResult> {
        self.fields.get("e2e").and_then(E2eResult::parse)
    }
    pub fn evidence(&self) -> Vec<String> {
        let mut all = split_list(self.fields.text("evidence"));
        for item in &self.file.evidence {
            if !all.contains(item) {
                all.push(item.clone());
            }
        }
        all
    }
    pub fn no_surface_reason(&self) -> String {
        let field = self.fields.text("no-surface").replace('_', " ");
        if field.trim().is_empty() {
            self.file.no_surface.trim().to_string()
        } else {
            field.trim().to_string()
        }
    }
    pub fn blocked_call(&self) -> String {
        let field = self.fields.text("blocked-call").replace('_', " ");
        if field.trim().is_empty() {
            self.file.blocked_call.trim().to_string()
        } else {
            field.trim().to_string()
        }
    }
    pub fn credentials(&self) -> Vec<String> {
        let mut all: Vec<String> = split_list(self.fields.text("needs-credential")).into_iter().filter(|n| valid_credential_name(n)).collect();
        for name in &self.file.credentials {
            if !all.contains(name) {
                all.push(name.clone());
            }
        }
        all
    }
    pub fn layers(&self) -> Vec<(&'static str, &'static str)> {
        let api = ("api", "routes answer with the expected status, payload and authorization");
        let ui = ("ui", "screens open and the user flow works in the running app");
        match self.fields.text("kind").to_lowercase().as_str() {
            "back" => vec![api],
            "front" => vec![ui],
            "both" => vec![api, ui],
            _ => vec![("run", "the feature runs end to end in the running app")],
        }
    }
    fn e2e_file_hint(&self) -> String {
        format!(".specs/features/{}/E2E.md", feature_folder_name(&self.slug))
    }
}

// ---------------------------------------------------------------- pluggable evidence rule

/// One check an `e2e=ok` must pass. Implement it to plug in a stack-specific proof (a junit file with no failures, a HAR file...).
pub trait EvidenceRule {
    fn name(&self) -> &'static str;
    /// `Err(what is missing)` is shown verbatim in the stop message.
    fn check(&self, input: &FeatureInput) -> Result<(), String>;
}

/// Default rule: at least one listed artifact exists inside the project, is a file and is not empty. Does not read the content (what
/// counts as proof for a given stack is an open decision).
pub struct ArtifactExists;

impl EvidenceRule for ArtifactExists {
    fn name(&self) -> &'static str {
        "artifact-exists"
    }
    fn check(&self, input: &FeatureInput) -> Result<(), String> {
        let listed = input.evidence();
        if listed.is_empty() {
            return Err(format!(
                "no evidence listed: add `evidence=<path to a result/log artifact>` to its ROADMAP line or an `evidence: <path>` line to {}",
                input.e2e_file_hint()
            ));
        }
        for item in &listed {
            let path = resolve_inside(&input.work, item).ok_or_else(|| format!("evidence `{item}` must be a path inside the project (no `..`, no URL)"))?;
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() && meta.len() > 0 => {}
                Ok(meta) if meta.is_file() => return Err(format!("evidence file is empty: {item}")),
                _ => return Err(format!("evidence file not found: {item}")),
            }
        }
        Ok(())
    }
}

pub fn default_rules() -> Vec<Box<dyn EvidenceRule>> {
    vec![Box::new(ArtifactExists)]
}

fn resolve_inside(work: &str, item: &str) -> Option<PathBuf> {
    if item.contains("://") || Path::new(item).components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    let path = Path::new(item);
    if path.is_absolute() {
        return path.starts_with(work).then(|| path.to_path_buf());
    }
    Some(Path::new(work).join(path))
}

// ---------------------------------------------------------------- proof of "no surface"

/// Data-driven definition of "has a surface". Default is the const list above and `generic` only; a project overrides it in
/// `.specs/project/loop/json/EVIDENCE.json` (`{"surface_markers": [...], "no_surface_kinds": [...]}`).
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceConfig {
    pub markers: Vec<String>,
    pub no_surface_kinds: Vec<String>,
}

impl Default for SurfaceConfig {
    fn default() -> Self {
        SurfaceConfig { markers: DEFAULT_SURFACE_MARKERS.iter().map(|m| m.to_string()).collect(), no_surface_kinds: vec!["generic".into()] }
    }
}

impl SurfaceConfig {
    pub fn load(work: &str) -> Self {
        let data = load_json(&loop_path(work, "EVIDENCE.json"));
        let list = |key: &str| -> Option<Vec<String>> {
            let items: Vec<String> = data.get(key)?.as_array()?.iter().filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase())).collect();
            (!items.is_empty()).then_some(items)
        };
        let base = SurfaceConfig::default();
        SurfaceConfig { markers: list("surface_markers").unwrap_or(base.markers), no_surface_kinds: list("no_surface_kinds").unwrap_or(base.no_surface_kinds) }
    }

    /// The marker a path touches, if any (compared with the path's words: `src/api/users.ts` -> `api`).
    pub fn marker_in(&self, path: &str) -> Option<&str> {
        let words: Vec<String> = path.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_ascii_lowercase).collect();
        self.markers.iter().find(|m| words.iter().any(|w| w == *m)).map(String::as_str)
    }
}

/// `Ok` only when the feature has a stated reason AND the engine can see no surface. `Err(what is missing)` otherwise.
pub fn no_surface_proof(input: &FeatureInput, config: &SurfaceConfig) -> Result<(), String> {
    let reason = input.no_surface_reason();
    if reason.split_whitespace().count() < MIN_REASON_WORDS {
        return Err(format!(
            "not-covered needs a stated reason (at least {MIN_REASON_WORDS} words): add `no-surface=<why_words>` to its ROADMAP line or a `no-surface: <why>` line to {}",
            input.e2e_file_hint()
        ));
    }
    let kind = input.fields.text("kind").to_lowercase();
    if !config.no_surface_kinds.contains(&kind) {
        let shown = if kind.is_empty() { "(missing)".to_string() } else { kind };
        return Err(format!("kind={shown} may have a runtime surface; not-covered is accepted only for kind in [{}]", config.no_surface_kinds.join(", ")));
    }
    let paths = split_list(input.fields.text("paths"));
    if paths.is_empty() {
        return Err("no `paths=` on its ROADMAP line, so the engine cannot prove the feature has no surface".into());
    }
    for path in &paths {
        if let Some(marker) = config.marker_in(path) {
            return Err(format!("path `{path}` touches the surface marker `{marker}` (routes, UI or entrypoints)"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- user decision: continue without a layer

/// Features whose layer the user chose to skip (`User answer: continue without this layer` to a question tagged `[layer-stop:<slug>]`).
/// `specs` is the `.specs/project` folder.
pub fn skipped_features(specs: &str) -> Vec<String> {
    answered_with(specs, STOP_MARK, SKIP_LAYER_OPTION)
}

/// Slugs tagged `<mark><slug>]` in a question of ANSWERS.md whose user answer starts with `option` (case-insensitive).
fn answered_with(specs: &str, mark: &str, option: &str) -> Vec<String> {
    let text = read(&Path::new(specs).join("ANSWERS.md"));
    let option = option.to_ascii_lowercase();
    let mut found = Vec::new();
    for block in text.split("\n## Answer ") {
        let Some(start) = block.find(mark) else { continue };
        let rest = &block[start + mark.len()..];
        let Some(end) = rest.find(']') else { continue };
        let answer = block.lines().find_map(|l| l.trim().strip_prefix("User answer:")).unwrap_or("").trim().to_ascii_lowercase();
        let slug = rest[..end].to_string();
        if answer.starts_with(&option) && !found.contains(&slug) {
            found.push(slug);
        }
    }
    found
}

/// The option that confirms a validation checkpoint (backlog V3).
pub const VALIDATED_OPTION: &str = "I validated it, go on";
/// The option that leaves the checkpoint on the manual list and lets the loop close (backlog V3).
pub const VALIDATE_LATER_OPTION: &str = "I will validate it later, close without it";
/// Appended to a validation stop reason so the answer can be tied back to the feature: `[validate-stop:<slug>]`.
const VALIDATE_MARK: &str = "[validate-stop:";

/// Features whose validation checkpoint the user confirmed.
pub fn validated_features(specs: &str) -> Vec<String> {
    let mut all = answered_with(specs, VALIDATE_MARK, VALIDATED_OPTION);
    for slug in answered_with(specs, VALIDATE_MARK, VALIDATE_LATER_OPTION) {
        if !all.contains(&slug) {
            all.push(slug);
        }
    }
    all
}

// ---------------------------------------------------------------- stops that wait for the user

/// The kind of a stop, so the panel and the outside notice can tell a wanted pause from an error or a question.
pub const KIND_BLOCKED: &str = "blocked_by_access";
pub const KIND_CREDENTIAL: &str = "credential";
pub const KIND_VALIDATION: &str = "validation_checkpoint";

/// A stop that waits for the user: a credential decision, a blocked call or a validation checkpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct AwaitingStop {
    /// One of [`KIND_BLOCKED`], [`KIND_CREDENTIAL`], [`KIND_VALIDATION`].
    pub kind: &'static str,
    pub feature: String,
    pub reason: String,
    pub options: Vec<String>,
    pub next: String,
    /// Present for a credential decision: `{name, feature, who, if_declined, options}`. Never a value.
    pub credential_request: Option<Value>,
}

fn credential_is_set(work: &str, name: &str) -> bool {
    credential_check::read_project_values(work).get(name).is_some_and(|v| !v.trim().is_empty())
}

/// The first stop the user must decide before the E2E can close, or `None`. Checked BEFORE the label: a blocked layer is never "covered".
/// Only verified features count; a feature the user already chose to skip does not stop again.
pub fn awaiting_stop(work: &str, specs: &str) -> Option<AwaitingStop> {
    let skipped = skipped_features(specs);
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    for (slug, fields) in features.iter() {
        if fields.status() != "verified" || skipped.contains(slug) {
            continue;
        }
        let input = FeatureInput::load(work, slug, fields);
        let blocked = input.blocked_call();
        if !blocked.is_empty() {
            let reason = format!(
                "E2E blocked by access on {slug}: the check could not run because this call was refused: {blocked}. The layer is NOT covered. Release that call with a permission rule and re-run the check, or choose to continue without this layer (it goes to the manual list). {STOP_MARK}{slug}]"
            );
            return Some(AwaitingStop {
                kind: KIND_BLOCKED,
                feature: slug.clone(),
                reason,
                options: vec!["I released the call, run the check again".into(), SKIP_LAYER_OPTION.into()],
                next: format!("if released, re-run the E2E of {slug}; if skipped, mark it e2e=not-covered and keep the layer on the manual list"),
                credential_request: None,
            });
        }
        let missing: Vec<String> = input.credentials().into_iter().filter(|n| !credential_is_set(work, n)).collect();
        if !missing.is_empty() && input.result() != Some(E2eResult::Ok) {
            let names = missing.join(", ");
            let who = if input.file.owner.trim().is_empty() && input.fields.text("credential-owner").is_empty() {
                "the person who manages the test environment (the spec does not name them)".to_string()
            } else if input.fields.text("credential-owner").is_empty() {
                input.file.owner.trim().to_string()
            } else {
                input.fields.text("credential-owner").replace('_', " ")
            };
            let declined = if input.file.if_declined.trim().is_empty() {
                format!("the layer of {slug} that needs it is not validated and goes to the manual list")
            } else {
                input.file.if_declined.trim().to_string()
            };
            let reason = format!(
                "credential decision: {slug} needs {names} (names only) and it is not set in .specs/project/.env.test. Who gives it: {who}. If you decline: {declined}. Add the value to .env.test yourself (never in chat or in a spec) or continue without this layer. {STOP_MARK}{slug}]"
            );
            let request = json!({"name": names, "names": missing, "feature": slug, "who": who, "if_declined": declined,
                "options": [format!("I added {names} to .specs/project/.env.test, check again"), SKIP_LAYER_OPTION]});
            return Some(AwaitingStop {
                kind: KIND_CREDENTIAL,
                feature: slug.clone(),
                reason,
                options: vec![format!("I added {names} to .specs/project/.env.test, check again"), SKIP_LAYER_OPTION.into()],
                next: format!("if provided, run the E2E of {slug}; if skipped, mark it e2e=not-covered and keep the layer on the manual list"),
                credential_request: Some(request),
            });
        }
    }
    // V3: a deliberate pause. Last, so every feature that does not depend on the checkpointed one has already been worked on.
    let validated = validated_features(specs);
    for (slug, fields) in features.iter() {
        if fields.status() != "verified" || !fields.has("checkpoint") || validated.contains(slug) {
            continue;
        }
        let what = fields.text("checkpoint").replace('_', " ");
        let reason = format!(
            "Waiting for your validation: feature {slug}. Do this: {what}. This is a planned pause, not an error: everything that does not depend on {slug} is already done. {VALIDATE_MARK}{slug}]"
        );
        return Some(AwaitingStop {
            kind: KIND_VALIDATION,
            feature: slug.clone(),
            reason,
            options: vec![VALIDATED_OPTION.into(), VALIDATE_LATER_OPTION.into()],
            next: format!("after you confirm, the loop closes the E2E of {slug}; if you validate later, it stays on the manual list"),
            credential_request: None,
        });
    }
    None
}

/// Turns the stop into the LOOP.json the panel and `--answer` read (status `awaiting_user`).
pub fn write_stop(work: &str, stop: &AwaitingStop) {
    let path = state_file(work, "LOOP.json");
    let mut state = load_json(&path);
    if let Some(map) = state.as_object_mut() {
        map.insert("status".into(), json!("awaiting_user"));
        map.insert("reason".into(), json!(stop.reason));
        map.insert("feature".into(), json!(stop.feature));
        map.insert("options".into(), json!(stop.options));
        map.insert("next".into(), json!(stop.next));
        map.insert("pause_kind".into(), json!(stop.kind));
        match &stop.credential_request {
            Some(request) => map.insert("credential_request".into(), request.clone()),
            None => map.shift_remove("credential_request"),
        };
    }
    let _ = save_json(&path, &state);
}

// ---------------------------------------------------------------- the gate: evidence, not labels

/// Problems with the E2E results that are marked: `ok` without evidence, `not-covered` without proof of no surface. Verified features
/// only; a skipped layer is exempt (the user decided). Empty = the labels are backed.
pub fn evidence_problems_with(work: &str, specs: &str, rules: &[Box<dyn EvidenceRule>], config: &SurfaceConfig) -> Vec<String> {
    let skipped = skipped_features(specs);
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    let mut problems = Vec::new();
    for (slug, fields) in features.iter() {
        if fields.status() != "verified" || skipped.contains(slug) {
            continue;
        }
        let input = FeatureInput::load(work, slug, fields);
        match input.result() {
            Some(E2eResult::Ok) => {
                for rule in rules {
                    if let Err(missing) = rule.check(&input) {
                        problems.push(format!("{slug}: e2e=ok is not backed ({}): {missing}", rule.name()));
                    }
                }
            }
            Some(E2eResult::NotCovered) => {
                if let Err(missing) = no_surface_proof(&input, config) {
                    problems.push(format!("{slug}: e2e=not-covered is not accepted: {missing}"));
                }
            }
            _ => {}
        }
    }
    problems
}

pub fn evidence_problems(work: &str, specs: &str) -> Vec<String> {
    evidence_problems_with(work, specs, &default_rules(), &SurfaceConfig::load(work))
}

/// The stop message when the E2E phase ended with labels the engine does not accept.
pub fn evidence_stop_message(problems: &[String]) -> String {
    format!(
        "E2E phase ended with results that are not backed by evidence: {}. Run the real check and list its artifact (`evidence=`), or give a verifiable no-surface reason; the loop does not accept a label alone.",
        problems.join("; ")
    )
}

// ---------------------------------------------------------------- G7: a result changes only with calls made in this turn

/// Clock slack when comparing a file time with the start of the turn (file systems round their times).
const CLOCK_SLACK: std::time::Duration = std::time::Duration::from_secs(2);

fn touched_since(work: &str, item: &str, since: std::time::SystemTime) -> bool {
    let Some(path) = resolve_inside(work, item) else { return false };
    let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else { return false };
    modified + CLOCK_SLACK >= since
}

/// Features whose `e2e=` became `ok` or `failed` in this turn (it was something else before) without any of their listed evidence being
/// written since `since` (the start of the turn). A turn that made no E2E calls leaves no fresh result file, so it cannot change a result;
/// marking everything at once after a reset fails here. A layer the user chose to skip is exempt. Returns one message per feature.
pub fn changes_without_calls(work: &str, specs: &str, prev_roadmap: &str, since: std::time::SystemTime) -> Vec<String> {
    let skipped = skipped_features(specs);
    let before = parse_roadmap(prev_roadmap).0;
    let after = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    let mut out = Vec::new();
    for (slug, fields) in after.iter() {
        let now = fields.get("e2e").and_then(E2eResult::parse);
        if !matches!(now, Some(E2eResult::Ok) | Some(E2eResult::Failed)) || skipped.contains(slug) {
            continue;
        }
        let was = before.get(slug).and_then(|f| f.get("e2e")).and_then(E2eResult::parse);
        if was == now {
            continue;
        }
        let input = FeatureInput::load(work, slug, fields);
        let evidence = input.evidence();
        if !evidence.iter().any(|item| touched_since(work, item, since)) {
            let word = now.map(E2eResult::roadmap_word).unwrap_or("");
            out.push(format!(
                "{slug}: e2e={word} was written but no listed evidence was produced in this turn, so no E2E call was made for it; run the check and list the result file it writes"
            ));
        }
    }
    out
}

/// The stop message for [`changes_without_calls`].
pub fn changes_without_calls_message(problems: &[String]) -> String {
    format!("E2E results changed without calls in this turn: {}. A result is accepted only with the calls that produced it.", problems.join("; "))
}

// ---------------------------------------------------------------- what is validated, and what stays with the user

/// `[{feature, layer, what, evidence, status}]`. Status: `ok`, `failed`, `pending`, `unproven` (ok without evidence), `not-covered`,
/// `rejected` (not-covered without proof), `blocked`, `skipped-by-user`.
pub fn validations_with(work: &str, specs: &str, rules: &[Box<dyn EvidenceRule>], config: &SurfaceConfig) -> Vec<Value> {
    let skipped = skipped_features(specs);
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    let mut out = Vec::new();
    for (slug, fields) in features.iter() {
        if fields.status() != "verified" && !fields.has("e2e") {
            continue;
        }
        let input = FeatureInput::load(work, slug, fields);
        let status = if skipped.contains(slug) {
            "skipped-by-user"
        } else if !input.blocked_call().is_empty() {
            "blocked"
        } else {
            match input.result() {
                None | Some(E2eResult::Pending) => "pending",
                Some(E2eResult::Failed) => "failed",
                Some(E2eResult::Ok) => {
                    if rules.iter().all(|r| r.check(&input).is_ok()) {
                        "ok"
                    } else {
                        "unproven"
                    }
                }
                Some(E2eResult::NotCovered) => {
                    if no_surface_proof(&input, config).is_ok() {
                        "not-covered"
                    } else {
                        "rejected"
                    }
                }
            }
        };
        for (layer, what) in input.layers() {
            out.push(json!({"feature": slug, "layer": layer, "what": what, "evidence": input.evidence(), "status": status}));
        }
    }
    out
}

pub fn validations(work: &str, specs: &str) -> Vec<Value> {
    validations_with(work, specs, &default_rules(), &SurfaceConfig::load(work))
}

/// What the engine never checks by design, each with the reason it stays manual (Z1-Z4), plus the features the run left to the user.
pub fn manual_checks(work: &str, specs: &str) -> Vec<Value> {
    let item = |feature: Value, what: &str, reason: &str, source: &str| json!({"feature": feature, "what": what, "reason": reason, "source": source});
    let mut list = vec![
        item(Value::Null, "a real e-mail or message arrives at its destination", "the loop is not allowed to send anything outside", "by_design"),
        item(Value::Null, "a scheduled job runs by itself at its time", "it needs wall-clock time outside the run", "by_design"),
        item(
            Value::Null,
            "behaviour against production data or a production environment",
            "the loop works on the test environment only (DEV vs PROD is a decision for you)",
            "by_design",
        ),
        item(Value::Null, "a second real person using the feature, and any product decision", "it needs a person", "by_design"),
    ];
    let skipped = skipped_features(specs);
    let config = SurfaceConfig::load(work);
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    for (slug, fields) in features.iter() {
        if skipped.contains(slug) {
            list.push(item(json!(slug), "the layer you chose to continue without", "you chose to continue without it (credential or blocked call)", "skipped"));
        } else if fields.status() == "verified" && fields.get("e2e").and_then(E2eResult::parse) == Some(E2eResult::NotCovered) {
            let input = FeatureInput::load(work, slug, fields);
            if no_surface_proof(&input, &config).is_ok() {
                list.push(item(json!(slug), "everything about this feature", &format!("accepted as not covered: {}", input.no_surface_reason()), "not_covered"));
            }
        }
        if fields.has("checkpoint") {
            list.push(item(json!(slug), "the checkpoint the feature declares", "an outside effect only a person can confirm", "checkpoint"));
        }
    }
    list.extend(crate::env_cleanup::manual_rows(work)); // Z3: test data the run left in the test environment
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Project {
        _dir: tempfile::TempDir,
        work: String,
        specs: String,
    }

    fn project(roadmap: &str) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let specs_dir = dir.path().join(".specs").join("project");
        std::fs::create_dir_all(&specs_dir).unwrap();
        std::fs::write(specs_dir.join("ROADMAP.md"), roadmap).unwrap();
        let specs = specs_dir.display().to_string();
        Project { _dir: dir, work, specs }
    }

    fn write(p: &Project, relative: &str, text: &str) {
        let path = Path::new(&p.work).join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn ok_passes_with_evidence_and_stops_without() {
        let p = project("## S\n- [a] status=verified kind=back paths=src/api e2e=ok evidence=out/a.log\n- [b] status=verified kind=back paths=src/b e2e=ok\n");
        write(&p, "out/a.log", "PASS 3 cases");
        let problems = evidence_problems(&p.work, &p.specs);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("b: e2e=ok is not backed (artifact-exists): no evidence listed") && problems[0].contains("E2E.md"), "{problems:?}");
    }

    #[test]
    fn ok_evidence_must_exist_be_non_empty_and_stay_inside() {
        let p = project("## S\n- [a] status=verified e2e=ok evidence=out/missing.log\n- [c] status=verified e2e=ok evidence=out/empty.log\n- [d] status=verified e2e=ok evidence=../x.log\n- [e] status=verified e2e=ok evidence=https://x.test/r\n");
        write(&p, "out/empty.log", "");
        let problems = evidence_problems(&p.work, &p.specs).join("\n");
        assert!(problems.contains("evidence file not found: out/missing.log"), "{problems}");
        assert!(problems.contains("evidence file is empty: out/empty.log"), "{problems}");
        assert_eq!(problems.matches("must be a path inside the project").count(), 2, "{problems}");
    }

    #[test]
    fn evidence_can_come_from_the_feature_e2e_file() {
        let p = project("## S\n- [a-b] status=verified e2e=ok\n");
        write(&p, "res/r.json", "{}");
        assert_eq!(evidence_problems(&p.work, &p.specs).len(), 1);
        write(&p, ".specs/features/a-b/E2E.md", "# E2E\n- evidence: res/r.json\n");
        assert!(evidence_problems(&p.work, &p.specs).is_empty());
    }

    #[test]
    fn a_pluggable_rule_can_be_stricter() {
        struct MustMentionPass;
        impl EvidenceRule for MustMentionPass {
            fn name(&self) -> &'static str {
                "mentions-pass"
            }
            fn check(&self, input: &FeatureInput) -> Result<(), String> {
                let text = read(&Path::new(&input.work).join(&input.evidence()[0]));
                text.contains("PASS").then_some(()).ok_or_else(|| "the log does not say PASS".to_string())
            }
        }
        let p = project("## S\n- [a] status=verified e2e=ok evidence=o.log\n");
        write(&p, "o.log", "nothing");
        let rules: Vec<Box<dyn EvidenceRule>> = vec![Box::new(ArtifactExists), Box::new(MustMentionPass)];
        let problems = evidence_problems_with(&p.work, &p.specs, &rules, &SurfaceConfig::default());
        assert_eq!(problems, vec!["a: e2e=ok is not backed (mentions-pass): the log does not say PASS"]);
        write(&p, "o.log", "PASS");
        assert!(evidence_problems_with(&p.work, &p.specs, &rules, &SurfaceConfig::default()).is_empty());
    }

    #[test]
    fn not_covered_passes_only_with_reason_and_no_surface() {
        let p = project("## S\n- [lib] status=verified kind=generic paths=tools/format e2e=not-covered no-surface=pure_formatting_helper_no_entrypoint\n");
        assert!(evidence_problems(&p.work, &p.specs).is_empty());
        assert_eq!(validations(&p.work, &p.specs)[0]["status"], "not-covered");
        assert!(manual_checks(&p.work, &p.specs).iter().any(|m| m["source"] == "not_covered" && m["feature"] == "lib"));
    }

    #[test]
    fn not_covered_is_refused_without_reason_or_with_a_surface() {
        let p = project(
            "## S\n- [a] status=verified kind=generic paths=tools/x e2e=not-covered\n- [b] status=verified kind=back paths=lib/x e2e=not-covered no-surface=only_a_helper_really\n- [c] status=verified kind=generic paths=src/api/users.ts e2e=not-covered no-surface=only_a_helper_really\n- [d] status=verified kind=generic e2e=not-covered no-surface=only_a_helper_really\n- [e] status=verified kind=generic paths=src/login.tsx e2e=not-covered no-surface=only_a_helper_really\n- [f] status=verified e2e=not-covered no-surface=only_a_helper_really\n",
        );
        let problems = evidence_problems(&p.work, &p.specs);
        assert_eq!(problems.len(), 6, "{problems:?}");
        assert!(problems[0].contains("needs a stated reason"));
        assert!(problems[1].contains("kind=back may have a runtime surface"));
        assert!(problems[2].contains("surface marker `api`"));
        assert!(problems[3].contains("cannot prove the feature has no surface"));
        assert!(problems[4].contains("surface marker `tsx`"));
        assert!(problems[5].contains("kind=(missing)"));
        assert!(validations(&p.work, &p.specs).iter().all(|v| v["status"] == "rejected"));
    }

    #[test]
    fn surface_markers_match_words_not_substrings_and_are_configurable() {
        let config = SurfaceConfig::default();
        assert_eq!(config.marker_in("src/Controllers/Users.cs"), Some("controllers"));
        assert_eq!(config.marker_in("tools/rapid-format"), None);
        let p = project("## S\n- [a] status=verified kind=generic paths=jobs/nightly e2e=not-covered no-surface=background_batch_only\n");
        assert!(evidence_problems(&p.work, &p.specs).is_empty());
        write(&p, ".specs/project/loop/json/EVIDENCE.json", r#"{"surface_markers": ["jobs"]}"#);
        assert!(evidence_problems(&p.work, &p.specs)[0].contains("surface marker `jobs`"));
    }

    #[test]
    fn failed_and_pending_are_not_this_gates_business() {
        let p = project("## S\n- [a] status=verified e2e=failed\n- [b] status=verified e2e=pending\n- [c] status=building e2e=ok\n");
        assert!(evidence_problems(&p.work, &p.specs).is_empty());
    }

    #[test]
    fn blocked_call_stops_with_the_call_named_even_when_labelled_covered() {
        let p = project(
            "## S\n- [a] status=verified kind=back paths=src/api e2e=not-covered blocked-call=curl_with_url_in_a_variable\n- [b] status=verified e2e=ok evidence=o.log\n",
        );
        let stop = awaiting_stop(&p.work, &p.specs).expect("must stop");
        assert_eq!(stop.feature, "a");
        assert!(stop.reason.contains("curl with url in a variable") && stop.reason.contains("NOT covered") && stop.reason.contains("[layer-stop:a]"), "{}", stop.reason);
        assert!(stop.options.contains(&SKIP_LAYER_OPTION.to_string()));
        assert!(stop.credential_request.is_none());
        write_stop(&p.work, &stop);
        let state = load_json(&state_file(&p.work, "LOOP.json"));
        assert_eq!((state["status"].as_str(), state["feature"].as_str()), (Some("awaiting_user"), Some("a")));
    }

    #[test]
    fn credential_decision_names_the_name_and_never_a_value() {
        let p = project("## S\n- [a] status=verified kind=front paths=web/a needs-credential=TEST_PORTAL_PASSWORD credential-owner=the_project_admin\n");
        write(&p, ".specs/features/a/E2E.md", "- credential: sk-live-abc123, OTHER_NAME\n- if-declined: the login flow stays unchecked\n");
        let stop = awaiting_stop(&p.work, &p.specs).expect("must stop");
        let request = stop.credential_request.clone().unwrap();
        assert_eq!(request["names"], json!(["TEST_PORTAL_PASSWORD", "OTHER_NAME"]));
        assert_eq!(request["who"], "the project admin");
        assert_eq!(request["if_declined"], "the login flow stays unchecked");
        assert!(stop.reason.contains("TEST_PORTAL_PASSWORD, OTHER_NAME") && stop.reason.contains("Who gives it: the project admin"), "{}", stop.reason);
        assert!(!stop.reason.contains("sk-live") && !request.to_string().contains("sk-live"));
        assert_eq!(stop.options[1], SKIP_LAYER_OPTION);
    }

    #[test]
    fn credential_that_is_set_does_not_stop_and_its_value_is_not_read_out() {
        let p = project("## S\n- [a] status=verified kind=front paths=web/a needs-credential=TEST_PORTAL_PASSWORD\n");
        write(&p, ".specs/project/.env.test", "TEST_PORTAL_PASSWORD=placeholder-not-real\n");
        assert!(awaiting_stop(&p.work, &p.specs).is_none());
        write(&p, ".specs/project/.env.test", "TEST_PORTAL_PASSWORD=\n");
        assert!(awaiting_stop(&p.work, &p.specs).is_some());
    }

    #[test]
    fn continue_without_this_layer_is_read_from_the_answer_and_only_that_answer() {
        let p = project("## S\n- [a] status=verified kind=front paths=web/a e2e=not-covered needs-credential=SOME_NAME\n- [b] status=verified blocked-call=some_call\n");
        let stop = awaiting_stop(&p.work, &p.specs).unwrap();
        let block = |slug: &str, answer: &str| format!("\n## Answer 2026-01-01 [pending]\nQuestion: why {STOP_MARK}{slug}]\nUser answer: {answer}\n");
        write(&p, ".specs/project/ANSWERS.md", &block("a", "I will think about it"));
        assert!(skipped_features(&p.specs).is_empty());
        assert_eq!(awaiting_stop(&p.work, &p.specs).unwrap().feature, stop.feature);
        write(&p, ".specs/project/ANSWERS.md", &(block("a", SKIP_LAYER_OPTION) + &block("b", "Continue without this layer.")));
        assert_eq!(skipped_features(&p.specs), vec!["a", "b"]);
        assert!(awaiting_stop(&p.work, &p.specs).is_none());
        // a skipped layer is exempt from the surface proof and lands on the manual list and the validations
        assert!(evidence_problems(&p.work, &p.specs).is_empty());
        assert!(validations(&p.work, &p.specs).iter().all(|v| v["status"] == "skipped-by-user"));
        assert_eq!(manual_checks(&p.work, &p.specs).iter().filter(|m| m["source"] == "skipped").count(), 2);
    }

    #[test]
    fn validations_list_layers_with_what_evidence_and_status() {
        let p = project("## S\n- [a] status=verified kind=both e2e=ok evidence=o.log\n- [b] status=verified kind=back e2e=ok\n- [c] status=building\n- [d] status=verified kind=back\n");
        write(&p, "o.log", "PASS");
        let v = validations(&p.work, &p.specs);
        let row = |feature: &str, layer: &str| v.iter().find(|r| r["feature"] == feature && r["layer"] == layer).cloned().unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!((row("a", "api")["status"].clone(), row("a", "ui")["status"].clone()), (json!("ok"), json!("ok")));
        assert_eq!(row("a", "ui")["evidence"], json!(["o.log"]));
        assert!(row("a", "api")["what"].as_str().unwrap().contains("routes"));
        assert_eq!(row("b", "api")["status"], "unproven");
        assert_eq!(row("d", "api")["status"], "pending");
    }

    #[test]
    fn manual_list_always_carries_the_by_design_items_with_reasons() {
        let p = project("## S\n- [a] status=verified kind=back checkpoint=send_test_mail e2e=ok evidence=o.log\n");
        let list = manual_checks(&p.work, &p.specs);
        assert!(list.iter().filter(|m| m["source"] == "by_design").count() >= 4);
        assert!(list.iter().all(|m| !m["reason"].as_str().unwrap().is_empty()));
        assert!(list.iter().any(|m| m["source"] == "checkpoint" && m["feature"] == "a"));
    }

    #[test]
    fn a_result_changed_in_a_turn_without_fresh_evidence_is_refused() {
        let before =
            "## S\n- [a] status=verified kind=back e2e=pending\n- [b] status=verified kind=back e2e=ok evidence=old.log\n- [c] status=verified kind=back e2e=pending\n";
        let p = project("## S\n- [a] status=verified kind=back e2e=ok evidence=new.log\n- [b] status=verified kind=back e2e=ok evidence=old.log\n- [c] status=verified kind=back e2e=ok evidence=stale.log\n");
        write(&p, "new.log", "PASS");
        write(&p, "old.log", "PASS");
        write(&p, "stale.log", "PASS");
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        // every file is fresh relative to an old turn start: nothing is refused
        assert!(changes_without_calls(&p.work, &p.specs, before, long_ago).is_empty());
        // the turn started after every file was written: a and c changed to ok with no fresh evidence; b did not change
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        let problems = changes_without_calls(&p.work, &p.specs, before, later);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("a: e2e=ok was written but no listed evidence"));
        assert!(problems[1].starts_with("c: "));
    }

    #[test]
    fn a_skipped_layer_and_an_unchanged_result_are_not_checked_for_calls() {
        let p = project("## S\n- [a] status=verified kind=back e2e=failed\n");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert!(changes_without_calls(&p.work, &p.specs, "## S\n- [a] status=verified kind=back e2e=failed\n", later).is_empty());
        assert_eq!(changes_without_calls(&p.work, &p.specs, "## S\n- [a] status=verified kind=back e2e=pending\n", later).len(), 1);
        write(&p, ".specs/project/ANSWERS.md", &format!("\n## Answer t [pending]\nQuestion: x {STOP_MARK}a]\nUser answer: {SKIP_LAYER_OPTION}\n"));
        assert!(changes_without_calls(&p.work, &p.specs, "## S\n- [a] status=verified kind=back e2e=pending\n", later).is_empty());
    }

    #[test]
    fn a_checkpoint_is_a_deliberate_pause_until_the_user_confirms_it() {
        let p = project(
            "## S
- [a] status=verified kind=back checkpoint=send_a_test_mail e2e=ok evidence=o.log
- [b] status=verified kind=back e2e=ok evidence=o.log
",
        );
        write(&p, "o.log", "PASS");
        let stop = awaiting_stop(&p.work, &p.specs).expect("a checkpoint waits for the user");
        assert_eq!((stop.kind, stop.feature.as_str()), (KIND_VALIDATION, "a"));
        assert!(stop.reason.starts_with("Waiting for your validation: feature a. Do this: send a test mail."), "{}", stop.reason);
        assert!(stop.reason.contains("planned pause, not an error"));
        assert_eq!(stop.options, vec![VALIDATED_OPTION.to_string(), VALIDATE_LATER_OPTION.to_string()]);
        write_stop(&p.work, &stop);
        assert_eq!(load_json(&state_file(&p.work, "LOOP.json"))["pause_kind"], KIND_VALIDATION);
        for option in [VALIDATED_OPTION, VALIDATE_LATER_OPTION] {
            write(
                &p,
                ".specs/project/ANSWERS.md",
                &format!(
                    "
## Answer t [pending]
Question: {} 
User answer: {option}
",
                    stop.reason
                ),
            );
            assert!(awaiting_stop(&p.work, &p.specs).is_none(), "{option}");
        }
    }

    #[test]
    fn a_blocked_call_and_a_credential_keep_their_own_kinds() {
        let p = project(
            "## S
- [a] status=verified kind=back blocked-call=curl_x
",
        );
        assert_eq!(awaiting_stop(&p.work, &p.specs).unwrap().kind, KIND_BLOCKED);
        let q = project(
            "## S
- [a] status=verified kind=back needs-credential=SOME_NAME
",
        );
        assert_eq!(awaiting_stop(&q.work, &q.specs).unwrap().kind, KIND_CREDENTIAL);
    }
}
