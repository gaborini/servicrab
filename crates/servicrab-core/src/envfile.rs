//! Parser for dotenv-style environment files.
//!
//! The format is intentionally small and predictable:
//!
//! ```text
//! # a comment
//! KEY=value
//! export KEY=value          # `export` is accepted and ignored
//! QUOTED="hello world"      # double quotes support \n \r \t \\ \" escapes
//! LITERAL='no $expansion'   # single quotes are taken literally
//! EMPTY=
//! ```
//!
//! Rules:
//!
//! * Blank lines and lines whose first non-blank character is `#` are skipped.
//! * A UTF-8 BOM at the start of the file is ignored.
//! * Keys must be valid environment keys: non-empty, no `=`, no NUL.
//! * Unquoted values are trimmed and may carry a trailing `#` comment when the
//!   `#` is preceded by whitespace.
//! * Quoted values must have a closing quote on the same line.
//!
//! Variable expansion is deliberately *not* supported: what is written is what
//! the service receives.

use std::collections::BTreeMap;
use std::path::Path;

/// Why an environment file could not be turned into key/value pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvFileError {
    /// The file could not be read (missing, unreadable, not UTF-8, ...).
    Read(String),
    /// A line could not be parsed.
    Syntax {
        /// 1-based line number.
        line: usize,
        /// Human-readable reason.
        reason: String,
    },
}

impl std::fmt::Display for EnvFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvFileError::Read(reason) => write!(f, "{reason}"),
            EnvFileError::Syntax { line, reason } => write!(f, "line {line}: {reason}"),
        }
    }
}

/// Read `path` and parse it as a dotenv-style file.
///
/// Later assignments to the same key win, matching the behaviour of a shell
/// sourcing the file top to bottom.
pub fn load(path: &Path) -> Result<BTreeMap<String, String>, EnvFileError> {
    let text = std::fs::read_to_string(path).map_err(|e| EnvFileError::Read(e.to_string()))?;
    parse(&text)
}

/// Parse the contents of a dotenv-style file.
pub fn parse(text: &str) -> Result<BTreeMap<String, String>, EnvFileError> {
    let mut out = BTreeMap::new();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").map_or(line, str::trim_start);

        let Some((key, rest)) = line.split_once('=') else {
            return Err(EnvFileError::Syntax {
                line: line_no,
                reason: format!("expected KEY=VALUE, got {line:?}"),
            });
        };

        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) || key.contains('\0') {
            return Err(EnvFileError::Syntax {
                line: line_no,
                reason: format!("invalid key {key:?}"),
            });
        }

        let value = parse_value(rest.trim_start()).map_err(|reason| EnvFileError::Syntax {
            line: line_no,
            reason,
        })?;

        out.insert(key.to_string(), value);
    }

    Ok(out)
}

fn parse_value(raw: &str) -> Result<String, String> {
    let mut chars = raw.chars();
    match chars.next() {
        Some('"') => unquote(raw, '"', true),
        Some('\'') => unquote(raw, '\'', false),
        _ => Ok(strip_trailing_comment(raw).trim_end().to_string()),
    }
}

/// Parse a quoted value; `raw` starts with `quote`.
fn unquote(raw: &str, quote: char, escapes: bool) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = raw.chars();
    chars.next(); // opening quote

    let mut closed = false;
    while let Some(c) = chars.next() {
        if c == quote {
            closed = true;
            break;
        }
        if escapes && c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('$') => out.push('$'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return Err("value ends with a dangling backslash".to_string()),
            }
            continue;
        }
        out.push(c);
    }

    if !closed {
        return Err(format!("unterminated {quote} quote"));
    }

    let trailing = chars.as_str().trim();
    if !trailing.is_empty() && !trailing.starts_with('#') {
        return Err(format!(
            "unexpected text after the closing quote: {trailing:?}"
        ));
    }

    Ok(out)
}

/// Strip an ` # comment` suffix from an unquoted value.
fn strip_trailing_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return &value[..i];
        }
        i += 1;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> BTreeMap<String, String> {
        parse(text).expect("parse")
    }

    #[test]
    fn plain_assignments_are_parsed() {
        let env = parsed("A=1\nB=two\n");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("two"));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let env = parsed("# leading\n\n  \nA=1\n   # indented\n");
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn export_prefix_is_accepted() {
        let env = parsed("export A=1\n");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn empty_values_are_allowed() {
        let env = parsed("A=\n");
        assert_eq!(env.get("A").map(String::as_str), Some(""));
    }

    #[test]
    fn double_quotes_keep_spaces_and_expand_escapes() {
        let env = parsed("A=\"hello world\\nline\"\n");
        assert_eq!(env.get("A").map(String::as_str), Some("hello world\nline"));
    }

    #[test]
    fn single_quotes_are_literal() {
        let env = parsed("A='no \\n escape'\n");
        assert_eq!(env.get("A").map(String::as_str), Some("no \\n escape"));
    }

    #[test]
    fn trailing_comment_after_an_unquoted_value_is_stripped() {
        let env = parsed("A=1 # why not\n");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn a_hash_inside_a_value_is_kept() {
        let env = parsed("A=ab#cd\n");
        assert_eq!(env.get("A").map(String::as_str), Some("ab#cd"));
    }

    #[test]
    fn a_comment_may_follow_a_quoted_value() {
        let env = parsed("A=\"x\"  # note\n");
        assert_eq!(env.get("A").map(String::as_str), Some("x"));
    }

    #[test]
    fn later_assignments_win() {
        let env = parsed("A=1\nA=2\n");
        assert_eq!(env.get("A").map(String::as_str), Some("2"));
    }

    #[test]
    fn a_line_without_equals_is_an_error() {
        let err = parse("JUST_A_WORD\n").unwrap_err();
        assert!(matches!(err, EnvFileError::Syntax { line: 1, .. }));
    }

    #[test]
    fn an_unterminated_quote_is_an_error() {
        let err = parse("A=\"oops\n").unwrap_err();
        assert!(err.to_string().contains("unterminated"));
    }

    #[test]
    fn text_after_the_closing_quote_is_an_error() {
        let err = parse("A=\"x\" y\n").unwrap_err();
        assert!(err.to_string().contains("after the closing quote"));
    }

    #[test]
    fn a_key_with_spaces_is_an_error() {
        let err = parse("A B=1\n").unwrap_err();
        assert!(err.to_string().contains("invalid key"));
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let env = parsed("\u{feff}A=1\n");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        let err = load(Path::new("/definitely/not/here/.env")).unwrap_err();
        assert!(matches!(err, EnvFileError::Read(_)));
    }
}
