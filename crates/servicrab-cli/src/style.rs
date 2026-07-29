//! Terminal styling helpers.
//!
//! Servicrab deliberately avoids a colour crate: the handful of ANSI escapes it
//! needs are easier to audit than another dependency.  Colour is disabled when
//! the stream is not a terminal, when `NO_COLOR` is set, or when
//! `TERM=dumb` — the usual conventions.

use std::io::IsTerminal;

/// Reset all attributes.
pub const RESET: &str = "\x1b[0m";
/// Dim / faint text.
pub const DIM: &str = "\x1b[2m";
/// Bold text.
pub const BOLD: &str = "\x1b[1m";

/// The colours cycled through when prefixing service output.
pub const SERVICE_COLORS: [&str; 6] = [
    "\x1b[36m", // cyan
    "\x1b[32m", // green
    "\x1b[33m", // yellow
    "\x1b[35m", // magenta
    "\x1b[34m", // blue
    "\x1b[31m", // red
];

/// Whether coloured output should be produced for stdout.
pub fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if matches!(std::env::var("TERM").as_deref(), Ok("dumb")) {
        return false;
    }
    std::io::stdout().is_terminal()
}

/// Wrap `text` in `code` when `enabled`, otherwise return it unchanged.
pub fn paint(enabled: bool, code: &str, text: &str) -> String {
    if enabled {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Format the current wall-clock time as `HH:MM:SS` in UTC.
///
/// Servicrab has no date/time dependency, and a plain UTC clock is enough for
/// interleaving log lines.
pub fn utc_hms(now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn paint_is_a_no_op_when_disabled() {
        assert_eq!(paint(false, BOLD, "api"), "api");
    }

    #[test]
    fn paint_wraps_text_when_enabled() {
        assert_eq!(paint(true, BOLD, "api"), format!("{BOLD}api{RESET}"));
    }

    #[test]
    fn utc_hms_formats_seconds_since_midnight() {
        assert_eq!(utc_hms(UNIX_EPOCH), "00:00:00");
        assert_eq!(utc_hms(UNIX_EPOCH + Duration::from_secs(3661)), "01:01:01");
        assert_eq!(
            utc_hms(UNIX_EPOCH + Duration::from_secs(86_399)),
            "23:59:59"
        );
        // Wraps around into the next day.
        assert_eq!(
            utc_hms(UNIX_EPOCH + Duration::from_secs(86_400)),
            "00:00:00"
        );
    }
}
