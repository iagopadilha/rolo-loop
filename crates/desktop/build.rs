// SPDX-License-Identifier: MIT
//! The installer resources come from `target/desktop-stage/` (filled by `tools/desktop/stage.mjs`, the `beforeBuildCommand` of
//! `cargo tauri build`). A plain `cargo build`/`clippy`/`test` on a fresh checkout has no stage yet: the folders are created empty so the
//! Tauri resource check passes; only the installer build needs their content.
use std::path::Path;

const STAGE_FOLDERS: [&str; 2] = ["../../target/desktop-stage/plugin", "../../target/desktop-stage/.claude-plugin"];

fn main() {
    for folder in STAGE_FOLDERS {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(folder);
        if !path.exists() {
            std::fs::create_dir_all(&path).expect("could not create the desktop stage folder");
        }
    }
    tauri_build::build()
}
