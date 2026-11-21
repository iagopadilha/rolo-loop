// SPDX-License-Identifier: MIT
//! Tray icon: a click shows the window; the menu opens it, checks for updates or quits. Quitting while a loop or a job runs asks first (the loop's state is in
//! the project files, so it can be resumed from the panel later).

use crate::host::Host;
use crate::window;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

const OPEN_ID: &str = "open";
const QUIT_ID: &str = "quit";
const UPDATE_ID: &str = "update";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, OPEN_ID, "Open Rolo Loop", true, None::<&str>)?;
    let update = MenuItem::with_id(app, UPDATE_ID, "Check for updates", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, QUIT_ID, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &update, &quit_item])?;
    let mut builder = TrayIconBuilder::with_id("main").tooltip("Rolo Loop").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            OPEN_ID => show_main(app),
            UPDATE_ID => crate::updates::check_now(app),
            QUIT_ID => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(main) = app.get_webview_window(window::MAIN_LABEL) {
        window::bring_to_front(&main);
    }
}

/// Quits at once when nothing runs; otherwise asks.
pub fn quit(app: &AppHandle) {
    let busy = app.try_state::<Arc<Host>>().is_some_and(|host| host.is_busy());
    if !busy {
        app.exit(0);
        return;
    }
    let handle = app.clone();
    app.dialog()
        .message("A loop or a Claude job is still running. Quit Rolo Loop anyway?\n\nThe loop keeps its state in the project files: open the app again to follow or resume it.")
        .title("Rolo Loop")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Quit".into(), "Keep running".into()))
        .show(move |confirmed| {
            if confirmed {
                handle.exit(0);
            }
        });
}
