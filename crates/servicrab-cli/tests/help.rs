//! The `--help` output as a contract.
//!
//! The `clap` doc comments in `main.rs` *are* `servicrab --help`, which makes
//! them the one piece of documentation a v1.0 promise applies to directly: an
//! operator's muscle memory and a script's `--help | grep` both read it.  Two
//! things can break that without breaking a compile, and neither had a test:
//!
//! - a reword.  A lint or a tidy-up that improves a sentence changes the page.
//! - a layout switch.  Attaching per-value help to an enum-valued flag makes
//!   `clap` render *every* page in its long form — one flag per paragraph, help
//!   on its own line, blank lines between — for the whole binary.  It has been
//!   hit once already, by `--color`, and the symptom is not local to the flag
//!   that caused it.
//!
//! What is pinned is the flag set and the wording, with runs of whitespace
//! collapsed.  Alignment is deliberately *not* pinned: `clap` aligns the
//! description column to the longest flag on the page, so adding a flag shifts
//! every other line on it — and adding a flag is exactly what 1.x is allowed to
//! do.  A byte-exact snapshot would forbid what semver permits, be rewritten
//! whenever it complained, and pin nothing by the second time.
//!
//! Adding a flag therefore means adding a row here.  Rewording an existing one
//! means changing a row that says it is frozen, which is the point.

use std::process::Command;

/// One page's option list: the flag as `clap` spells it, and its description
/// with runs of whitespace collapsed to one space.
type Page = (&'static str, &'static [(&'static str, &'static str)]);

/// What `--color`'s description says on every page it appears on.
///
/// Spelled once because it is global, so it is on all of them, and twenty
/// copies of one sentence is twenty chances for them to disagree.
const COLOR: &str = "When to colour output: auto (a stream that is a terminal), always, never \
                     [default: auto] [possible values: auto, always, never]";

/// What `--no-color`'s description says, likewise.
const NO_COLOR: &str = "Never colour output; the same as --color=never";

/// What `--config`'s description says, likewise.
const CONFIG: &str = "Path to the configuration file. If omitted, discovers servicrab.toml by \
                      walking up from the current directory";

/// A page rendered in `clap`'s long layout puts each description on its own
/// line, so there is no description beside the flag to record.
///
/// `generate` is the only page that is legitimately in the long layout — its
/// value enums carry per-value help of their own — and it is here so that a
/// change *to* the long layout elsewhere shows up as a page that started
/// claiming this.
const LONG: &str = "<the long layout>";

/// Every page, and the options it offers.
///
/// The empty name is the top-level page.  `help` is left out: it is `clap`'s
/// own, and `servicrab help --help` is an error rather than a page.
const PAGES: &[Page] = &[
    (
        "",
        &[
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
            ("-V, --version", "Print version"),
        ],
    ),
    (
        "init",
        &[
            ("--color <WHEN>", COLOR),
            (
                "--path <PATH>",
                "Where to write the config file [default: servicrab.toml]",
            ),
            ("--force", "Overwrite the file if it already exists"),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "check",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            (
                "--json",
                "Print machine-readable JSON instead of the human-readable report. Validation \
                 errors become a JSON list",
            ),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "list",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            (
                "--json",
                "Output in JSON format instead of the human-readable table",
            ),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "run",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            (
                "--no-restart",
                "Never restart the service, whatever the configured policy says",
            ),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "exec",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "up",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            (
                "--profile <NAME>",
                "Also start the services in this profile. Repeatable. Cannot be combined with \
                 naming services",
            ),
            (
                "--no-restart",
                "Never restart services, whatever their configured policy says",
            ),
            (
                "--no-prefix",
                "Do not prefix output lines with the service name",
            ),
            ("--timestamps", "Prefix output lines with a UTC timestamp"),
            (
                "--abort-on-failure",
                "Stop the whole stack as soon as one service fails",
            ),
            (
                "--json",
                "Print one JSON event per line on stdout instead of rendering for a terminal",
            ),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "watch",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            (
                "--profile <NAME>",
                "Also start the services in this profile. Repeatable. Cannot be combined with \
                 naming services",
            ),
            (
                "--no-restart",
                "Never restart services, whatever their configured policy says",
            ),
            (
                "--no-prefix",
                "Do not prefix output lines with the service name",
            ),
            ("--timestamps", "Prefix output lines with a UTC timestamp"),
            (
                "--abort-on-failure",
                "Stop the whole stack as soon as one service fails",
            ),
            (
                "--json",
                "Print one JSON event per line on stdout instead of rendering for a terminal",
            ),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "logs",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            (
                "-f, --follow",
                "Keep printing new lines as they are written",
            ),
            ("--no-color", NO_COLOR),
            (
                "-n, --lines <LINES>",
                "Number of trailing lines to show per service [default: 50]",
            ),
            (
                "--no-prefix",
                "Do not prefix output lines with the service name",
            ),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "start",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            (
                "--profile <NAME>",
                "Also supervise the services in this profile. Repeatable. The daemon keeps the \
                 set, so `reload` plans the same stack. Cannot be combined with naming services",
            ),
            (
                "--no-restart",
                "Never restart services, whatever their configured policy says",
            ),
            (
                "--wait",
                "Return only once every started service is ready — running, and health-checked \
                 if it declares a health check. Exits non-zero if a service fails or the timeout \
                 runs out",
            ),
            (
                "--timeout <TIMEOUT>",
                "How long --wait waits before giving up, e.g. `90s` or `2m`",
            ),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "stop",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "restart",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "reload",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "events",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            (
                "--json",
                "Print one JSON object per line instead of rendering for a terminal",
            ),
            ("--no-color", NO_COLOR),
            ("--no-prefix", "Do not prefix lines with the service name"),
            ("-t, --timestamps", "Prefix lines with a UTC timestamp"),
            (
                "--no-logs",
                "Leave captured stdout/stderr out of the stream",
            ),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "status",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--json", "Print machine-readable JSON instead of a table"),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "down",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "generate",
        &[
            ("-c, --config <CONFIG>", LONG),
            ("--color <WHEN>", LONG),
            ("--no-color", LONG),
            ("--scope <SCOPE>", LONG),
            ("-o, --output <OUTPUT>", LONG),
            ("--user <USER>", LONG),
            ("--profile <NAME>", LONG),
            ("-h, --help", LONG),
        ],
    ),
    (
        "completions",
        &[
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "man",
        &[
            ("--color <WHEN>", COLOR),
            (
                "-o, --output <OUTPUT>",
                "Write one page per command into this directory instead, creating it if needed",
            ),
            ("--no-color", NO_COLOR),
            ("-h, --help", "Print help"),
        ],
    ),
    (
        "daemon",
        &[
            ("-c, --config <CONFIG>", CONFIG),
            ("--color <WHEN>", COLOR),
            ("--no-color", NO_COLOR),
            (
                "--profile <NAME>",
                "Also supervise the services in this profile. Repeatable",
            ),
            (
                "--no-restart",
                "Never restart services, whatever their configured policy says",
            ),
            ("-h, --help", "Print help"),
        ],
    ),
];

fn help(page: &str) -> String {
    let binary = assert_cmd::cargo::cargo_bin("servicrab");
    let mut command = Command::new(binary);
    if !page.is_empty() {
        command.arg(page);
    }
    // `TERM` and the rest are irrelevant to `--help`, but `COLUMNS` is not:
    // `clap` wraps to the terminal width, and a narrow one would fold the long
    // descriptions differently.  Set wide, and collapsed whitespace absorbs the
    // rest.
    let output = command
        .arg("--help")
        .env("COLUMNS", "400")
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab");
    assert!(
        output.status.success(),
        "`servicrab {page} --help` exited {:?}",
        output.status.code()
    );
    String::from_utf8(output.stdout).expect("--help is UTF-8")
}

/// The flag and description read off one page, whitespace collapsed.
fn options(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("Options:") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        // A page's option list runs to the first line that is neither indented
        // nor blank; `clap`'s long layout has blank lines inside it.
        if !line.starts_with(' ') && !line.trim().is_empty() {
            break;
        }
        let Some((flag, rest)) = split_flag(line) else {
            continue;
        };
        found.push((flag, collapse(&rest)));
    }
    found
}

/// Split an option line into the flag and whatever follows it, if it starts
/// with a flag at all: a description's own continuation lines do not, and
/// neither do the `- value: …` bullets the long layout lists a value enum with.
fn split_flag(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let rest_of_flag = trimmed.strip_prefix("--").or_else(|| {
        trimmed
            .strip_prefix('-')
            .filter(|rest| !rest.starts_with('-'))
    })?;
    // `- system: a system-wide unit` is a bullet, not a flag; a flag's name
    // begins immediately after its dashes.
    if !rest_of_flag.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    // The flag is everything up to the run of spaces that opens the description
    // column, or the whole of it in the long layout, where there is none.
    let (flag, rest) = match trimmed.find("  ") {
        Some(gap) => (&trimmed[..gap], &trimmed[gap..]),
        None => (trimmed, ""),
    };
    let rest = if rest.trim().is_empty() {
        LONG.to_string()
    } else {
        rest.to_string()
    };
    Some((flag.trim().to_string(), rest))
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Rewording an existing flag's help, or dropping one, is a breaking change to
/// a v1.0 contract, and nothing else would notice.
#[test]
fn every_help_page_offers_the_flags_it_promised_with_the_wording_it_promised() {
    for (page, expected) in PAGES {
        let actual = options(&help(page));
        let name = if page.is_empty() { "servicrab" } else { page };

        let actual_flags: Vec<&str> = actual.iter().map(|(flag, _)| flag.as_str()).collect();
        let expected_flags: Vec<&str> = expected.iter().map(|(flag, _)| *flag).collect();
        assert_eq!(
            actual_flags, expected_flags,
            "the flags of `{name} --help` changed"
        );

        for ((flag, actual_help), (_, promised)) in actual.iter().zip(expected.iter()) {
            assert_eq!(
                actual_help,
                &collapse(promised),
                "the help for `{flag}` on `{name} --help` was reworded"
            );
        }
    }
}

/// The trap this file exists for: per-value help on an enum-valued flag puts
/// `clap` into its long layout for the *whole binary*, not just that flag's
/// page, so the damage shows up nowhere near the change that caused it.
/// `generate` is the one page that is meant to be in the long layout.
#[test]
fn only_the_page_with_per_value_help_is_in_claps_long_layout() {
    for (page, expected) in PAGES {
        let long = expected.iter().any(|(_, help)| *help == LONG);
        let actual = options(&help(page));
        let name = if page.is_empty() { "servicrab" } else { page };

        assert!(
            !actual.is_empty(),
            "`{name} --help` listed no options at all"
        );
        assert_eq!(
            actual.iter().all(|(_, help)| help == LONG),
            long,
            "`{name} --help` switched layout — per-value help on a flag does this to every page"
        );
    }
}

/// Every page is reachable and reported, so a subcommand added without a row
/// here fails rather than going unchecked.
#[test]
fn every_subcommand_the_binary_offers_has_a_pinned_help_page() {
    let top = help("");
    let listed: Vec<&str> = top
        .lines()
        .skip_while(|line| !line.starts_with("Commands:"))
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .filter_map(|line| line.split_whitespace().next())
        // `help` is clap's own, and `servicrab help --help` is an error.
        .filter(|name| *name != "help")
        .collect();

    let pinned: Vec<&str> = PAGES
        .iter()
        .map(|(page, _)| *page)
        .filter(|page| !page.is_empty())
        .collect();

    assert_eq!(listed, pinned, "the set of subcommands changed");
}
