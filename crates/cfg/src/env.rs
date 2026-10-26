// SPDX-License-Identifier: MIT
//! Environment access with an injectable snapshot (so tests never touch the real process environment),
//! `.env` parsing and the app's `.env` layering.
//!
//! Layering: `LOOP_ENV_FILE` (explicit file) is the ONLY file read when set; otherwise
//! `<data dir>/.env`. A real environment variable always beats a file value.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const EXPLICIT_FILE_VARIABLE: &str = "LOOP_ENV_FILE";
pub const DATA_DIR_VARIABLE: &str = "LOOP_DATA_DIR";
pub const DEFAULT_DATA_DIR_NAME: &str = ".loop-harness";

/// Process environment, or an in-memory map for tests.
#[derive(Clone, Debug, Default)]
pub struct Env {
    map: Option<HashMap<String, String>>,
    /// Values [`load_all`] copied in from an app `.env` (a name the environment already had is never recorded).
    loaded: Vec<(String, String)>,
}

impl Env {
    /// The live process environment.
    pub fn process() -> Self {
        Env { map: None, loaded: Vec::new() }
    }

    /// An isolated environment made of the given pairs only.
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Env {
            map: Some(
                pairs
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            ),
            loaded: Vec::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<String> {
        match &self.map {
            None => std::env::var(name).ok(),
            Some(map) => map
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone()),
        }
    }

    /// Value of `name`, or `""` when missing (the Python `os.environ.get(name, "")`).
    pub fn text(&self, name: &str) -> String {
        self.get(name).unwrap_or_default()
    }

    pub fn set(&mut self, name: &str, value: &str) {
        match &mut self.map {
            Some(map) => {
                map.insert(name.to_string(), value.to_string());
            }
            None => std::env::set_var(name, value),
        }
    }

    /// Every variable of the environment (name, value).
    pub fn vars(&self) -> Vec<(String, String)> {
        match &self.map {
            None => std::env::vars().collect(),
            Some(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    /// True when `name == value` is the copy [`load_all`] made from an app `.env` file (the user did not set it).
    pub fn is_loaded_value(&self, name: &str, value: &str) -> bool {
        self.loaded.iter().any(|(k, v)| k == name && v == value)
    }

    /// Home folder as Python's `expanduser("~")` sees it on Windows: `USERPROFILE`, then
    /// `HOMEDRIVE`+`HOMEPATH`; `HOME` is ignored on Windows since Python 3.8.
    pub fn home_dir(&self) -> PathBuf {
        if let Some(profile) = self.get("USERPROFILE").filter(|v| !v.is_empty()) {
            return PathBuf::from(profile);
        }
        let drive = self.text("HOMEDRIVE");
        let path = self.get("HOMEPATH").filter(|v| !v.is_empty());
        if let Some(path) = path {
            return PathBuf::from(format!("{drive}{path}"));
        }
        #[cfg(not(windows))]
        if let Some(home) = self.get("HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(home);
        }
        PathBuf::from("~")
    }

    /// `~/.loop-harness` or `LOOP_DATA_DIR`, made absolute.
    pub fn user_data_dir(&self) -> PathBuf {
        let chosen = self
            .get(DATA_DIR_VARIABLE)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home_dir().join(DEFAULT_DATA_DIR_NAME));
        abspath(&chosen)
    }

    /// Candidate `.env` files in priority order.
    pub fn env_file_candidates(&self) -> Vec<PathBuf> {
        match self.get(EXPLICIT_FILE_VARIABLE).filter(|v| !v.is_empty()) {
            Some(explicit) => vec![PathBuf::from(explicit)],
            None => vec![self.user_data_dir().join(".env")],
        }
    }
}

/// Python's `os.path.abspath`: absolute, `..` and `.` collapsed lexically, no symlink resolution.
pub fn abspath(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Parses `KEY=value` lines: whole-line `#` comments, optional `export `, one pair of matching quotes stripped.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in text.lines() {
        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = value.trim();
        let bytes = value.as_bytes();
        if bytes.len() >= 2
            && bytes[0] == bytes[bytes.len() - 1]
            && (bytes[0] == b'\'' || bytes[0] == b'"')
        {
            value = &value[1..value.len() - 1];
        }
        if !key.is_empty() {
            if let Some(slot) = out.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value.to_string();
            } else {
                out.push((key.to_string(), value.to_string()));
            }
        }
    }
    out
}

/// Reads a UTF-8 file tolerating a BOM; `None` when it cannot be read as UTF-8.
pub fn read_text_sig(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Loads every candidate `.env`; a variable already present is never overwritten.
/// Returns the files read.
pub fn load_all(env: &mut Env) -> Vec<PathBuf> {
    let mut used = Vec::new();
    for path in env.env_file_candidates() {
        if path.is_file() {
            if let Some(text) = read_text_sig(&path) {
                for (key, value) in parse_env(&text) {
                    if env.get(&key).is_none() {
                        env.set(&key, &value);
                        env.loaded.push((key, value));
                    }
                }
                used.push(path);
            }
        }
    }
    used
}

/// Value of a variable from the environment, then from the app `.env` (read without mutating the environment).
pub fn app_env_value(env: &Env, name: &str) -> String {
    if let Some(v) = env.get(name).filter(|v| !v.is_empty()) {
        return v;
    }
    for path in env.env_file_candidates() {
        if let Some(text) = read_text_sig(&path) {
            if let Some((_, v)) = parse_env(&text).into_iter().find(|(k, _)| k == name) {
                return v;
            }
        }
    }
    String::new()
}

/// Like [`app_env_value`] but tells a variable that is not there (`None`) from one set to an empty value.
pub fn app_env_option(env: &Env, name: &str) -> Option<String> {
    if let Some(v) = env.get(name) {
        return Some(v);
    }
    for path in env.env_file_candidates() {
        if let Some(text) = read_text_sig(&path) {
            if let Some((_, v)) = parse_env(&text).into_iter().find(|(k, _)| k == name) {
                return Some(v);
            }
        }
    }
    None
}

/// `envload.get_list`: the comma separated, trimmed, non-empty items of a variable (environment, then the app `.env`).
pub fn app_env_list(env: &Env, name: &str) -> Vec<String> {
    app_env_value(env, name).split(',').map(|i| i.trim().to_string()).filter(|i| !i.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs_quotes_comments_and_export() {
        let got =
            parse_env("# c\nA=1\nexport B = \"two words\"\nC='x'\nnoequals\n=novalue\nD=a=b\n");
        assert_eq!(
            got,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two words".into()),
                ("C".into(), "x".into()),
                ("D".into(), "a=b".into())
            ]
        );
    }

    #[test]
    fn real_variable_beats_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "\u{feff}X=file\nY=file\n").unwrap();
        let mut env = Env::from_pairs([
            ("LOOP_DATA_DIR", dir.path().to_str().unwrap()),
            ("X", "real"),
        ]);
        let used = load_all(&mut env);
        assert_eq!(used.len(), 1);
        assert_eq!(env.get("X").as_deref(), Some("real"));
        assert_eq!(env.get("Y").as_deref(), Some("file"));
    }

    #[test]
    fn explicit_file_is_the_only_one_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=data\n").unwrap();
        let explicit = dir.path().join("other.env");
        std::fs::write(&explicit, "B=explicit\n").unwrap();
        let mut env = Env::from_pairs([
            ("LOOP_DATA_DIR", dir.path().to_str().unwrap()),
            ("LOOP_ENV_FILE", explicit.to_str().unwrap()),
        ]);
        load_all(&mut env);
        assert_eq!(env.get("A"), None);
        assert_eq!(env.get("B").as_deref(), Some("explicit"));
    }

    #[test]
    fn home_dir_uses_userprofile_not_home() {
        let env = Env::from_pairs([("USERPROFILE", r"C:\Users\x"), ("HOME", r"C:\ignored")]);
        assert_eq!(env.home_dir(), PathBuf::from(r"C:\Users\x"));
    }
}
