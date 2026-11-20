// SPDX-License-Identifier: MIT
//! `harness install [--data-dir D] [--skill-dir S] [--desktop-exe E] [--path add|remove]`: writes `~/.loop-harness/config.json`
//! (`app_root_dir`, the skill folder and, from the desktop installer, `desktop_exe`); this is how the skill finds the app. Does not touch
//! `~/.claude`. `--path add` also puts the folder of this executable on the user PATH; `--path remove` ONLY takes it off (the uninstaller).

use crate::serve_cmd::apply_data_dir;
use cfg::{Env, Locator};
use clap::Args as ClapArgs;
use serde_json::{json, Map, Value};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(long, default_value = "")]
    data_dir: String,
    /// orchestrator skill folder (default: detected)
    #[arg(long, default_value = "")]
    skill_dir: String,
    /// the desktop app executable, recorded so `harness serve` can open projects in it
    #[arg(long, default_value = "")]
    desktop_exe: String,
    /// `add`: also put this executable's folder on the user PATH; `remove`: only take it off (nothing else is written)
    #[arg(long, value_parser = ["add", "remove"])]
    path: Option<String>,
}

/// Adds or removes the folder of this executable on the user PATH; prints what it did. Never fails the command: a PATH that cannot be
/// changed is reported and `harness doctor` shows it.
fn update_user_path(add: bool) {
    let Some(folder) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|p| p.display().to_string())) else { return };
    let (current, expandable) = match winproc::read_user_path() {
        Ok(read) => read,
        Err(failure) => {
            eprintln!("user PATH not changed (cannot read it: {failure})");
            return;
        }
    };
    match winproc::path_with_entry(&current, &folder, add) {
        None => println!("user PATH already {} {folder}", if add { "has" } else { "lacks" }),
        Some(next) => match winproc::write_user_path(&next, expandable) {
            Ok(()) => println!("user PATH: {} {folder} (new terminals see it)", if add { "added" } else { "removed" }),
            Err(failure) => eprintln!("user PATH not changed ({failure})"),
        },
    }
}

pub fn run(args: Args) -> i32 {
    if args.path.as_deref() == Some("remove") {
        update_user_path(false);
        return 0;
    }
    let mut env = Env::process();
    apply_data_dir(&mut env, &args.data_dir);
    let data_dir = env.user_data_dir();
    let path = data_dir.join("config.json");
    let mut config: Map<String, Value> = Map::new();
    if path.is_file() {
        let read = std::fs::read(&path).ok().and_then(|bytes| String::from_utf8(bytes).ok()).map(|t| t.strip_prefix('\u{feff}').unwrap_or(&t).to_string());
        let parsed = match read {
            Some(text) if text.trim().is_empty() => Ok(Map::new()),
            Some(text) => {
                serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()).and_then(|v| v.as_object().cloned().ok_or_else(|| "not a JSON object".to_string()))
            }
            None => Err("cannot be read as UTF-8 text".to_string()),
        };
        match parsed {
            Ok(existing) => config = existing,
            Err(failure) => {
                eprintln!(
                    "{} exists but cannot be read ({failure}): fix or remove it, then run install again. Nothing was written (your settings are kept).",
                    path.display()
                );
                return 1;
            }
        }
    }
    let repo_root = engine_state::app_ctx::repo_root(&env);
    let locator = Locator::new(env.clone(), repo_root.clone());
    let configured =
        if args.skill_dir.is_empty() { config.get("orchestrator_skill_dir").and_then(Value::as_str).unwrap_or("").to_string() } else { args.skill_dir.clone() };
    let skill_dir = locator.locate_orchestrator_skill_dir(&configured).display().to_string();
    let app_root = repo_root.join("app").display().to_string();
    config.insert("app_root_dir".into(), json!(app_root));
    if !skill_dir.is_empty() {
        config.insert("orchestrator_skill_dir".into(), json!(skill_dir));
    }
    if !args.desktop_exe.is_empty() {
        config.insert("desktop_exe".into(), json!(pycompat::nt_abspath(&args.desktop_exe)));
    }
    if std::fs::create_dir_all(&data_dir).is_err() {
        eprintln!("cannot create {}", data_dir.display());
        return 1;
    }
    if path.is_file() {
        let backup = format!("{}.bak", path.display());
        let _ = std::fs::copy(&path, &backup);
        println!("backup: {backup}");
    }
    if let Err(failure) = engine_state::loop_io::atomic_write_text(&path, &pycompat::json::py_json_dumps_indent(&Value::Object(config), false, 2)) {
        eprintln!("cannot write {}: {failure}", path.display());
        return 1;
    }
    println!("written: {}", path.display());
    println!("  app_root_dir = {app_root}");
    println!("  orchestrator_skill_dir = {}", if skill_dir.is_empty() { "(skill not found: run again with --skill-dir)" } else { &skill_dir });
    if args.path.as_deref() == Some("add") {
        update_user_path(true);
    }
    if skill_dir.is_empty() {
        1
    } else {
        0
    }
}
