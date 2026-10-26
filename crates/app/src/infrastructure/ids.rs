// SPDX-License-Identifier: MIT
//! Random identifiers (`secrets.token_hex`). Reads the operating system's generator (`BCryptGenRandom` through winproc).

/// `bytes` random bytes as lower-case hex (`2 * bytes` characters).
pub fn random_hex(bytes: usize) -> String {
    winproc::random_bytes(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_has_the_asked_length_and_differs() {
        let (a, b) = (random_hex(6), random_hex(6));
        assert_eq!((a.len(), b.len()), (12, 12));
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(random_hex(0), "");
    }
}
