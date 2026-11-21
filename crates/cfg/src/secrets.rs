// SPDX-License-Identifier: MIT
//! Named secrets of the app (API keys, sign-in tokens, SMTP passwords, webhook header values), kept in `<data dir>/secrets.json` encrypted
//! for the current Windows user (DPAPI, `winproc::protect_for_user`). The file only holds names and encrypted blobs:
//! `{"version": 1, "secrets": {"LOOP_SMTP_PASSWORD": "<hex>"}}`. A value is never written in clear, never logged and never returned by the
//! panel API (only [`masked`]). The engine reads them as environment values through `env::load_all`, below the real environment, the `.env`
//! files and the Settings values, so an existing setup keeps working.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "secrets.json";
const VERSION: u64 = 1;
const NAME_MAX: usize = 64;

pub fn file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// `NAME` in upper case letters, digits and `_`, starting with a letter (the shape of an environment variable).
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase()) && name.len() <= NAME_MAX && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// `••••` plus the last 4 characters of a long value (`••••` alone for a short one): what the panel may show.
pub fn masked(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() >= 12 {
        format!("••••{}", chars[chars.len() - 4..].iter().collect::<String>())
    } else {
        "••••".to_string()
    }
}

fn read_blobs(data_dir: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(file_path(data_dir)) else { return Map::new() };
    serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')).ok().and_then(|v| v.get("secrets").and_then(Value::as_object).cloned()).unwrap_or_default()
}

fn write_blobs(data_dir: &Path, blobs: Map<String, Value>) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = file_path(data_dir);
    let temporary = data_dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(&json!({"version": VERSION, "secrets": blobs})).unwrap_or_default();
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, &path)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// The names stored (sorted), without decrypting anything.
pub fn names(data_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = read_blobs(data_dir).keys().cloned().collect();
    names.sort();
    names
}

/// The clear value of one secret; `None` when it is not stored or cannot be decrypted (another user, another machine, a damaged file).
pub fn get(data_dir: &Path, name: &str) -> Option<String> {
    let blob = from_hex(read_blobs(data_dir).get(name)?.as_str()?)?;
    String::from_utf8(winproc::unprotect_for_user(&blob).ok()?).ok()
}

/// Every secret that can be decrypted, as `(name, value)`.
pub fn all(data_dir: &Path) -> Vec<(String, String)> {
    read_blobs(data_dir)
        .iter()
        .filter_map(|(name, blob)| {
            let bytes = from_hex(blob.as_str()?)?;
            Some((name.clone(), String::from_utf8(winproc::unprotect_for_user(&bytes).ok()?).ok()?))
        })
        .collect()
}

/// Stores (or replaces) one secret. An empty value removes it.
pub fn set(data_dir: &Path, name: &str, value: &str) -> std::io::Result<()> {
    if !is_valid_name(name) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("invalid secret name: {name}")));
    }
    let mut blobs = read_blobs(data_dir);
    if value.is_empty() {
        blobs.remove(name);
    } else {
        blobs.insert(name.to_string(), json!(to_hex(&winproc::protect_for_user(value.as_bytes())?)));
    }
    write_blobs(data_dir, blobs)
}

/// Removes one secret; `true` when it was there.
pub fn remove(data_dir: &Path, name: &str) -> std::io::Result<bool> {
    let mut blobs = read_blobs(data_dir);
    let existed = blobs.remove(name).is_some();
    if existed {
        write_blobs(data_dir, blobs)?;
    }
    Ok(existed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_masks() {
        assert!(is_valid_name("LOOP_SMTP_PASSWORD") && is_valid_name("A1"));
        assert!(!is_valid_name("lower") && !is_valid_name("1A") && !is_valid_name("A-B") && !is_valid_name(""));
        assert_eq!(masked("sk-ant-0123456789abcd"), "••••abcd");
        assert_eq!(masked("short"), "••••");
        assert_eq!(from_hex(&to_hex(&[0, 255, 16])).unwrap(), vec![0, 255, 16]);
        assert!(from_hex("abc").is_none() && from_hex("zz").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn secrets_are_stored_encrypted_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        set(dir.path(), "LOOP_SMTP_PASSWORD", "s3cret-value-1234").unwrap();
        set(dir.path(), "ANTHROPIC_API_KEY", "sk-ant-xyz").unwrap();
        let on_disk = std::fs::read_to_string(file_path(dir.path())).unwrap();
        assert!(!on_disk.contains("s3cret") && !on_disk.contains("sk-ant"));
        assert_eq!(get(dir.path(), "LOOP_SMTP_PASSWORD").as_deref(), Some("s3cret-value-1234"));
        assert_eq!(names(dir.path()), vec!["ANTHROPIC_API_KEY", "LOOP_SMTP_PASSWORD"]);
        assert_eq!(all(dir.path()).len(), 2);
        set(dir.path(), "ANTHROPIC_API_KEY", "").unwrap();
        assert!(remove(dir.path(), "LOOP_SMTP_PASSWORD").unwrap());
        assert!(names(dir.path()).is_empty() && get(dir.path(), "LOOP_SMTP_PASSWORD").is_none());
        assert!(set(dir.path(), "bad name", "x").is_err());
    }
}
