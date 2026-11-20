// SPDX-License-Identifier: MIT
//! Agents Maestro desktop app: hosts the panel server (`crates/app`) in this process, or attaches to a running `harness serve`, and shows it
//! in a frameless window. `Agents Maestro.exe [--project <folder>]`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host;
mod window;

use host::Host;
use std::sync::Arc;
use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_window_state::StateFlags;

const PROJECT_FLAG: &str = "--project";

/// The folder after `--project` (or `--project=<folder>`), empty when none.
fn project_arg(args: &[String]) -> String {
    let prefix = format!("{PROJECT_FLAG}=");
    for (index, arg) in args.iter().enumerate() {
        if arg == PROJECT_FLAG {
            return args.get(index + 1).cloned().unwrap_or_default();
        }
        if let Some(value) = arg.strip_prefix(&prefix) {
            return value.to_string();
        }
    }
    String::new()
}

/// A second launch: focus the window and open the project it names.
fn on_second_launch(app: &tauri::AppHandle, args: Vec<String>) {
    let Some(window) = app.get_webview_window(window::MAIN_LABEL) else { return };
    let project = project_arg(&args);
    if !project.is_empty() {
        if let Some(url) = app.try_state::<Arc<Host>>().and_then(|host| host.open_project(&project)) {
            if let Ok(parsed) = url.parse() {
                let _ = window.navigate(parsed);
            }
        }
    }
    window::bring_to_front(&window);
}

fn main() {
    let initial_project = project_arg(&std::env::args().collect::<Vec<_>>());
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| on_second_launch(app, args)))
        .plugin(tauri_plugin_window_state::Builder::default().with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED).build())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let env = cfg::Env::process();
            let stopped = app.handle().clone();
            match Host::start(&env, &initial_project, move || stopped.exit(0)) {
                Ok((host, first_url)) => {
                    let panel_url = host.url().to_string();
                    app.manage(Arc::new(host));
                    window::create(app.handle(), &first_url, &panel_url)?;
                }
                Err(failure) => {
                    let handle = app.handle().clone();
                    app.dialog()
                        .message(format!("Agents Maestro could not start.\n\n{failure}"))
                        .title("Agents Maestro")
                        .kind(MessageDialogKind::Error)
                        .show(move |_| handle.exit(1));
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Mac-like: closing the window while a loop or a job runs keeps the app (and the loop) alive; a second launch shows it again
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.app_handle().try_state::<Arc<Host>>().is_some_and(|host| host.is_busy()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("the Tauri application could not be built");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(host) = handle.try_state::<Arc<Host>>() {
                host.shutdown();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_flag_forms() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(project_arg(&args(&["app.exe", "--project", "C:\\repo"])), "C:\\repo");
        assert_eq!(project_arg(&args(&["app.exe", "--project=C:\\a b"])), "C:\\a b");
        assert_eq!(project_arg(&args(&["app.exe"])), "");
        assert_eq!(project_arg(&args(&["app.exe", "--project"])), "");
    }
}
