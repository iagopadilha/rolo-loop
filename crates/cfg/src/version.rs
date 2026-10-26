// SPDX-License-Identifier: MIT
//! Tolerant version comparison: numbers compare as numbers (`0.10.0 > 0.3.0`), a pre-release sorts
//! below its release, and nothing here ever panics.

use regex::Regex;
use std::sync::LazyLock;

static PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*v?(\d+)\.(\d+)(?:\.(\d+))?").expect("valid regex"));
static PRERELEASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*v?\d+(?:\.\d+){0,2}\s*[-+]").expect("valid regex"));

/// `(major, minor, patch)` from `0.3.0` (a pre-release suffix is ignored), or `None`.
pub fn version_tuple(text: &str) -> Option<(u64, u64, u64)> {
    let caps = PREFIX.captures(text)?;
    let part = |i: usize| -> u64 {
        caps.get(i)
            .map(|m| m.as_str().parse::<u64>().unwrap_or(u64::MAX))
            .unwrap_or(0)
    };
    Some((part(1), part(2), part(3)))
}

/// `(major, minor, patch, is_release)`.
pub type VersionKey = (u64, u64, u64, u8);

/// Sortable version key. `0.4.0-beta` sorts before `0.4.0` and after `0.3.9`.
pub fn version_key(text: &str) -> Option<VersionKey> {
    let (a, b, c) = version_tuple(text)?;
    let release = if PRERELEASE.is_match(text) { 0 } else { 1 };
    Some((a, b, c, release))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn numbers_compare_as_numbers() {
        assert!(version_key("0.10.0") > version_key("0.3.0"));
    }

    #[test]
    fn prerelease_below_release_above_previous() {
        assert!(version_key("0.4.0-beta") < version_key("0.4.0"));
        assert!(version_key("0.4.0-beta") > version_key("0.3.9"));
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(version_key("garbage"), None);
        assert_eq!(version_key("not-a-version"), None);
        assert_eq!(version_key(""), None);
    }

    #[test]
    fn missing_patch_is_zero_and_v_prefix_is_ok() {
        assert_eq!(version_tuple(" v1.2 "), Some((1, 2, 0)));
        assert_eq!(version_tuple("0.3.1"), Some((0, 3, 1)));
    }

    proptest! {
        #[test]
        fn never_panics(s in ".*") {
            let _ = version_key(&s);
        }
    }
}
