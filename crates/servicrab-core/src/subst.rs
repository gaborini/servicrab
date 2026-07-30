//! `${VAR}` substitution in configuration values.
//!
//! Every string *value* in `servicrab.toml` is expanded against the process
//! environment before validation, so a config can carry machine-specific paths
//! without every checkout editing the file.  Table **keys** are not expanded,
//! and neither are the names that identify things — the project name and the
//! service names, the latter being keys anyway.
//!
//! An unset variable is an error, not an empty string: a `cwd` that silently
//! becomes `/` or a `command` that silently loses an argument is worse than a
//! config that refuses to load.  Write `${VAR:-default}` to allow an absence.
//!
//! The forms follow Docker Compose, because that is where anyone writing this
//! has met the syntax before:
//!
//! | Written | Expands to |
//! |---|---|
//! | `${VAR}` | the value; an error when unset |
//! | `${VAR:-default}` | `default` when unset **or empty** |
//! | `${VAR-default}` | `default` when unset |
//! | `$${VAR}` | a literal `${VAR}` |
//!
//! Unlike Compose, the braces are required: `${VAR}` is the only sequence with
//! a meaning here, and a bare `$` never has one.  Half of the commands in a
//! process manager are shell snippets, and a config language that quietly ate
//! the `$i` of `while ...; do echo $i; done` — at load time, against the wrong
//! environment — would be a worse trade than the lost keystrokes.
//!
//! Note for anyone adding a field to [`crate::raw`]: string fields do not
//! expand by themselves.  Each one needs a line in [`expand_raw`] below, which
//! is why that function walks the fields in declaration order.

use std::collections::BTreeMap;

use crate::error::ConfigError;
use crate::raw::{RawConfig, RawEnvFile, RawHealthCheck, RawLogs, RawService, RawWatch};

/// The variables a config may refer to.
pub(crate) type Vars = BTreeMap<String, String>;

/// Why one value could not be expanded.
enum SubstError {
    /// A reference to a variable that is not set.
    Undefined(String),
    /// A `${...}` that is not one of the accepted forms.
    Malformed(String),
}

/// Expand every value of `raw` in place, returning all failures at once.
pub(crate) fn expand_raw(raw: &mut RawConfig, vars: &Vars) -> Vec<ConfigError> {
    let mut expander = Expander {
        vars,
        errors: Vec::new(),
    };

    // The project name is deliberately absent: it decides where the daemon
    // keeps its socket and state, and a stack whose control socket moves with
    // the environment is a debugging trap rather than a feature.
    expander.map_values(&mut raw.project.env, "project", "env");
    expander.env_file(raw.project.env_file.as_mut(), "project");
    if let Some(logs) = raw.project.logs.as_mut() {
        expander.logs(logs);
    }

    for (name, service) in raw.services.iter_mut() {
        let scope = format!("service {name:?}");
        expander.service(service, &scope);
    }

    expander.errors
}

/// Expands values in place, collecting failures with the location they came
/// from — the same `scope`/`field` wording the rest of the config errors use.
struct Expander<'a> {
    vars: &'a Vars,
    errors: Vec<ConfigError>,
}

impl Expander<'_> {
    fn service(&mut self, service: &mut RawService, scope: &str) {
        // `depends_on` and `profiles` are left alone: they name services,
        // groups, and conditions from a closed set, so a variable there could
        // only make the shape of the stack depend on who started it.
        self.list(&mut service.command, scope, "command");
        self.maybe(service.cwd.as_mut(), scope, "cwd");
        self.map_values(&mut service.env, scope, "env");
        self.env_file(service.env_file.as_mut(), scope);
        self.maybe(service.restart_delay.as_mut(), scope, "restart_delay");
        self.maybe(
            service.restart_max_delay.as_mut(),
            scope,
            "restart_max_delay",
        );
        self.maybe(service.stable_after.as_mut(), scope, "stable_after");
        self.maybe(service.shutdown_signal.as_mut(), scope, "shutdown_signal");
        self.maybe(service.shutdown_timeout.as_mut(), scope, "shutdown_timeout");
        if let Some(health) = service.health.as_mut() {
            self.health(health, scope);
        }
        if let Some(watch) = service.watch.as_mut() {
            self.watch(watch, scope);
        }
    }

    fn health(&mut self, health: &mut RawHealthCheck, scope: &str) {
        if let Some(command) = health.command.as_mut() {
            self.list(command, scope, "health.command");
        }
        self.maybe(health.http.as_mut(), scope, "health.http");
        self.maybe(health.tcp.as_mut(), scope, "health.tcp");
        self.maybe(health.interval.as_mut(), scope, "health.interval");
        self.maybe(health.timeout.as_mut(), scope, "health.timeout");
        self.maybe(health.start_period.as_mut(), scope, "health.start_period");
        self.maybe(health.on_unhealthy.as_mut(), scope, "health.on_unhealthy");
    }

    fn watch(&mut self, watch: &mut RawWatch, scope: &str) {
        self.list(&mut watch.paths, scope, "watch.paths");
        self.list(&mut watch.ignore, scope, "watch.ignore");
        self.maybe(watch.interval.as_mut(), scope, "watch.interval");
        self.maybe(watch.debounce.as_mut(), scope, "watch.debounce");
    }

    fn logs(&mut self, logs: &mut RawLogs) {
        self.maybe(logs.dir.as_mut(), "project.logs", "dir");
        self.maybe(logs.max_size.as_mut(), "project.logs", "max_size");
    }

    fn env_file(&mut self, env_file: Option<&mut RawEnvFile>, scope: &str) {
        match env_file {
            Some(RawEnvFile::One(path)) => self.one(path, scope, "env_file"),
            Some(RawEnvFile::Many(paths)) => self.list(paths, scope, "env_file"),
            None => {}
        }
    }

    fn one(&mut self, value: &mut String, scope: &str, field: &str) {
        match expand(value, self.vars) {
            Ok(expanded) => *value = expanded,
            Err(err) => self.errors.push(to_config_error(err, scope, field)),
        }
    }

    fn maybe(&mut self, value: Option<&mut String>, scope: &str, field: &str) {
        if let Some(value) = value {
            self.one(value, scope, field);
        }
    }

    fn list(&mut self, values: &mut [String], scope: &str, field: &str) {
        for (index, value) in values.iter_mut().enumerate() {
            self.one(value, scope, &format!("{field}[{index}]"));
        }
    }

    fn map_values(&mut self, map: &mut BTreeMap<String, String>, scope: &str, field: &str) {
        // Keys are not expanded: an environment variable whose *name* comes
        // from another variable is a puzzle, not a feature.
        for (key, value) in map.iter_mut() {
            self.one(value, scope, &format!("{field}.{key}"));
        }
    }
}

fn to_config_error(err: SubstError, scope: &str, field: &str) -> ConfigError {
    match err {
        SubstError::Undefined(variable) => ConfigError::UndefinedVariable {
            scope: scope.to_string(),
            field: field.to_string(),
            variable,
        },
        SubstError::Malformed(reason) => ConfigError::InvalidSubstitution {
            scope: scope.to_string(),
            field: field.to_string(),
            reason,
        },
    }
}

/// Expand every reference in one value.
fn expand(input: &str, vars: &Vars) -> Result<String, SubstError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(at) = rest.find("${") {
        let (before, opener) = rest.split_at(at);

        // A `$` of its own right before the brace escapes it, which is the only
        // way a value can keep a literal `${`.
        if let Some(before) = before.strip_suffix('$') {
            out.push_str(before);
            out.push_str("${");
            rest = &opener["${".len()..];
            continue;
        }

        out.push_str(before);
        let (body, tail) = split_braced(&opener["${".len()..])?;
        out.push_str(&resolve(body, vars)?);
        rest = tail;
    }

    out.push_str(rest);
    Ok(out)
}

/// Split `${...}` at its closing brace, returning the body and what follows.
///
/// Braces nest, so that the default of one reference can be another.
fn split_braced(input: &str) -> Result<(&str, &str), SubstError> {
    let mut depth = 1usize;
    let mut chars = input.char_indices().peekable();

    while let Some((index, c)) = chars.next() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&input[..index], &input[index + 1..]));
                }
            }
            // `$$` is an escape, so the brace it may hide does not count.
            '$' if matches!(chars.peek(), Some((_, '$'))) => {
                chars.next();
            }
            _ => {}
        }
    }

    Err(SubstError::Malformed(format!(
        "${{{input} is missing its closing brace"
    )))
}

/// Resolve the body of a `${...}` reference.
fn resolve(body: &str, vars: &Vars) -> Result<String, SubstError> {
    let name_len = name_length(body);
    let (name, modifier) = body.split_at(name_len);

    if name.is_empty() {
        return Err(SubstError::Malformed(format!("${{{body}}} has no name")));
    }

    // `${VAR:-default}` falls back when the variable is unset *or* empty,
    // `${VAR-default}` only when it is unset — as in the shell.
    let (default, empty_counts) = match modifier {
        "" => return lookup(name, vars),
        _ => match modifier.strip_prefix(":-") {
            Some(default) => (default, true),
            None => match modifier.strip_prefix('-') {
                Some(default) => (default, false),
                None => {
                    return Err(SubstError::Malformed(format!(
                        "${{{body}}} is not a supported form; expected ${{{name}}}, \
                         ${{{name}:-default}} or ${{{name}-default}}"
                    )))
                }
            },
        },
    };

    match vars.get(name) {
        Some(value) if !value.is_empty() || !empty_counts => Ok(value.clone()),
        _ => expand(default, vars),
    }
}

/// Look up a variable that has to be there.
fn lookup(name: &str, vars: &Vars) -> Result<String, SubstError> {
    vars.get(name)
        .cloned()
        .ok_or_else(|| SubstError::Undefined(name.to_string()))
}

/// How many bytes at the start of `input` form a variable name.
fn name_length(input: &str) -> usize {
    let mut length = 0;
    for (index, c) in input.char_indices() {
        let ok = if index == 0 {
            c.is_ascii_alphabetic() || c == '_'
        } else {
            c.is_ascii_alphanumeric() || c == '_'
        };
        if !ok {
            break;
        }
        length = index + c.len_utf8();
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vars {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Expand, expecting success.
    fn ok(input: &str, pairs: &[(&str, &str)]) -> String {
        match expand(input, &vars(pairs)) {
            Ok(value) => value,
            Err(SubstError::Undefined(name)) => panic!("unexpectedly undefined: {name}"),
            Err(SubstError::Malformed(reason)) => panic!("unexpectedly malformed: {reason}"),
        }
    }

    #[test]
    fn a_braced_reference_expands_anywhere_in_a_value() {
        let env = &[("HOME", "/home/me")];
        assert_eq!(ok("${HOME}/bin", env), "/home/me/bin");
        assert_eq!(ok("a${HOME}b${HOME}", env), "a/home/meb/home/me");
    }

    #[test]
    fn an_unset_variable_is_an_error() {
        assert!(matches!(
            expand("${NOPE}", &vars(&[])),
            Err(SubstError::Undefined(name)) if name == "NOPE"
        ));
    }

    #[test]
    fn a_set_but_empty_variable_expands_to_nothing() {
        assert_eq!(ok("[${EMPTY}]", &[("EMPTY", "")]), "[]");
    }

    #[test]
    fn defaults_differ_on_whether_empty_counts() {
        // `:-` treats empty as absent, `-` does not.
        assert_eq!(ok("${X:-fallback}", &[("X", "")]), "fallback");
        assert_eq!(ok("${X-fallback}", &[("X", "")]), "");
        assert_eq!(ok("${X:-fallback}", &[]), "fallback");
        assert_eq!(ok("${X-fallback}", &[]), "fallback");
        assert_eq!(ok("${X:-fallback}", &[("X", "set")]), "set");
    }

    #[test]
    fn a_default_may_be_another_reference() {
        assert_eq!(ok("${A:-${B}}", &[("B", "from-b")]), "from-b");
        assert_eq!(ok("${A:-${B:-last}}", &[]), "last");
    }

    #[test]
    fn a_dollar_before_the_brace_escapes_the_reference() {
        let env = &[("HOME", "/home/me")];
        assert_eq!(ok("echo $${HOME}", env), "echo ${HOME}");
        // Inside a default too, where the braces have to stay balanced.
        assert_eq!(ok("${A:-$${HOME}}", env), "${HOME}");
    }

    #[test]
    fn a_dollar_that_does_not_open_a_brace_is_left_alone() {
        // Everything a shell snippet does with a dollar survives, which is the
        // reason the braces are mandatory.
        assert_eq!(ok("echo $HOME", &[("HOME", "/home/me")]), "echo $HOME");
        assert_eq!(
            ok("i=0; while true; do echo $i; i=$((i+1)); done", &[]),
            "i=0; while true; do echo $i; i=$((i+1)); done"
        );
        assert_eq!(ok("echo $$ $1 $@", &[]), "echo $$ $1 $@");
        assert_eq!(ok("50$", &[]), "50$");
    }

    #[test]
    fn an_unclosed_or_nameless_reference_is_malformed() {
        for input in ["${HOME", "${}", "${:-x}"] {
            assert!(
                matches!(
                    expand(input, &vars(&[("HOME", "/h")])),
                    Err(SubstError::Malformed(_))
                ),
                "{input} should be rejected"
            );
        }
    }

    #[test]
    fn an_unsupported_modifier_names_what_is_supported() {
        let err = expand("${X:?required}", &vars(&[]));
        let Err(SubstError::Malformed(reason)) = err else {
            panic!("expected a malformed error");
        };
        assert!(reason.contains("${X:-default}"), "{reason}");
    }

    /// A config that puts `${V}` in every field substitution reaches, so that a
    /// field added to [`crate::raw`] without a line in [`expand_raw`] shows up
    /// here rather than in someone's stack.
    const EVERY_FIELD: &str = r#"
version = 1

[project]
name = "demo"
env_file = ["${V}.env", "b.env"]

[project.env]
FROM_PROJECT = "${V}"

[project.logs]
dir = "${V}/logs"
max_size = "${V}"

[services.api]
command = ["${V}", "--flag=${V}"]
cwd = "${V}"
env_file = "${V}.env"
depends_on = ["db"]
restart_delay = "${V}"
restart_max_delay = "${V}"
stable_after = "${V}"
shutdown_signal = "${V}"
shutdown_timeout = "${V}"

[services.api.env]
FROM_SERVICE = "${V}"

[services.api.health]
command = ["${V}"]
interval = "${V}"
timeout = "${V}"
start_period = "${V}"
on_unhealthy = "${V}"

[services.api.watch]
paths = ["${V}"]
ignore = ["${V}"]
interval = "${V}"
debounce = "${V}"

[services.db]
command = ["db"]

[services.db.health]
http = "${V}"
tcp = "${V}"
"#;

    #[test]
    fn every_substitutable_field_is_expanded() {
        let mut raw: RawConfig = toml::from_str(EVERY_FIELD).expect("valid TOML");
        let errors = expand_raw(&mut raw, &vars(&[("V", "yes")]));
        assert!(errors.is_empty(), "{:?}", errors[0].to_string());

        let project = &raw.project;
        assert_eq!(project.env["FROM_PROJECT"], "yes");
        assert_eq!(project.env_file.as_ref().unwrap().paths()[0], "yes.env");
        let logs = project.logs.as_ref().unwrap();
        assert_eq!(logs.dir.as_deref(), Some("yes/logs"));
        assert_eq!(logs.max_size.as_deref(), Some("yes"));

        let api = &raw.services["api"];
        assert_eq!(api.command, vec!["yes", "--flag=yes"]);
        assert_eq!(api.cwd.as_deref(), Some("yes"));
        assert_eq!(api.env["FROM_SERVICE"], "yes");
        assert_eq!(api.env_file.as_ref().unwrap().paths(), ["yes.env"]);
        assert_eq!(api.restart_delay.as_deref(), Some("yes"));
        assert_eq!(api.restart_max_delay.as_deref(), Some("yes"));
        assert_eq!(api.stable_after.as_deref(), Some("yes"));
        assert_eq!(api.shutdown_signal.as_deref(), Some("yes"));
        assert_eq!(api.shutdown_timeout.as_deref(), Some("yes"));

        let health = api.health.as_ref().unwrap();
        assert_eq!(
            health.command.as_deref(),
            Some(["yes".to_string()].as_slice())
        );
        assert_eq!(health.interval.as_deref(), Some("yes"));
        assert_eq!(health.timeout.as_deref(), Some("yes"));
        assert_eq!(health.start_period.as_deref(), Some("yes"));
        assert_eq!(health.on_unhealthy.as_deref(), Some("yes"));

        let watch = api.watch.as_ref().unwrap();
        assert_eq!(watch.paths, ["yes"]);
        assert_eq!(watch.ignore, ["yes"]);
        assert_eq!(watch.interval.as_deref(), Some("yes"));
        assert_eq!(watch.debounce.as_deref(), Some("yes"));

        let db_health = raw.services["db"].health.as_ref().unwrap();
        assert_eq!(db_health.http.as_deref(), Some("yes"));
        assert_eq!(db_health.tcp.as_deref(), Some("yes"));
    }

    #[test]
    fn the_shape_of_the_stack_is_not_substituted() {
        let toml = r#"
version = 1
[project]
name = "${V}"
[services.api]
command = ["api"]
depends_on = ["${V}"]
[services.web]
command = ["web"]
profiles = ["${V}"]
[services.web.depends_on]
db = { condition = "${V}" }
"#;
        let mut raw: RawConfig = toml::from_str(toml).expect("valid TOML");
        let errors = expand_raw(&mut raw, &vars(&[("V", "yes")]));

        // Not expanded, and — just as importantly — not reported as an error
        // either, so the message the user gets is about the name or the
        // condition being invalid.
        assert!(errors.is_empty(), "{:?}", errors[0].to_string());
        assert_eq!(raw.project.name, "${V}");
        let entries = raw.services["api"].depends_on.as_ref().unwrap().entries();
        assert_eq!(entries[0].0, "${V}");
        let web = &raw.services["web"];
        assert_eq!(
            web.depends_on.as_ref().unwrap().entries()[0].1,
            Some("${V}")
        );
        assert_eq!(web.profiles, ["${V}"]);
    }

    #[test]
    fn every_failure_is_reported_at_once_with_its_location() {
        let toml = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["${MISSING_A}", "ok"]
cwd = "${MISSING_B}"
[services.api.env]
PORT = "${MISSING_C}"
"#;
        let mut raw: RawConfig = toml::from_str(toml).expect("valid TOML");
        let messages: Vec<String> = expand_raw(&mut raw, &vars(&[]))
            .iter()
            .map(ToString::to_string)
            .collect();

        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(
            messages[0].contains(r#"service "api": command[0]"#),
            "{messages:?}"
        );
        assert!(messages[0].contains("MISSING_A"), "{messages:?}");
        assert!(messages[1].contains("cwd"), "{messages:?}");
        assert!(messages[2].contains("env.PORT"), "{messages:?}");
    }

    #[test]
    fn expansion_does_not_recurse_into_a_resolved_value() {
        // A variable whose value looks like a reference stays as it is: config
        // is data, not a macro language.
        assert_eq!(ok("${A}", &[("A", "${B}"), ("B", "no")]), "${B}");
    }
}
