//! The machine-readable half of the CLI's output: exit codes, JSON envelopes,
//! and the one format errors are reported in.
//!
//! These shapes are a contract, so they live in one place rather than being
//! spelled out again in every command.  Three rules hold everywhere:
//!
//! - every `--json` document carries a [`SCHEMA_VERSION`], so a script can
//!   refuse a stream it was not written for instead of guessing;
//! - every error is one `error: …` line on stderr, with its details as bullets
//!   below it — never a bare message, never a `✗`, never on stdout;
//! - under `--json` that error is a JSON object instead, still on stderr, so
//!   stdout carries nothing but the document a caller asked for.

use serde::Serialize;
use servicrab_protocol::{ErrorCode, Response, SCHEMA_VERSION};

/// The command failed.
pub const EXIT_FAILURE: i32 = 1;

/// No daemon is running for this project.
///
/// Its own code because "there is nothing to talk to" is the one failure a
/// script routinely wants to handle rather than report: `down` on an already
/// stopped stack, `status` in a health check, `stop` in a teardown that may run
/// twice.  Telling that apart from a real failure used to mean matching on the
/// message.
pub const EXIT_NO_DAEMON: i32 = 3;

/// Something that went wrong, together with how to report it.
///
/// Built from a `String` wherever a command has nothing more to say, so the
/// ordinary `Err(format!(…))` and `?` paths keep working and only the errors
/// that carry a code or an exit status of their own spell one out.
#[derive(Debug, Clone)]
pub struct CliError {
    code: ErrorCode,
    message: String,
    errors: Vec<String>,
    hint: Option<String>,
    exit: i32,
    json: bool,
}

impl CliError {
    /// An error with a code for a script and a message for a person.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            errors: Vec::new(),
            hint: None,
            exit: EXIT_FAILURE,
            json: false,
        }
    }

    /// The individual problems behind the message.
    pub fn with_errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }

    /// What to do about it, for a person reading the text form.
    ///
    /// Left out of the JSON document on purpose: advice is for the operator,
    /// and a script that wants to act has `code` and `errors`.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Report as JSON rather than as text, because the command was asked for
    /// JSON.
    pub fn in_json(mut self, json: bool) -> Self {
        self.json = json;
        self
    }

    /// Exit with this status instead of [`EXIT_FAILURE`].
    pub fn with_exit(mut self, exit: i32) -> Self {
        self.exit = exit;
        self
    }

    /// The status the process should exit with.
    pub fn exit_code(&self) -> i32 {
        self.exit
    }

    /// Write the error out, in whichever of the two formats applies.
    pub fn report(&self) {
        if self.json {
            let document = ErrorDocument {
                schema_version: SCHEMA_VERSION,
                error: ErrorBody {
                    code: self.code.as_str(),
                    message: &self.message,
                    errors: &self.errors,
                },
            };
            // A JSON error that cannot be rendered still has to reach the
            // operator, and the text form always can.
            match serde_json::to_string(&document) {
                Ok(line) => eprintln!("{line}"),
                Err(_) => self.report_as_text(),
            }
            return;
        }
        self.report_as_text();
    }

    fn report_as_text(&self) {
        eprintln!("error: {}", self.message);
        for problem in &self.errors {
            eprintln!("  • {problem}");
        }
        if let Some(hint) = &self.hint {
            // Stderr, because that is where this whole report goes; deciding it
            // from stdout would colour a hint being redirected into a file.
            let color = crate::style::color_enabled_for(crate::style::Stream::Stderr);
            eprintln!(
                "{}",
                crate::style::paint(color, crate::style::DIM, &format!("  {hint}"))
            );
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        CliError::new(ErrorCode::Failed, message)
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        CliError::new(ErrorCode::Failed, message)
    }
}

/// The error a command reports when no daemon is listening.
pub fn no_daemon(project: &str, json: bool) -> CliError {
    CliError::new(
        ErrorCode::NotRunning,
        format!("no daemon is running for {project} — start one with `servicrab start`"),
    )
    .with_exit(EXIT_NO_DAEMON)
    .in_json(json)
}

/// What every `--json` document looks like from the outside.
#[derive(Serialize)]
struct Document<T> {
    schema_version: u32,
    #[serde(flatten)]
    payload: T,
}

#[derive(Serialize)]
struct ErrorDocument<'a> {
    schema_version: u32,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    errors: &'a [String],
}

/// Print one `--json` document on stdout, wrapped in the envelope.
///
/// Pretty-printed, because these are whole documents a person also reads.  The
/// event streams are the exception and are newline-delimited; see
/// [`print_stream_header`].
pub fn print_document<T: Serialize>(payload: T) -> Result<(), CliError> {
    let document = Document {
        schema_version: SCHEMA_VERSION,
        payload,
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|e| CliError::new(ErrorCode::Failed, format!("could not render JSON: {e}")))?;
    println!("{text}");
    Ok(())
}

/// Open an NDJSON event stream with the same handshake line the daemon sends.
///
/// `up --json`, `watch --json` and `events --json` all emit the events the
/// daemon streams, so they all announce the contract the same way: one `ok`
/// line carrying the schema version, then one event per line.  Repeating the
/// version on every event line would be noise in a stream that can run for
/// days.
pub fn print_stream_header() {
    let hello = Response::Ok {
        message: None,
        changes: None,
        schema_version: Some(SCHEMA_VERSION),
    };
    if let Ok(line) = servicrab_protocol::encode(&hello) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_message_becomes_an_unclassified_failure() {
        let error = CliError::from("could not find config".to_string());

        assert_eq!(error.exit_code(), EXIT_FAILURE);
        assert_eq!(error.to_string(), "could not find config");
    }

    /// The dedicated code is the whole point: a teardown script has to be able
    /// to tell "nothing was running" from "the command broke".
    #[test]
    fn an_absent_daemon_has_an_exit_code_of_its_own() {
        let error = no_daemon("demo", false);

        assert_eq!(error.exit_code(), EXIT_NO_DAEMON);
        assert!(error.to_string().contains("no daemon is running for demo"));
    }

    #[test]
    fn a_json_document_carries_the_schema_version() {
        let document = Document {
            schema_version: SCHEMA_VERSION,
            payload: serde_json::json!({ "running": false, "services": [] }),
        };

        let json = serde_json::to_value(&document).expect("serialize");
        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        assert_eq!(json["running"], false);
    }

    #[test]
    fn a_json_error_carries_the_schema_version_and_the_code() {
        let error = CliError::new(ErrorCode::ValidationFailed, "2 error(s)")
            .with_errors(vec!["first".to_string(), "second".to_string()])
            .in_json(true);
        let document = ErrorDocument {
            schema_version: SCHEMA_VERSION,
            error: ErrorBody {
                code: error.code.as_str(),
                message: &error.message,
                errors: &error.errors,
            },
        };

        let json = serde_json::to_value(&document).expect("serialize");
        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        assert_eq!(json["error"]["code"], "validation_failed");
        assert_eq!(json["error"]["errors"].as_array().expect("a list").len(), 2);
    }

    /// The stream header is the daemon's own handshake line, so a reader can
    /// treat a foreground `up --json` exactly like a subscription.
    #[test]
    fn the_stream_header_is_the_daemons_handshake_line() {
        let hello = Response::Ok {
            message: None,
            changes: None,
            schema_version: Some(SCHEMA_VERSION),
        };

        let line = servicrab_protocol::encode(&hello).expect("encode");
        assert_eq!(line.trim_end(), r#"{"type":"ok","schema_version":1}"#);
    }
}
