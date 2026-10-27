// SPDX-License-Identifier: MIT
//! Folder choice to open/create a project: the native Windows dialog (PowerShell with fixed code, no shell string, with a timeout) and, when it is
//! not available, the front end's own folder browser (only subfolder names, flagging `.specs`/`.git`).

use super::App;
use crate::domain::errors::AppError;
use serde_json::{json, Value};
use std::path::Path;

pub const FOLDER_DIALOG_TIMEOUT_SECONDS: u64 = 300;
pub const PATH_OUTPUT_PREFIX: &str = "PATH:";
pub const NO_DIALOG_OUTPUT_PREFIX: &str = "NO_DIALOG:";
pub const LISTED_FOLDERS_LIMIT: usize = 500;
pub const CANCELLED_REASON: &str = "cancelled";
pub const INITIAL_FOLDER_VARIABLE: &str = "HARNESS_DIALOG_INITIAL_DIR";
/// Fixed PowerShell code of the native dialog (never built from user input; the initial folder travels in an environment variable).
const FOLDER_DIALOG_CODE: &str = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
try {
    Add-Type -AssemblyName System.Windows.Forms
    $owner = New-Object System.Windows.Forms.Form
    $owner.TopMost = $true
    $owner.ShowInTaskbar = $false
    $owner.StartPosition = 'Manual'
    $owner.Location = New-Object System.Drawing.Point(-32000, -32000)
    $owner.Show()
    $dialog = New-Object System.Windows.Forms.FolderBrowserDialog
    $dialog.Description = 'Choose the project folder'
    $dialog.ShowNewFolderButton = $true
    $initial = $env:HARNESS_DIALOG_INITIAL_DIR
    if ($initial -and (Test-Path -LiteralPath $initial -PathType Container)) { $dialog.SelectedPath = $initial }
    $result = $dialog.ShowDialog($owner)
    $owner.Close()
    if ($result -eq [System.Windows.Forms.DialogResult]::OK) { Write-Output ('PATH:' + $dialog.SelectedPath) } else { Write-Output 'PATH:' }
} catch {
    Write-Output ('NO_DIALOG:' + $_.Exception.GetType().Name + ': ' + $_.Exception.Message.Substring(0, [Math]::Min(200, $_.Exception.Message.Length)))
    exit 3
}
"#;

/// Drives (Windows) plus the user's home folder.
pub fn list_system_roots(home: &Path) -> Vec<String> {
    let mut roots: Vec<String> = (b'A'..=b'Z').map(|letter| format!("{}:\\", letter as char)).filter(|root| Path::new(root).is_dir()).collect();
    roots.push(home.display().to_string());
    roots
}

fn describe_folder(name: &str, path: &str) -> Value {
    json!({"name": name, "path": path, "has_specs": Path::new(path).join(".specs").is_dir(), "has_git": Path::new(path).join(".git").exists()})
}

pub struct FolderPickerService<'a> {
    pub app: &'a App,
    /// Replaces the native dialog command (tests).
    pub dialog_argv: Option<Vec<String>>,
    pub timeout_seconds: u64,
}

impl App {
    pub fn folder_picker(&self) -> FolderPickerService<'_> {
        FolderPickerService { app: self, dialog_argv: None, timeout_seconds: FOLDER_DIALOG_TIMEOUT_SECONDS }
    }
}

impl FolderPickerService<'_> {
    fn dialog_command(&self) -> Vec<String> {
        self.dialog_argv.clone().unwrap_or_else(|| {
            vec!["powershell.exe".into(), "-NoProfile".into(), "-STA".into(), "-NonInteractive".into(), "-Command".into(), FOLDER_DIALOG_CODE.to_string()]
        })
    }

    /// `{"path": "...|null", "reason": "..."}`: null with a reason when the dialog is not available, it failed or the user cancelled. `initial`: an
    /// existing folder the dialog opens in (anything else is ignored).
    pub fn pick_folder_with_dialog(&self, initial: Option<&str>) -> Value {
        let mut extra_env: Vec<(String, String)> = Vec::new();
        if let Some(initial) = initial.map(str::trim).filter(|i| !i.is_empty()) {
            let expanded = if initial.starts_with('~') { format!("{}{}", self.app.env.home_dir().display(), &initial[1..]) } else { initial.to_string() };
            if Path::new(&expanded).is_dir() {
                extra_env.push((INITIAL_FOLDER_VARIABLE.into(), pycompat::nt_abspath(&expanded)));
            }
        }
        let (code, output, error) = self.app.processes.run_and_wait(&self.dialog_command(), "", self.timeout_seconds, None, &extra_env, None);
        let lines: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        for line in lines.iter().rev() {
            if let Some(path) = line.strip_prefix(PATH_OUTPUT_PREFIX) {
                let path = path.trim();
                return if path.is_empty() { json!({"path": null, "reason": CANCELLED_REASON}) } else { json!({"path": pycompat::nt_normpath(path), "reason": null}) };
            }
            if let Some(detail) = line.strip_prefix(NO_DIALOG_OUTPUT_PREFIX) {
                self.app.log.warn("folder_dialog_unavailable", &[("detail", json!(detail.chars().take(200).collect::<String>()))]);
                return json!({"path": null, "reason": "native dialog unavailable: use the folder browser"});
            }
        }
        let tail: String = error.chars().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
        self.app.log.warn("folder_dialog_failed", &[("code", json!(code)), ("error", json!(tail))]);
        json!({"path": null, "reason": format!("native dialog failed (code {code}): use the folder browser")})
    }

    /// No path: the roots. With a path: only the subfolder names (no files, no hidden ones), flagging projects.
    pub fn list_subfolders(&self, path: Option<&str>) -> Result<Value, AppError> {
        let Some(path) = path.filter(|p| !p.is_empty()) else {
            let roots = list_system_roots(&self.app.env.home_dir());
            return Ok(json!({"path": null, "parent": null, "folders": roots.iter().map(|r| describe_folder(r, r)).collect::<Vec<_>>(), "truncated": false}));
        };
        let expanded = if path.starts_with('~') { format!("{}{}", self.app.env.home_dir().display(), &path[1..]) } else { path.to_string() };
        let folder = pycompat::nt_abspath(&expanded);
        if !Path::new(&folder).is_dir() {
            return Err(AppError::invalid_path(format!("not an existing folder: {folder}")));
        }
        let entries = std::fs::read_dir(&folder).map_err(|e| AppError::invalid_path(format!("no access to {folder}: {}", io_name(&e))))?;
        let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort_by_key(|n| n.to_lowercase());
        let visible: Vec<&String> = names.iter().filter(|n| !n.starts_with(['.', '$']) && Path::new(&folder).join(n).is_dir()).collect();
        let parent = pycompat::nt_dirname(&folder);
        Ok(json!({
            "path": folder, "parent": if parent != folder { json!(parent) } else { Value::Null },
            "folders": visible.iter().take(LISTED_FOLDERS_LIMIT).map(|name| describe_folder(name, &pycompat::nt_join(&folder, name))).collect::<Vec<_>>(),
            "truncated": visible.len() > LISTED_FOLDERS_LIMIT,
        }))
    }
}

fn io_name(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        _ => "OSError",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::process_runner::ProcessRunner;
    use std::sync::{Arc, Mutex};

    struct Dialog(Mutex<(i32, String)>);

    impl ProcessRunner for Dialog {
        fn run_and_wait(&self, _: &[String], _: &str, _: u64, _: Option<&str>, _: &[(String, String)], _: Option<&[(String, String)]>) -> (i32, String, String) {
            let state = self.0.lock().unwrap().clone();
            (state.0, state.1, "boom".into())
        }
        fn spawn_detached(&self, _: &[String], _: &str, _: &str, _: &[(String, String)]) -> std::io::Result<i64> {
            Ok(1)
        }
        fn is_alive(&self, _: i64) -> bool {
            false
        }
        fn kill_tree(&self, _: i64) {}
    }

    #[test]
    fn dialog_answers_are_interpreted_and_folders_listed() {
        let dir = tempfile::tempdir().unwrap();
        let dialog = Arc::new(Dialog(Mutex::new((0, "noise\nPATH:C:\\work\\app\n".into()))));
        let app = App::for_tests_with(dir.path(), dialog.clone());
        let picker = app.folder_picker();
        assert_eq!(picker.pick_folder_with_dialog(Some("C:\\none")), json!({"path": "C:\\work\\app", "reason": null}));
        *dialog.0.lock().unwrap() = (0, "PATH:\n".into());
        assert_eq!(picker.pick_folder_with_dialog(None), json!({"path": null, "reason": "cancelled"}));
        *dialog.0.lock().unwrap() = (3, "NO_DIALOG:Exception: x\n".into());
        assert_eq!(picker.pick_folder_with_dialog(None)["reason"], "native dialog unavailable: use the folder browser");
        *dialog.0.lock().unwrap() = (1, String::new());
        assert_eq!(picker.pick_folder_with_dialog(None)["reason"], "native dialog failed (code 1): use the folder browser");
        std::fs::create_dir_all(dir.path().join("Beta").join(".specs")).unwrap();
        std::fs::create_dir_all(dir.path().join("alpha").join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "x").unwrap();
        let listing = picker.list_subfolders(Some(&dir.path().display().to_string())).unwrap();
        let names: Vec<&str> = listing["folders"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["alpha", "Beta", "data"]);
        assert_eq!(
            (listing["folders"][0]["has_git"].clone(), listing["folders"][1]["has_specs"].clone(), listing["truncated"].clone()),
            (json!(true), json!(true), json!(false))
        );
        assert!(picker.list_subfolders(Some("C:\\definitely\\missing")).is_err());
        let roots = picker.list_subfolders(None).unwrap();
        assert!(roots["path"].is_null() && !roots["folders"].as_array().unwrap().is_empty());
    }
}
