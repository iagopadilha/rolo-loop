// SPDX-License-Identifier: MIT
//! The main window: frameless (the page draws the macOS-style traffic lights), OS shadow and rounded corners, a minimum size the panel's
//! compact layout fits, and a navigation lock: the window only shows the panel origin; any other link opens in the default browser.

use tauri::{AppHandle, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const MAIN_LABEL: &str = "main";
const TITLE: &str = "Agents Maestro";
const DEFAULT_SIZE: (f64, f64) = (1280.0, 820.0);
const MIN_SIZE: (f64, f64) = (760.0, 520.0);
const EXTERNAL_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

pub fn create(app: &AppHandle, first_url: &str, panel_url: &str) -> Result<WebviewWindow, Box<dyn std::error::Error>> {
    let first: Url = first_url.parse()?;
    let panel: Url = panel_url.parse()?;
    let window = WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::External(first))
        .title(TITLE)
        .inner_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1)
        .min_inner_size(MIN_SIZE.0, MIN_SIZE.1)
        .center()
        .decorations(false)
        .shadow(true)
        .on_navigation(move |target| {
            if same_origin(target, &panel) {
                return true;
            }
            if EXTERNAL_SCHEMES.contains(&target.scheme()) {
                app::composition::open_in_browser(target.as_str());
            }
            false
        })
        .build()?;
    Ok(window)
}

/// Shows, restores and focuses the main window (tray click, second launch).
pub fn bring_to_front(window: &WebviewWindow) {
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// Same scheme, host and port as the panel.
pub fn same_origin(target: &Url, panel: &Url) -> bool {
    target.scheme() == panel.scheme() && target.host_str() == panel.host_str() && target.port_or_known_default() == panel.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_panel_origin_stays_in_the_window() {
        let panel: Url = "http://127.0.0.1:47800/".parse().unwrap();
        let ok = |u: &str| same_origin(&u.parse().unwrap(), &panel);
        assert!(ok("http://127.0.0.1:47800/p/abc/#/loop"));
        assert!(!ok("http://127.0.0.1:47801/"));
        assert!(!ok("http://localhost:47800/"));
        assert!(!ok("https://127.0.0.1:47800/"));
        assert!(!ok("https://github.com/iagopadilha/claude-maestro"));
    }
}
