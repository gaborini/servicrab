//! Terminal styling helpers.
//!
//! Servicrab deliberately avoids a colour crate: the handful of ANSI escapes it
//! needs are easier to audit than another dependency.  Colour is disabled when
//! the stream is not a terminal, when `NO_COLOR` is set, or when
//! `TERM=dumb` — the usual conventions.
//!
//! The stream matters, and it is the one being written to: most of what
//! servicrab renders is progress and diagnostics, which go to stderr, while a
//! supervised service's stdout goes to stdout.  Deciding both from stdout's
//! terminal-ness got it wrong in both directions — `servicrab up 2> stack.err`
//! put escapes in the file, `servicrab up | cat` dropped the colour stderr was
//! still entitled to — so every decision here names the stream it is about.

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Reset all attributes.
pub const RESET: &str = "\x1b[0m";
/// Dim / faint text.
pub const DIM: &str = "\x1b[2m";
/// Bold text.
pub const BOLD: &str = "\x1b[1m";

/// Green, for healthy/running things.
pub const GREEN: &str = "\x1b[32m";
/// Yellow, for transient states.
pub const YELLOW: &str = "\x1b[33m";
/// Red, for failures.
pub const RED: &str = "\x1b[31m";

/// The colours cycled through when prefixing service output.
pub const SERVICE_COLORS: [&str; 6] = [
    "\x1b[36m", // cyan
    "\x1b[32m", // green
    "\x1b[33m", // yellow
    "\x1b[35m", // magenta
    "\x1b[34m", // blue
    "\x1b[31m", // red
];

/// Whether coloured output should be produced for `stream`.
pub fn color_enabled_for(stream: Stream) -> bool {
    decide(choice(), Environment::current(), stream.is_terminal())
}

/// Which stream a colour decision is being made about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Command output, and the stdout of supervised services.
    Stdout,
    /// Progress, status lines and diagnostics.
    Stderr,
}

impl Stream {
    fn is_terminal(self) -> bool {
        match self {
            Stream::Stdout => std::io::stdout().is_terminal(),
            Stream::Stderr => std::io::stderr().is_terminal(),
        }
    }
}

/// What `--color` was set to.
///
/// `Auto` colours a stream when it is a terminal; `Always` colours both streams
/// whatever they are; `Never` colours neither.
///
/// The variants carry no clap help of their own on purpose: per-value help
/// switches every `--help` page to clap's long layout, and the v1.0 help output
/// is a frozen contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

/// The `--color` value for this process.
///
/// A command-line choice is one value for the whole run, and threading it
/// through every renderer's constructor would put a parameter on code that has
/// no other reason to know about it, so it is recorded once in [`set_choice`]
/// and read from here.
static CHOICE: OnceLock<ColorChoice> = OnceLock::new();

/// Record what `--color` asked for, before any output is rendered.
///
/// Later calls are ignored: the choice is made once, on the command line.
pub fn set_choice(choice: ColorChoice) {
    let _ = CHOICE.set(choice);
}

fn choice() -> ColorChoice {
    CHOICE.get().copied().unwrap_or_default()
}

/// The colour-related environment, sampled once per decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Environment {
    /// `NO_COLOR` is set, to anything at all.
    no_color: bool,
    /// `CLICOLOR_FORCE` is set to something other than `0`.
    clicolor_force: bool,
    /// `TERM` says the terminal cannot render escapes.
    dumb_term: bool,
}

impl Environment {
    fn current() -> Self {
        Self {
            no_color: std::env::var_os("NO_COLOR").is_some(),
            clicolor_force: matches!(
                std::env::var("CLICOLOR_FORCE").as_deref(),
                Ok(value) if !value.is_empty() && value != "0"
            ),
            dumb_term: matches!(std::env::var("TERM").as_deref(), Ok("dumb")),
        }
    }
}

/// Resolve one colour decision from the choice, the environment and whether the
/// stream is a terminal.
///
/// The order is what makes this predictable, and it runs from the most
/// deliberate signal to the least:
///
/// - `--color` was typed on this command line, so it wins over everything.
/// - `NO_COLOR` is a standing "no" from the operator's environment; nothing
///   short of the flag overrides it, which also keeps it usable as the one
///   switch a test or a build script can rely on.
/// - `CLICOLOR_FORCE` answers only the terminal test — "colour this even though
///   it is a pipe" — so it beats both remaining signals.
/// - `TERM=dumb` is a terminal that cannot render escapes.
/// - Otherwise: colour a terminal, leave a pipe or a file alone.
fn decide(choice: ColorChoice, env: Environment, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => return true,
        ColorChoice::Never => return false,
        ColorChoice::Auto => {}
    }
    if env.no_color {
        return false;
    }
    if env.clicolor_force {
        return true;
    }
    if env.dumb_term {
        return false;
    }
    is_terminal
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

    /// A `TERM` that renders escapes, no `NO_COLOR`, no `CLICOLOR_FORCE`.
    fn plain() -> Environment {
        Environment::default()
    }

    #[test]
    fn a_terminal_is_coloured_and_a_pipe_is_not() {
        assert!(decide(ColorChoice::Auto, plain(), true));
        assert!(!decide(ColorChoice::Auto, plain(), false));
    }

    #[test]
    fn the_flag_wins_over_the_environment_and_the_terminal() {
        let hostile = Environment {
            no_color: true,
            dumb_term: true,
            ..plain()
        };
        assert!(decide(ColorChoice::Always, hostile, false));

        let inviting = Environment {
            clicolor_force: true,
            ..plain()
        };
        assert!(!decide(ColorChoice::Never, inviting, true));
    }

    #[test]
    fn no_color_beats_clicolor_force() {
        let env = Environment {
            no_color: true,
            clicolor_force: true,
            ..plain()
        };
        assert!(!decide(ColorChoice::Auto, env, true));
    }

    #[test]
    fn clicolor_force_colours_a_pipe() {
        let env = Environment {
            clicolor_force: true,
            ..plain()
        };
        assert!(decide(ColorChoice::Auto, env, false));
        // Even a dumb terminal, since forcing is the more specific request.
        let dumb = Environment {
            dumb_term: true,
            ..env
        };
        assert!(decide(ColorChoice::Auto, dumb, false));
    }

    #[test]
    fn a_dumb_terminal_is_left_alone() {
        let env = Environment {
            dumb_term: true,
            ..plain()
        };
        assert!(!decide(ColorChoice::Auto, env, true));
    }

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
