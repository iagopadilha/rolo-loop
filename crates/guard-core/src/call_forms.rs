// SPDX-License-Identifier: MIT
//! The call forms the guard accepts, as DATA: the words the guard's own messages are built from, the accepted alternative for each refusal, and
//! the two texts made from them (the refusal explainer the guard prints, and the "How the E2E must call things" block of the plugin texts).
//!
//! Nothing here decides what is blocked. The guard rules (`net.rs`, `check.rs`, `scope.rs`) build their reasons from the constants below and
//! [`explain`] only reads those reasons back, so text and guard cannot drift apart: a test runs every example of [`forms`] through the real
//! rules, and another compares the generated block with the plugin files.

/// Suffixes of a `.env.test` variable name the guard expands when a command uses it as a target (`API_URL`, `WEB_HOST`, `ADMIN_BASE`).
pub const EXPANDABLE_SUFFIXES: [&str; 3] = ["URL", "HOST", "BASE"];
/// Letters of the python flags that skip the preload the loop's egress layer depends on (`-S`, `-E`, `-I`).
pub const PYTHON_SKIP_FLAGS: &str = "SEI";
pub const PYTHON_SKIP_REASON: &str = "skips site/sitecustomize or the PYTHON* variables the loop's egress preload depends on";
pub const NO_TARGET_URL: &str = "no target URL found";
pub const SUBSHELL_TARGET: &str = "the target comes from a sub-shell";
pub const VARIABLE_NOT_NAMED: &str = "is a variable that is not a";
pub const RUN_CODE_UNREADABLE: &str = "cannot be read, so it cannot be proven not to change the network filter";
pub const OUTSIDE_WORKTREES: &str = "outside the project's worktrees";
/// The words a wrapper function passes on as the target, which the guard cannot read as a URL.
pub const POSITIONAL_TARGETS: [&str; 4] = ["$1", "$2", "$@", "$*"];

/// `*_URL` / `*_HOST` / `*_BASE` joined by `separator` (the guard writes it with `/` in one message and ` / ` in another).
pub fn variable_names(separator: &str) -> String {
    EXPANDABLE_SUFFIXES.iter().map(|s| format!("*_{s}")).collect::<Vec<_>>().join(separator)
}

/// One accepted form: what the guard refuses, what it accepts instead, and whether the refusal is only about the way the call is written.
#[derive(Clone, Debug)]
pub struct CallForm {
    pub id: &'static str,
    /// The guard rule that refuses (empty: a rule of Claude Code itself, not of the guard).
    pub rule: &'static str,
    /// Pieces of the guard's reason that identify this refusal (all from the constants above).
    pub triggers: Vec<String>,
    pub refused: String,
    pub accepted: String,
    /// A command that must pass / must be refused by the real rules (empty: no example, the form is guidance only).
    pub accepted_example: &'static str,
    pub refused_example: &'static str,
    /// `true`: the same request is accepted when written in the accepted form. `false`: refused on purpose, nothing to ask for.
    pub call_form: bool,
}

pub fn forms() -> Vec<CallForm> {
    let names = variable_names(" / ");
    let skip: Vec<String> = PYTHON_SKIP_FLAGS.chars().map(|c| format!("-{c}")).collect();
    vec![
        CallForm {
            id: "url-target",
            rule: "egress-host",
            triggers: vec![NO_TARGET_URL.into(), SUBSHELL_TARGET.into(), VARIABLE_NOT_NAMED.into()],
            refused: "a `curl` whose URL is built in a sub-shell, read from a file or held in a variable with another name".into(),
            accepted: format!(
                "write the URL in the command (`curl http://localhost:5000/health`) or use a variable named `{names}` that is defined in `.specs/project/.env.test` (`curl $API_URL/health`)"
            ),
            accepted_example: "curl $API_URL/health",
            refused_example: "curl $TARGET/health",
            call_form: true,
        },
        CallForm {
            id: "no-wrapper",
            rule: "egress-host",
            triggers: POSITIONAL_TARGETS.iter().map(|w| format!("{w} is not in this project's network list")).collect(),
            refused: "a wrapper function or script that forwards its arguments (`call() { curl \"$@\"; }`, `$*`), because the URL is no longer in the command".into(),
            accepted: "call `curl` directly in every command, with the URL or the variable written in that command".into(),
            accepted_example: "curl http://api.example.com/x",
            refused_example: "call() { curl \"$1\"; }; call http://api.example.com/x",
            call_form: true,
        },
        CallForm {
            id: "python-isolated",
            rule: "egress-tamper",
            triggers: vec![PYTHON_SKIP_REASON.into()],
            refused: format!("`python {}`: they skip the preload the loop's network layer depends on", skip.join("`, `python ")),
            accepted: "run the script as `python script.py` (or `python -c '...'`) without those flags".into(),
            accepted_example: "python script.py",
            refused_example: "python -I script.py",
            call_form: false,
        },
        CallForm {
            id: "browser-script",
            rule: "egress-tamper",
            triggers: vec![RUN_CODE_UNREADABLE.into()],
            refused: "`playwright-cli run-code --filename x.js` when the file does not exist yet (written in the same call), because the guard must read it first".into(),
            accepted: "write the script file in one call and run it in a LATER call (`playwright-cli run-code --filename=<worktree>/e2e/step.js`), or pass the code inline".into(),
            accepted_example: "playwright-cli run-code --filename=step.js",
            refused_example: "playwright-cli run-code --filename=missing-step.js",
            call_form: true,
        },
        CallForm {
            id: "inside-worktree",
            rule: "repo-only",
            triggers: vec![OUTSIDE_WORKTREES.into()],
            refused: "a write, or a git command, from a folder that is not one of the loop's worktrees (for example the parent folder of the worktrees)".into(),
            accepted: "run from inside the worktree and keep the evidence and scripts in it".into(),
            accepted_example: "",
            refused_example: "",
            call_form: true,
        },
        CallForm {
            id: "token-not-inline",
            rule: "",
            triggers: vec![],
            refused: "a token or password written in the command (`curl -H \"Authorization: Bearer eyJ...\"`)".into(),
            accepted: "let the test runner set it: `\"<TOOLKIT>/bin/craft.exe\" testenv exec --project <root> '<command>'` puts it in an environment variable, and the command sends `-H \"Authorization: Bearer $JWT\"`; never write the value".into(),
            accepted_example: "",
            refused_example: "",
            call_form: true,
        },
    ]
}

/// The explanation appended to a refusal, or an empty string when no form matches the blocked reasons. It names the rule, the accepted form, and says
/// whether this is a call form (rewrite the call) or refused on purpose. It only reads the reasons; it never changes what is blocked.
pub fn explain(blocked: &[(String, String)]) -> String {
    let all = forms();
    let mut lines: Vec<String> = Vec::new();
    for (rule, reason) in blocked {
        let Some(form) = all.iter().find(|f| f.rule == rule && f.triggers.iter().any(|t| reason.contains(t.as_str()))) else {
            continue;
        };
        let verdict = if form.call_form {
            "This is a call form, not a missing permission: the same request is accepted when it is written like this, and that is a new call, not a workaround."
        } else {
            "This is refused on purpose, not a call form and not a missing permission: there is nothing to ask for; use the form above."
        };
        let line = format!("[{rule}] {} Accepted form: {}. {verdict}", capitalise(&form.refused), form.accepted);
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("What to do instead: {}\n", lines.join(" | "))
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("Refused: {}{}.", first.to_lowercase(), chars.as_str().trim_end_matches('.')),
        None => String::new(),
    }
}

pub const BLOCK_BEGIN: &str = "<!-- BEGIN call-forms (generated from the guard's rule data by `cargo test -p guard-core call_forms`; do not edit by hand) -->";
pub const BLOCK_END: &str = "<!-- END call-forms -->";

/// The "How the E2E must call things (forms the guard accepts)" block, between the markers, ready to paste into a plugin text.
pub fn render_block() -> String {
    let mut out = String::new();
    out.push_str(BLOCK_BEGIN);
    out.push_str("\n### How the E2E must call things (forms the guard accepts)\n\n");
    out.push_str(
        "Most guard refusals in an E2E run are about HOW a call is written, not about a missing permission. Write each call in the form below the first time.\n\n",
    );
    for form in forms() {
        let kind = if form.call_form { "call form" } else { "refused on purpose" };
        let rule = if form.rule.is_empty() { "Claude Code".to_string() } else { format!("`{}`", form.rule) };
        out.push_str(&format!("- **Not this** ({kind}, {rule}): {}.\n  **This:** {}.\n", form.refused, form.accepted));
    }
    out.push_str("- **Network list:** only `localhost` and the hosts of `.specs/project/NETWORK.json` are reachable; a host that is not listed is `UNVERIFIED` with the host named in `NOTES`, never a reason to try another client.\n");
    out.push_str(BLOCK_END);
    out.push('\n');
    out
}

/// Replaces the text between the markers of `text` with [`render_block`]; `None` when the markers are not there.
pub fn replace_block(text: &str) -> Option<String> {
    let start = text.find(BLOCK_BEGIN)?;
    let end = text[start..].find(BLOCK_END)? + start + BLOCK_END.len();
    let tail = text[end..].strip_prefix('\n').unwrap_or(&text[end..]);
    Some(format!("{}{}{}", &text[..start], render_block(), tail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{command_hits, Ctx, RULES};
    use crate::paths::{normalize_path, G};
    use cfg::Env;

    const PLUGIN_FILES: [&str; 2] = ["plugin/agents/verifier-flow.md", "plugin/skills/orchestrator/SKILL.md"];

    struct Project {
        _dir: tempfile::TempDir,
        root: String,
        g: G,
    }

    fn project() -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        std::fs::create_dir_all(dir.path().join(".specs/project")).unwrap();
        std::fs::write(dir.path().join(".specs/project/NETWORK.json"), r#"{"allow": ["api.example.com"]}"#).unwrap();
        std::fs::write(dir.path().join(".specs/project/.env.test"), "API_URL=https://api.example.com\n").unwrap();
        std::fs::write(dir.path().join("step.js"), "async page => { await page.goto('https://api.example.com'); }\n").unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let g = G::new(Env::from_pairs([("LOOP_DATA_DIR", data.display().to_string()), ("USERPROFILE", dir.path().display().to_string())]));
        Project { _dir: dir, root, g }
    }

    fn hits(p: &Project, command: &str) -> Vec<(String, String)> {
        let root = normalize_path(&p.g, &p.root, None, None);
        let mut ctx = Ctx::new(&p.g, &root, vec![root.clone()], Some(&root));
        command_hits(&mut ctx, command, &RULES)
    }

    #[test]
    fn every_accepted_example_passes_and_every_refused_one_is_explained() {
        let p = project();
        for form in forms() {
            if !form.accepted_example.is_empty() {
                assert!(hits(&p, form.accepted_example).is_empty(), "{}: the accepted example is refused: {:?}", form.id, hits(&p, form.accepted_example));
            }
            if !form.refused_example.is_empty() {
                let refused = hits(&p, form.refused_example);
                assert!(!refused.is_empty(), "{}: the refused example passes", form.id);
                assert!(refused.iter().all(|(r, _)| *r == form.rule), "{}: {refused:?}", form.id);
                let text = explain(&refused);
                assert!(!text.is_empty(), "{}: a refusal with no explanation: {refused:?}", form.id);
                if !form.triggers.is_empty() {
                    assert!(text.contains(&form.accepted), "{}: the explainer does not name the accepted form: {text}", form.id);
                    assert_eq!(text.contains("not a missing permission: the same request"), form.call_form, "{}", form.id);
                }
            }
        }
    }

    #[test]
    fn the_variable_suffixes_are_the_ones_the_guard_expands() {
        let p = project();
        for suffix in EXPANDABLE_SUFFIXES {
            std::fs::write(std::path::Path::new(&p.root).join(".specs/project/.env.test"), format!("SVC_{suffix}=https://api.example.com\n")).unwrap();
            assert!(hits(&p, &format!("curl $SVC_{suffix}/x")).is_empty(), "{suffix}");
        }
        std::fs::write(std::path::Path::new(&p.root).join(".specs/project/.env.test"), "SVC_ADDR=https://api.example.com\n").unwrap();
        assert!(!hits(&p, "curl $SVC_ADDR/x").is_empty());
    }

    #[test]
    fn the_python_flags_listed_are_the_ones_refused() {
        let p = project();
        for flag in PYTHON_SKIP_FLAGS.chars() {
            assert!(hits(&p, &format!("python -{flag} script.py")).iter().any(|(r, _)| r == "egress-tamper"), "-{flag}");
        }
        assert!(hits(&p, "python script.py").is_empty());
    }

    #[test]
    fn a_reason_with_no_matching_form_is_not_explained() {
        assert_eq!(explain(&[("egress-host".into(), "GET evil.example.org/ is not in this project's network list".into())]), "");
        assert_eq!(explain(&[("secrets".into(), "credential path".into())]), "");
    }

    #[test]
    fn the_repo_only_reasons_carry_the_trigger() {
        let reasons =
            [format!("file {OUTSIDE_WORKTREES}: only repository code"), format!("git commit {OUTSIDE_WORKTREES} (x): the repo of its -C/cwd must be one of them")];
        for reason in reasons {
            let text = explain(&[("repo-only".into(), reason)]);
            assert!(text.contains("inside the worktree"), "{text}");
        }
    }

    #[test]
    fn the_explainer_does_not_change_what_is_blocked() {
        let p = project();
        let before = hits(&p, "curl $TARGET/x");
        let _ = explain(&before);
        assert_eq!(before, hits(&p, "curl $TARGET/x"));
    }

    fn repo_file(relative: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(relative)
    }

    #[test]
    fn the_plugin_texts_carry_the_block_generated_from_the_rule_data() {
        let update = std::env::var("UPDATE_CALL_FORMS").is_ok();
        for relative in PLUGIN_FILES {
            let path = repo_file(relative);
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{relative}: {e}"));
            let fresh = replace_block(&text).unwrap_or_else(|| panic!("{relative} has no call-forms markers"));
            if update && fresh != text {
                std::fs::write(&path, &fresh).unwrap();
                continue;
            }
            assert_eq!(fresh, text, "{relative} drifted from the guard's rule data: run `UPDATE_CALL_FORMS=1 cargo test -p guard-core call_forms`");
        }
    }

    #[test]
    fn a_block_with_a_form_changed_by_hand_is_detected() {
        let block = render_block();
        let tampered = block.replace("without those flags", "with any flags");
        assert_ne!(tampered, block);
        assert_eq!(replace_block(&tampered).unwrap(), block);
    }
}
