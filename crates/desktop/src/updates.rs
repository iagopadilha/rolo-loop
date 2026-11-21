// SPDX-License-Identifier: MIT
//! In-app updates from the GitHub releases (`latest.json`, signed with the project's updater key). The app checks a while after it
//! starts and then every few hours; "Check for updates" in the tray checks now. An update is only installed after the user says yes, and
//! never while a loop or a job runs: the installer replaces the app's files, which would stop the loop. Everything runs off the main
//! thread; a failed check (offline, no release yet) is silent unless the user asked for it.
use crate::host::Host;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const TITLE: &str = "Rolo Loop";

/// One check at a time (the timer and the tray may ask together).
static CHECKING: AtomicBool = AtomicBool::new(false);

/// What a check leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// Nothing new: say so only when the user asked.
    UpToDate { tell: bool },
    /// A newer version while a loop or a job runs: wait (say so only when the user asked).
    WaitForIdle { version: String, tell: bool },
    /// A newer version and nothing running: ask the user.
    Ask { version: String },
}

/// Pure decision of a check: `found` is the newer version, if any.
pub fn next_step(found: Option<&str>, busy: bool, asked: bool) -> Next {
    match found {
        None => Next::UpToDate { tell: asked },
        Some(version) if busy => Next::WaitForIdle { version: version.to_string(), tell: asked },
        Some(version) => Next::Ask { version: version.to_string() },
    }
}

/// Starts the background checks (first after [`FIRST_CHECK_DELAY`], then every [`CHECK_EVERY`]).
pub fn start(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK_DELAY);
        loop {
            check(&app, false);
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Checks now on a worker thread (`asked` = the user clicked "Check for updates").
pub fn check_now(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || check(&app, true));
}

fn check(app: &AppHandle, asked: bool) {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    let outcome = tauri::async_runtime::block_on(run_check(app, asked));
    CHECKING.store(false, Ordering::SeqCst);
    if let Err(problem) = outcome {
        eprintln!("[updates] {problem}");
        if asked {
            tell(app, &format!("Could not check for updates.\n\n{problem}"), MessageDialogKind::Warning);
        }
    }
}

async fn run_check(app: &AppHandle, asked: bool) -> Result<(), String> {
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    let busy = app.try_state::<Arc<Host>>().is_some_and(|host| host.is_busy());
    match next_step(update.as_ref().map(|u| u.version.as_str()), busy, asked) {
        Next::UpToDate { tell: true } => tell(app, &format!("You have the latest version ({}).", app.package_info().version), MessageDialogKind::Info),
        Next::UpToDate { tell: false } => {}
        Next::WaitForIdle { version, tell: true } => tell(
            app,
            &format!("Rolo Loop {version} is available. A loop or a job is running: check again when it finishes, so the update does not stop it."),
            MessageDialogKind::Info,
        ),
        Next::WaitForIdle { .. } => {}
        Next::Ask { version } => {
            let install = app
                .dialog()
                .message(format!("Rolo Loop {version} is available (you have {}).\n\nInstall it now? The app closes and opens again.", app.package_info().version))
                .title(TITLE)
                .buttons(MessageDialogButtons::OkCancelCustom("Install now".into(), "Later".into()))
                .blocking_show();
            if install {
                let update = update.expect("Ask comes with an update");
                let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
                if app.try_state::<Arc<Host>>().is_some_and(|host| host.is_busy()) {
                    return Err("a loop started during the download; the update waits for the next check".into());
                }
                if let Some(host) = app.try_state::<Arc<Host>>() {
                    host.shutdown(); // the installer replaces the files: stop the panel server first
                }
                update.install(bytes).map_err(|e| e.to_string())?; // Windows: the installer takes over and the app exits
                app.restart();
            }
        }
    }
    Ok(())
}

fn tell(app: &AppHandle, text: &str, kind: MessageDialogKind) {
    app.dialog().message(text).title(TITLE).kind(kind).blocking_show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_without_news_is_silent_unless_asked() {
        assert_eq!(next_step(None, false, false), Next::UpToDate { tell: false });
        assert_eq!(next_step(None, true, true), Next::UpToDate { tell: true });
    }

    #[test]
    fn a_running_loop_is_never_interrupted() {
        assert_eq!(next_step(Some("0.4.0"), true, false), Next::WaitForIdle { version: "0.4.0".into(), tell: false });
        assert_eq!(next_step(Some("0.4.0"), true, true), Next::WaitForIdle { version: "0.4.0".into(), tell: true });
    }

    #[test]
    fn an_idle_app_asks_before_installing() {
        assert_eq!(next_step(Some("0.4.0"), false, false), Next::Ask { version: "0.4.0".into() });
    }
}
