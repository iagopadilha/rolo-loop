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
/// It shows the modern Windows folder picker (IFileOpenDialog with FOS_PICKFOLDERS: the Explorer window with the folder tree, address bar and
/// "New folder"); if that COM dialog cannot be created it falls back to the classic FolderBrowserDialog. Without a valid initial folder, Windows
/// opens its own default location.
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
    $initial = $env:HARNESS_DIALOG_INITIAL_DIR
    if (-not ($initial -and (Test-Path -LiteralPath $initial -PathType Container))) { $initial = '' }
    $picked = $null; $answered = $false
    try {
        Add-Type -ErrorAction Stop -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("DC1C5A9C-E88A-4dde-A5A1-60F82A20AEF7")] class MaestroFileOpenDialog {}
[ComImport, Guid("43826d1e-e718-42ee-bc55-a1e261c37bfe"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMaestroShellItem {
    void BindToHandler(IntPtr pbc, ref Guid bhid, ref Guid riid, out IntPtr ppv);
    void GetParent(out IMaestroShellItem ppsi);
    void GetDisplayName(uint sigdn, [MarshalAs(UnmanagedType.LPWStr)] out string name);
    void GetAttributes(uint mask, out uint attr);
    void Compare(IMaestroShellItem psi, uint hint, out int order);
}
[ComImport, Guid("42f85136-db7e-439c-85f1-e4075d135fc8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMaestroFileDialog {
    [PreserveSig] int Show(IntPtr parent);
    void SetFileTypes(uint count, IntPtr types);
    void SetFileTypeIndex(uint index);
    void GetFileTypeIndex(out uint index);
    void Advise(IntPtr events, out uint cookie);
    void Unadvise(uint cookie);
    void SetOptions(uint options);
    void GetOptions(out uint options);
    void SetDefaultFolder(IMaestroShellItem item);
    void SetFolder(IMaestroShellItem item);
    void GetFolder(out IMaestroShellItem item);
    void GetCurrentSelection(out IMaestroShellItem item);
    void SetFileName([MarshalAs(UnmanagedType.LPWStr)] string name);
    void GetFileName([MarshalAs(UnmanagedType.LPWStr)] out string name);
    void SetTitle([MarshalAs(UnmanagedType.LPWStr)] string title);
    void SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string text);
    void SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string text);
    void GetResult(out IMaestroShellItem item);
    void AddPlace(IMaestroShellItem item, int placement);
    void SetDefaultExtension([MarshalAs(UnmanagedType.LPWStr)] string extension);
    void Close(int hr);
    void SetClientGuid(ref Guid guid);
    void ClearClientData();
    void SetFilter(IntPtr filter);
}
public static class MaestroFolderPicker {
    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = false)]
    static extern void SHCreateItemFromParsingName(string path, IntPtr bindContext, ref Guid riid, out IMaestroShellItem item);
    /// <summary>The chosen folder, "" when the user cancelled.</summary>
    public static string Pick(IntPtr owner, string initial, string title) {
        IMaestroFileDialog dialog = (IMaestroFileDialog)new MaestroFileOpenDialog();
        uint options; dialog.GetOptions(out options);
        dialog.SetOptions(options | 0x20 | 0x40 | 0x800); // FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST
        dialog.SetTitle(title);
        if (!string.IsNullOrEmpty(initial)) {
            Guid shellItem = new Guid("43826d1e-e718-42ee-bc55-a1e261c37bfe");
            IMaestroShellItem folder; SHCreateItemFromParsingName(initial, IntPtr.Zero, ref shellItem, out folder);
            dialog.SetFolder(folder);
        }
        int hr = dialog.Show(owner);
        if (hr == unchecked((int)0x800704C7)) return ""; // ERROR_CANCELLED
        if (hr != 0) Marshal.ThrowExceptionForHR(hr);
        IMaestroShellItem result; dialog.GetResult(out result);
        string path; result.GetDisplayName(0x80058000, out path); // SIGDN_FILESYSPATH
        return path;
    }
}
'@
        $picked = [MaestroFolderPicker]::Pick($owner.Handle, $initial, 'Choose the project folder')
        $answered = $true
    } catch {
        $answered = $false
    }
    if (-not $answered) {
        $dialog = New-Object System.Windows.Forms.FolderBrowserDialog
        $dialog.Description = 'Choose the project folder'
        $dialog.ShowNewFolderButton = $true
        if ($initial) { $dialog.SelectedPath = $initial }
        $result = $dialog.ShowDialog($owner)
        if ($result -eq [System.Windows.Forms.DialogResult]::OK) { $picked = $dialog.SelectedPath } else { $picked = '' }
    }
    $owner.Close()
    Write-Output ('PATH:' + $picked)
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
