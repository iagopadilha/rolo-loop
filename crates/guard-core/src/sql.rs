// SPDX-License-Identifier: MIT
//! SQL analysis (port of `guard_sql.py`). One left-to-right tokenizer handles quotes and comments together (what is a
//! comment depends on which quote opened first). The text is scanned under every combination of the server modes that
//! change where a literal or a comment ends; an unterminated quote or comment FAILS CLOSED.

use crate::pyre::Re;
use std::sync::LazyLock;

const WRITE_KEYWORDS: &[&str] = &[
    "insert",
    "update",
    "delete",
    "replace",
    "merge",
    "create",
    "alter",
    "drop",
    "truncate",
    "rename",
    "load",
    "copy",
    "grant",
    "revoke",
    "call",
    "do",
    "prepare",
    "execute",
    "exec",
    "deallocate",
    "kill",
    "handler",
    "flush",
    "lock",
    "unlock",
    "reset",
    "purge",
    "optimize",
    "repair",
    "analyze",
    "install",
    "uninstall",
    "shutdown",
    "restart",
    "clone",
    "import",
    "change",
    "stop",
    "xa",
    "binlog",
    "cache",
    "vacuum",
    "reindex",
];
pub const SQL_UNPARSEABLE: &str = "\u{0}unparseable";
const COMMENT_MARK: char = '\u{1}';

static SET_WRITE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^set\s+(?:global|persist|persist_only|@@global\.|@@persist\.|@@persist_only\.|password|role|default\s+role|resource\s+group|binary_log)"));
static START_WRITE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^start\s+(?:replica|slave|group_replication)"));
static STRONG_WRITE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r#"^\s*(?:delete\s+from|insert\s+(?:ignore\s+)?into|update\s+[\w.`'"]+\s+set|replace\s+into|drop\s+\w|truncate\s+\w|alter\s+\w|create\s+\w|grant\s+\w|revoke\s+\w|rename\s+table|load\s+data|call\s+\w|prepare\s+\w|execute\s+\w|deallocate\s+\w|kill\s+\w|flush\s+\w|lock\s+tables|handler\s+\w|do\s+\w)"#,
    )
});
static WORDS: LazyLock<Re> = LazyLock::new(|| Re::new(r"[a-z_@.]+"));
static INTO_OUTFILE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\binto\s+(?:outfile|dumpfile)\b"));

/// Index just past the closing quote of the literal opened at `start`, or `None` when it never closes.
fn quoted_end(text: &[char], start: usize, quote: char, backslash_escapes: bool) -> Option<usize> {
    let mut index = start + 1;
    let size = text.len();
    while index < size {
        let c = text[index];
        if backslash_escapes && c == '\\' && quote != '`' {
            index += 2;
            continue;
        }
        if c == quote {
            if text.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

fn is_lstrip_char(c: char) -> bool {
    matches!(c, '(' | ' ' | '\t' | '\r' | '\n')
}

/// `(statements, complete)`.
fn scan(text: &[char], backslash_escapes: bool, strict_dashes: bool) -> (Vec<String>, bool) {
    let mut statements: Vec<String> = Vec::new();
    let mut current = String::new();
    let (mut index, size) = (0usize, text.len());
    let mut executable_depth = 0usize;
    let mut complete = true;
    while index < size {
        let c = text[index];
        let next = text.get(index + 1).copied();
        if c == '\'' || c == '"' || c == '`' {
            match quoted_end(text, index, c, backslash_escapes) {
                None => {
                    complete = false;
                    break;
                }
                Some(end) => {
                    current.push_str("''");
                    index = end;
                }
            }
        } else if c == '/' && next == Some('*') {
            let third = text.get(index + 2).copied();
            let is_exec = third == Some('!') || (third == Some('M') && text.get(index + 3) == Some(&'!'));
            if is_exec {
                index += if third == Some('!') { 3 } else { 4 };
                while index < size && text[index].is_ascii_digit() {
                    index += 1;
                }
                executable_depth += 1;
                current.push(' ');
            } else {
                let end = (index + 2..size.saturating_sub(1)).find(|&i| text[i] == '*' && text[i + 1] == '/');
                match end {
                    None => {
                        complete = false;
                        break;
                    }
                    Some(end) => {
                        current.push(COMMENT_MARK);
                        index = end + 2;
                    }
                }
            }
        } else if c == '*' && next == Some('/') && executable_depth > 0 {
            executable_depth -= 1;
            current.push(' ');
            index += 2;
        } else if c == '#'
            || (c == '-'
                && next == Some('-')
                && (!strict_dashes || index + 2 >= size || matches!(text[index + 2], ' ' | '\t' | '\r' | '\n' | '\u{c}' | '\u{b}') || (text[index + 2] as u32) < 32))
        {
            let end = (index..size).find(|&i| text[i] == '\n');
            index = end.unwrap_or(size);
            current.push(COMMENT_MARK);
        } else if c == ';' {
            statements.push(std::mem::take(&mut current));
            index += 1;
        } else {
            current.extend(c.to_lowercase());
            index += 1;
        }
    }
    if executable_depth > 0 {
        complete = false;
    }
    statements.push(current);
    let mut result: Vec<String> = Vec::new();
    for part in statements {
        let pieces: Vec<&str> = part.split(COMMENT_MARK).collect();
        let joined = pieces.join(" ");
        let whole = joined.trim().trim_start_matches(is_lstrip_char).to_string();
        if !whole.trim_matches(is_lstrip_char).is_empty() {
            result.push(whole);
        }
        for piece in pieces.iter().skip(1) {
            let stripped = piece.trim();
            if STRONG_WRITE.match_start(stripped.trim_start_matches(is_lstrip_char)).is_some() {
                result.push(stripped.to_string());
            }
        }
    }
    (result, complete)
}

/// Statements of a SQL text, lowercased, comments removed and literals replaced by `''`. Unparseable text adds
/// [`SQL_UNPARSEABLE`], which counts as a write.
pub fn sql_statements(sql: &str) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut found: Vec<String> = Vec::new();
    let mut modes: Vec<(bool, bool)> = vec![(true, true)];
    if sql.contains('\\') {
        modes.push((false, true));
    }
    if sql.contains("--") {
        modes.push((true, false));
        if sql.contains('\\') {
            modes.push((false, false));
        }
    }
    for (position, (backslash_escapes, strict_dashes)) in modes.into_iter().enumerate() {
        let (statements, complete) = scan(&chars, backslash_escapes, strict_dashes);
        for statement in statements {
            if !found.contains(&statement) {
                found.push(statement);
            }
        }
        if position == 0 && !complete {
            found.push(SQL_UNPARSEABLE.to_string());
        }
    }
    found
}

/// The keyword that makes a statement write (or administer, or run something); empty when it reads.
pub fn sql_statement_writes(statement: &str) -> String {
    if statement == SQL_UNPARSEABLE {
        return "UNPARSEABLE SQL".to_string();
    }
    let words: Vec<String> = WORDS.find_all(statement).into_iter().map(|m| m.text().to_string()).collect();
    let first = words.first().map(String::as_str).unwrap_or("");
    if WRITE_KEYWORDS.contains(&first) {
        return first.to_uppercase();
    }
    if first == "set" && SET_WRITE.match_start(statement).is_some() {
        return "SET GLOBAL".to_string();
    }
    if first == "start" && START_WRITE.match_start(statement).is_some() {
        return "START REPLICA".to_string();
    }
    if ["with", "explain", "describe", "desc"].contains(&first) {
        if first != "with" && !words.iter().any(|w| w == "analyze") {
            return String::new();
        }
        for (position, word) in words.iter().enumerate() {
            if !["insert", "update", "delete", "replace"].contains(&word.as_str()) {
                continue;
            }
            if word == "update" && position > 0 && words[position - 1] == "for" {
                continue;
            }
            if word == "insert" || word == "replace" {
                let call = Re::cached(&format!(r"\b{word}\s*\("), false);
                let stmt = Re::cached(&format!(r"\b{word}\s+(?:into|ignore|low_priority|delayed|high_priority)\b"), false);
                if call.is_match(statement) && !stmt.is_match(statement) {
                    continue;
                }
            }
            return word.to_uppercase();
        }
    }
    if INTO_OUTFILE.is_match(statement) {
        return "SELECT INTO OUTFILE".to_string();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(sql: &str) -> String {
        sql_statements(sql).iter().map(|s| sql_statement_writes(s)).find(|k| !k.is_empty()).unwrap_or_default()
    }

    #[test]
    fn reads_pass_and_writes_are_named() {
        assert_eq!(writes("SELECT * FROM t"), "");
        assert_eq!(writes("DELETE FROM t"), "DELETE");
        assert_eq!(writes("select 1; drop table t"), "DROP");
        assert_eq!(writes("SET GLOBAL x=1"), "SET GLOBAL");
        assert_eq!(writes("SELECT 1 INTO OUTFILE '/x'"), "SELECT INTO OUTFILE");
    }

    #[test]
    fn comment_and_string_tricks_do_not_hide_a_write() {
        assert_eq!(writes("SELECT '/*'; DELETE FROM t; SELECT '*/'"), "DELETE");
        assert_eq!(writes("SELECT 1 -- '\nDELETE FROM t; -- '"), "DELETE");
        assert_eq!(writes("/*!50000 DROP TABLE t */"), "DROP");
        assert_eq!(writes("SELECT 'a\\'; DELETE FROM t; --'"), "DELETE");
    }

    #[test]
    fn unterminated_fails_closed() {
        assert_eq!(writes("SELECT 'oops"), "UNPARSEABLE SQL");
        assert_eq!(writes("SELECT /* never closed"), "UNPARSEABLE SQL");
    }

    #[test]
    fn cte_and_explain() {
        assert_eq!(writes("WITH x AS (SELECT 1) DELETE FROM t"), "DELETE");
        assert_eq!(writes("EXPLAIN SELECT 1"), "");
        assert_eq!(writes("EXPLAIN ANALYZE DELETE FROM t"), "DELETE");
        assert_eq!(writes("SELECT a FROM t FOR UPDATE"), "");
        assert_eq!(writes("WITH x AS (SELECT insert(a,1,1,'b')) SELECT 1"), "");
    }
}
