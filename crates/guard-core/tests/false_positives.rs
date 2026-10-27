// SPDX-License-Identifier: MIT
//! Safe work the guard used to refuse, and the real cases it must keep refusing.

use cfg::Env;
use guard_core::check::Check;
use guard_core::paths::G;
use guard_core::rules::profile_rules;
use guard_core::state::Permissions;

struct Repo {
    _dir: tempfile::TempDir,
    root: String,
    g: G,
}

fn repo(headless: bool, env_test: Option<&str>, network: Option<&str>) -> Repo {
    // under the target dir: the OS temp dir can be an 8.3 short path, which the guard expands on one side only
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join(".specs/project")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    if let Some(e) = env_test {
        std::fs::write(root.join(".specs/project/.env.test"), e).unwrap();
    }
    if let Some(n) = network {
        std::fs::write(root.join(".specs/project/NETWORK.json"), n).unwrap();
    }
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut pairs = vec![("LOOP_DATA_DIR", data.display().to_string()), ("USERPROFILE", dir.path().display().to_string()), ("HOME", dir.path().display().to_string())];
    if headless {
        pairs.push(("LOOP_HEADLESS", "1".to_string()));
    }
    let g = G::new(Env::from_pairs(pairs));
    Repo { root: root.display().to_string(), _dir: dir, g }
}

fn rules_hit(r: &Repo, profile: &str, command: &str) -> Vec<String> {
    let perm = Permissions { cwd: Some(r.root.clone()), specs_dir: Some(format!("{}/.specs", r.root)), ..Permissions::default() };
    let mut check = Check::new(&r.g, perm);
    let mut warnings = String::new();
    let mut rules = profile_rules(profile);
    if r.g.env.get("LOOP_HEADLESS").is_some() {
        rules.extend(guard_core::net::RULES.iter().map(|s| s.to_string()));
    }
    let mut hits: Vec<String> = check.check_bash(command, &rules, &mut warnings).into_iter().map(|(rule, why)| format!("{rule}: {why}")).collect();
    hits.dedup();
    hits
}

fn passes(r: &Repo, profile: &str, command: &str) {
    let hits = rules_hit(r, profile, command);
    assert!(hits.is_empty(), "must pass: {command} -> {hits:?}");
}

fn blocks(r: &Repo, profile: &str, command: &str) {
    assert!(!rules_hit(r, profile, command).is_empty(), "must block: {command}");
}

#[test]
fn map_docs_folder_itself_without_slash() {
    let r = repo(false, None, None);
    passes(&r, "roadmapmap", "mkdir -p docs/codebase");
    passes(&r, "roadmapmap", "mkdir -p docs/codebase/");
    passes(&r, "roadmapmap", "mkdir -p .specs");
    blocks(&r, "roadmapmap", "mkdir -p docs/codebase-evil");
    blocks(&r, "roadmapmap", "mkdir -p src");
    blocks(&r, "roadmapmap", "mkdir -p docs");
}

#[test]
fn git_bash_drive_cd_then_relative_mkdir() {
    if !cfg!(windows) {
        return;
    }
    let r = repo(false, None, None);
    let posix = {
        let p = r.root.replace('\\', "/");
        let (drive, rest) = p.split_at(2);
        format!("/{}{}", drive[..1].to_lowercase(), rest)
    };
    passes(&r, "roadmapmap", &format!("cd \"{posix}\" && mkdir -p docs/codebase"));
    blocks(&r, "roadmapmap", &format!("cd \"{posix}\" && mkdir -p src"));
}

#[test]
fn regex_arguments_are_not_paths() {
    let r = repo(false, None, None);
    passes(&r, "roadmapmap", "grep -rn '^foo.*files' .specs");
    passes(&r, "roadmapmap", "sed -n '/^a/,/^b/p' .specs/project/ROADMAP.md");
    blocks(&r, "roadmapmap", "sed -i 's/a/b/' src/main.rs");
    blocks(&r, "roadmapmap", "echo x > src/main.rs");
}

#[test]
fn heredoc_bodies_are_data() {
    let r = repo(true, None, Some(r#"{"allow": ["api.example.com"]}"#));
    passes(&r, "roadmapmap", "cat > docs/codebase/NOTES.md <<'EOF'\nSee https://docs.example.org/guide for details.\nEOF");
    blocks(&r, "roadmapmap", "curl https://docs.example.org/guide");
    blocks(&r, "roadmapmap", "cat > src/NOTES.md <<'EOF'\nplain text\nEOF");
}

#[test]
fn url_variable_for_curl() {
    let r = repo(true, Some("API_URL=https://api.example.com\n"), Some(r#"{"allow": ["api.example.com"]}"#));
    passes(&r, "spec", "curl $API_URL/x");
    blocks(&r, "spec", "curl $TARGET");
    blocks(&r, "spec", "curl $(cat url.txt)");
}

#[test]
fn roadmap_edit_script_is_not_a_credential_read() {
    let r = repo(false, None, None);
    passes(&r, "orchestrator", "python -c \"import re; p='.specs/project/ROADMAP.md'; t=open(p).read(); open(p,'w').write(re.sub('e2e=pending','e2e=pass',t))\"");
    blocks(&r, "orchestrator", "cat .secrets/token");
    blocks(&r, "orchestrator", "cat ~/.ssh/id_rsa");
}

#[test]
fn read_only_git_passes_for_every_profile() {
    let r = repo(false, None, None);
    for profile in ["spec", "roadmapmap", "planning"] {
        passes(&r, profile, "git check-ignore -v build/out.txt");
        passes(&r, profile, "git ls-tree HEAD");
        blocks(&r, profile, "git commit -m x");
        blocks(&r, profile, "git add file.txt");
    }
}

#[test]
fn more_search_and_script_shapes() {
    let r = repo(true, None, Some(r#"{"allow": ["api.example.com"]}"#));
    passes(&r, "roadmapmap", "rg -n \"^## Stage\" .specs/project/ROADMAP.md");
    passes(&r, "roadmapmap", "grep -rEn '^(a|b).*files' . --include='*files*'");
    passes(&r, "roadmapmap", "grep -rn \"*files*\" docs");
    passes(&r, "roadmapmap", "find . -name '*files*' -not -path './node_modules/*'");
    passes(&r, "orchestrator", "sed -i 's/e2e=pending/e2e=pass/' .specs/project/ROADMAP.md");
    passes(
        &r,
        "orchestrator",
        "python - <<'EOF'\nimport re\np = '.specs/project/ROADMAP.md'\nt = open(p).read()\nopen(p, 'w').write(re.sub(r'e2e=\\w+', 'e2e=pass', t))\nEOF",
    );
    passes(&r, "orchestrator", "node -e \"const fs=require('fs');const p='.specs/project/ROADMAP.md';fs.writeFileSync(p,fs.readFileSync(p,'utf8').replace('a','b'))\"");
    passes(&r, "roadmapmap", "cat > docs/codebase/A.md <<EOF\nvisit http://old-host.example.net/path and ~/notes\nEOF");
    blocks(&r, "orchestrator", "cat ~/.aws/credentials");
}

#[test]
fn real_egress_cases_stay_refused() {
    let r = repo(true, None, Some(r#"{"allow": ["api.example.com"]}"#));
    blocks(&r, "spec", "echo \"await page.goto('https://prod.example.net')\" > t.js && playwright-cli run-code --filename t.js");
    blocks(&r, "spec", "curl https://prod.example.net/health");
    blocks(&r, "spec", "python -c \"import urllib.request as u; u.urlopen('https://prod.example.net')\"");
}

#[test]
fn python_isolated_mode_stays_refused() {
    let r = repo(true, None, None);
    blocks(&r, "spec", "python -I script.py");
}
