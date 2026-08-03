# Servicrab

A **lightweight process supervisor** for local development stacks, homelabs, and
small servers — written in Rust. Supervision is **Linux and macOS only**; the
Windows build compiles but every runtime entry point reports
`UnsupportedPlatform`.

Think of it as a smaller alternative to
[overmind](https://github.com/DarthSim/overmind) or
[Honcho](https://github.com/nicksylett/honcho): one binary, a declarative config
file, a background daemon with a documented socket protocol, and `--json` on
everything a script would want to read.

[![CI](https://github.com/gaborini/servicrab/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/gaborini/servicrab/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/servicrab.svg)](https://crates.io/crates/servicrab)
[![MSRV](https://img.shields.io/badge/rustc-1.85%2B-blue.svg)](https://github.com/gaborini/servicrab#from-source)

---

## Features

- Declare your entire local stack in one `servicrab.toml`, or split it across
  files with `include`
- Profiles: group the optional services and ask for them with `--profile`
- `${VAR}` substitution in the string-valued config fields, strict about unset
  variables
- `servicrab init` — scaffold a config in seconds
- `servicrab check` — validate your config before running anything
- `servicrab list` — see all services and their restart policies at a glance
- `servicrab run <service>` — supervise a single service in the foreground with live stdout/stderr, restart policy, and process-group shutdown
- `servicrab exec <service> -- <cmd>` — run anything in a service's environment and working directory, whether or not the service is up
- `servicrab up` — run your whole stack in the foreground: dependency-ordered start, interleaved and colour-prefixed output, reverse-order shutdown
- Health checks: `command`, `http` and `tcp` probes with readiness gating and automatic restart of unhealthy services
- `servicrab watch` — restart a service as soon as its sources change, with ignore rules and debouncing
- Log files: opt-in per-service capture with size-based rotation, plus `servicrab logs [-f]` to read and follow them
- Background daemon: `servicrab start` / `status` / `stop` / `restart` / `down`, with a documented JSON-over-Unix-socket protocol
- `servicrab start --wait` — block until every service is ready (health checks included), with an exit code that says whether it worked
- `servicrab reload` — apply config changes to a running stack without stopping the services you did not touch
- `servicrab events` — follow a running stack live: logs, state changes, restarts and health verdicts, as text or JSON
- `servicrab generate systemd|launchd` — hand the stack over to the init system, with `systemctl reload` wired to `servicrab reload`
- Restart policies: `never`, `on-failure`, `always`, `unless-stopped`, with
  exponential backoff
- Environment: per-service variables, working directories, and dotenv-style `env_file` layering
- Shell completions for bash, zsh, fish, PowerShell and elvish, and man pages via `servicrab man`
- Dependency declarations and a deterministic start order

---

## Installation

### Prebuilt binaries

Every release ships a tarball per platform (`x86_64` and `aarch64` for both
Linux and macOS) on the
[releases page](https://github.com/gaborini/servicrab/releases), each with a
`.sha256` next to it:

```sh
tag=$(curl -fsSL https://api.github.com/repos/gaborini/servicrab/releases/latest | grep -m1 '"tag_name"' | cut -d'"' -f4)
arch=$(uname -m); [ "$arch" = arm64 ] && arch=aarch64
os=unknown-linux-gnu; [ "$(uname -s)" = Darwin ] && os=apple-darwin
dir="servicrab-${tag#v}-${arch}-${os}"

curl -fsSL "https://github.com/gaborini/servicrab/releases/download/${tag}/${dir}.tar.gz" | tar -xz
sudo install "${dir}/servicrab" /usr/local/bin/
servicrab --version
```

The tarball also contains the man pages in `man/`, one per command:

```sh
sudo install -m 644 "${dir}"/man/*.1 /usr/local/share/man/man1/
man servicrab
```

### From crates.io

```sh
cargo install servicrab
```

### From source

Building from source needs Rust **1.85** or newer (the minimum supported
version, enforced by CI):

```sh
git clone https://github.com/gaborini/servicrab
cd servicrab
cargo install --path crates/servicrab-cli
```

### Development build

```sh
cargo build                        # debug build
cargo build --release              # optimised build
./target/debug/servicrab --help
```

---

## Quick start

```sh
# 1. Scaffold a config
servicrab init

# 2. Edit servicrab.toml to match your stack
#    (see the example below)

# 3. Validate
servicrab check

# 4. List services
servicrab list

# 5. Run a service in the foreground
servicrab run api

# 6. …or bring the whole stack up
servicrab up
```

---

## Example `servicrab.toml`

```toml
version = 1

[project]
name = "my-dev-stack"

[project.env]
RUST_LOG = "info"

[services.db]
command = ["postgres", "-D", "/usr/local/var/postgres"]
restart = "always"

[services.api]
command = ["cargo", "run", "--bin", "api"]
cwd = "./api"
depends_on = ["db"]
restart = "on-failure"
restart_delay = "1s"
restart_max_delay = "30s"
max_restarts = 5
stable_after = "60s"
shutdown_signal = "term"
shutdown_timeout = "10s"

[services.api.env]
PORT = "3000"

[services.worker]
command = ["python", "worker.py"]
cwd = "./worker"
restart = "never"

[services.worker.env]
DATABASE_URL = "postgres://localhost/mydb"
QUEUE_URL    = "redis://localhost"
```

`command` is always a list: the first element is the executable, the rest are
arguments passed verbatim. Servicrab never runs your command through a shell,
so quoting, globbing, and `&&` are not interpreted — configure `sh` explicitly
if you want shell semantics.

---

## Configuration reference

Every field, its type, its default and its accepted range. The ranges are real:
each one is checked at load time, and `servicrab check` reports a violation with
the field name and the limit it broke.

Durations use the [humantime](https://docs.rs/humantime) syntax — `500ms`, `10s`,
`2m`, `1h30m`. Sizes accept `B`, `KB`/`KiB`, `MB`/`MiB`, `GB`/`GiB` and
`TB`/`TiB`, all as powers of 1024.

### Top level

| Field | Type | Default | Range / notes |
|---|---|---|---|
| `version` | integer | — | Required, and must be `1`. A later number is reported as an unsupported schema version rather than as a typo in one of its keys. |
| `include` | string or list of strings | none | Paths to files holding further `[services.<name>]` tables. Relative to the file that declares them. Not `${VAR}`-substituted. |

### `[project]`

| Field | Type | Default | Range / notes |
|---|---|---|---|
| `name` | string | — | Required. 1–64 bytes, ASCII only, must start alphanumeric, then alphanumerics `.` `_` `-`. Not `${VAR}`-substituted: it decides where the daemon keeps its socket. |
| `env` | table of string → string | empty | Keys must be non-empty and contain no `=` or NUL. Values are substituted. |
| `env_file` | string or list of strings | none | Dotenv files loaded for every service, in declaration order, before `[project.env]`. Relative to the config file. |

### `[project.logs]`

Absent by default, and its absence means no file capture at all.

| Field | Type | Default | Range / notes |
|---|---|---|---|
| `dir` | string | `.servicrab/logs` | Relative paths resolve against the config file. An existing non-directory is a config error. |
| `max_size` | size string | `10MB` | **1 KiB … 1 TiB** (`1024` … `1099511627776` bytes). |
| `max_files` | integer | `3` | **0 … 100.** `0` truncates instead of keeping history. |

### `[services.<name>]`

The name follows the project-name rules with a **48-byte** limit rather than 64.

| Field | Type | Default | Range / notes |
|---|---|---|---|
| `command` | list of strings | — | Required and non-empty. First element is the executable; no element may contain a NUL. Never run through a shell. |
| `cwd` | string | the declaring file's directory | Must exist and be a directory. Relative paths resolve against the file that declared the service. |
| `env` | table of string → string | empty | As `[project.env]`, and wins over it. |
| `env_file` | string or list of strings | none | Loaded after the project's, before `[services.<name>.env]`. |
| `depends_on` | list of names, or table of name → `{ condition }` | none | A service uses one form or the other, not both. Neither the names nor the conditions are substituted. |
| `profiles` | list of strings | empty | Each follows the service-name rules (48 bytes). Duplicates are an error. Not substituted. |
| `autostart` | boolean | `true` | Not a string, so not `${VAR}`-substitutable. |
| `restart` | `never` \| `on-failure` \| `always` \| `unless-stopped` | `never` | An enum, so not substitutable. |
| `restart_delay` | duration | `1s` | **100ms … 1h.** |
| `restart_max_delay` | duration | `30s` | **100ms … 24h**, and must be ≥ `restart_delay`. |
| `max_restarts` | integer | `10` | Any `u32`. `0` means unlimited, so there is no range to check. |
| `stable_after` | duration | `60s` | **1s … 24h.** |
| `shutdown_signal` | `term` \| `int` \| `quit` \| `hup` | `term` | A string field, so substitutable — but only these four values are accepted. |
| `shutdown_timeout` | duration | `10s` | **100ms … 1h.** |

`restart_delay`, `restart_max_delay`, `max_restarts` and `stable_after` are
warned about — not rejected — when `restart = "never"`, because there they have
nothing to do.

### `[services.<name>.health]`

Absent by default. Exactly one of `command`, `http` or `tcp` must be set.

| Field | Type | Default | Range / notes |
|---|---|---|---|
| `command` | list of strings | — | Healthy when it exits `0`. Run with the service's environment and `cwd`. |
| `http` | string | — | An `http://host[:port][/path]` URL; healthy on any 2xx or 3xx. No TLS, no redirects, no credentials. Port defaults to `80`, path to `/`. |
| `tcp` | string | — | `host:port`, or `[::1]:port` for an IPv6 literal. Port must not be `0`. |
| `interval` | duration | `2s` | **100ms … 1h.** |
| `timeout` | duration | `5s` | **100ms … 1h.** |
| `retries` | integer | `3` | **≥ 1.** `0` is rejected: a service cannot be declared unhealthy without one failed probe. |
| `start_period` | duration | `0s` | **0s … 24h.** Failures inside it never count. |
| `on_unhealthy` | `restart` \| `ignore` | `restart` | With `restart`, the service is stopped and its **restart policy** decides what happens next. |

### `[services.<name>.watch]`

Absent by default.

| Field | Type | Default | Range / notes |
|---|---|---|---|
| `paths` | list of strings | — | Required and non-empty; every entry must exist. Relative to the service's `cwd`. |
| `ignore` | list of strings | empty | Names, `dir/prefix` or `*.ext`. `.git` and `.servicrab` are always added. |
| `interval` | duration | `1s` | **100ms … 1h.** |
| `debounce` | duration | `300ms` | **50ms … 1h.** |

### `[services.<name>.logs]`

| Field | Type | Default | Range / notes |
|---|---|---|---|
| `enabled` | boolean | `true` | `false` keeps this one service out of the log files. Inert, with a warning, when there is no `[project.logs]`. |

### What `${VAR}` reaches

Substitution applies to **string-valued fields only**, which follows from where
it happens: values are expanded before the TOML is turned into typed data, so a
field that is not a string has no string for a `${VAR}` to live in.

```toml
shutdown_timeout = "${TIMEOUT:-10s}"   # a string field — expanded
max_restarts     = "${MAX}"            # rejected: expected u32, got a string
```

The fields that cannot take a variable are therefore `version`, `autostart`,
`restart`, `max_restarts`, `logs.max_files`, `logs.enabled` and `health.retries`
(all non-strings), plus `include`, the project `name`, the service names, the
`profiles` and the `depends_on` names and conditions — those last are strings,
but are deliberately left literal so that the shape of a stack cannot depend on
who started it.

An unknown field anywhere is a hard error, not a warning: a misspelled `comand`
that loaded quietly would be far worse than one that refuses.

---

## Command reference

`--config` may be spelled `-c`, and every command accepts the two global colour
flags (`--color=auto|always|never`, `--no-color`); neither is repeated below.

| Command | Description |
|---|---|
| `servicrab init [--path PATH] [--force]` | Generate an example `servicrab.toml` |
| `servicrab check [--config PATH] [--json]` | Parse and validate the config file |
| `servicrab list [--config PATH] [--json]` | List all services with their restart policies |
| `servicrab run <SERVICE> [--config PATH] [--no-restart]` | Supervise one service in the foreground |
| `servicrab exec <SERVICE> [--config PATH] -- <COMMAND>...` | Run a command in a service's environment and working directory |
| `servicrab up [SERVICE...] [--config PATH] [--profile NAME]... [--no-restart] [--no-prefix] [--timestamps] [--abort-on-failure] [--json]` | Supervise a whole stack in the foreground |
| `servicrab watch [SERVICE...] [--config PATH] [--profile NAME]... [--no-restart] [--no-prefix] [--timestamps] [--abort-on-failure] [--json]` | Like `up`, and restart services when their watched files change |
| `servicrab logs [SERVICE...] [--config PATH] [-f] [-n N] [--no-prefix]` | Show (and follow) the captured log files |
| `servicrab start [SERVICE...] [--config PATH] [--profile NAME]... [--no-restart] [--wait] [--timeout DUR]` | Start the stack in the background, optionally waiting until it is ready |
| `servicrab status [--config PATH] [--json]` | Show what the background daemon is doing |
| `servicrab stop <SERVICE...> [--config PATH]` | Stop individual services in the running daemon |
| `servicrab restart <SERVICE...> [--config PATH]` | Restart individual services |
| `servicrab reload [--config PATH]` | Re-read the config and apply the difference to the running daemon |
| `servicrab events [SERVICE...] [--config PATH] [--json] [--no-prefix] [--timestamps] [--no-logs]` | Follow the daemon's live event stream |
| `servicrab down [--config PATH]` | Stop the daemon and every service it supervises |
| `servicrab daemon [--config PATH] [--profile NAME]... [--no-restart]` | Run the daemon in the foreground (for systemd/launchd/containers) |
| `servicrab generate <systemd\|launchd> [--config PATH] [--scope system\|user] [-o PATH] [--user NAME] [--profile NAME]...` | Generate an init-system unit that runs the stack |
| `servicrab completions <SHELL>` | Print a completion script for bash, zsh, fish, PowerShell or elvish |
| `servicrab man [-o DIR]` | Print the man page in roff, or write one page per command into `DIR` |

`--profile` is repeatable, and the five commands that take it are `up`, `watch`,
`start`, `daemon` and `generate` — the ones that decide which services make up a
stack. On `up`, `watch` and `start` it cannot be combined with naming services,
because that would be two answers to one question; `daemon` and `generate` take
no service names, so there is nothing to conflict with.

If `--config` is omitted, Servicrab discovers `servicrab.toml` by walking up
from the current directory.

---

## Running a service in the foreground

```sh
servicrab run api                    # supervise `api` until it stops
servicrab run api --config ./stack.toml
servicrab run api --no-restart       # ignore the configured restart policy
RUST_LOG=info servicrab run api      # see lifecycle transitions
```

### Try it right now

```toml
version = 1

[project]
name = "demo"

[services.ticker]
command = ["sh", "-c", "i=0; while true; do echo tick $i; i=$((i+1)); sleep 1; done"]
restart = "on-failure"
shutdown_timeout = "5s"
```

```sh
servicrab run ticker     # Ctrl+C to stop
```

### Foreground semantics

`servicrab run` is a **single-service, no-daemon** mode: it stays in the
foreground, supervises exactly one service, and exits when that service is
finished. Nothing is written to disk and no background process is left behind.

- The executable is spawned directly — **never** through an implicit `sh -c`.
- The working directory is the validated, absolute `cwd`.
- The environment is the merged result of the process, project, and service
  environments, in that order.
- `stdout` and `stderr` are inherited, so output appears live and unbuffered.
- `stdin` is closed; interactive services are not supported in this mode.

### Restart behaviour

| Policy | Clean exit (`0`) | Non-zero exit | Killed by signal |
|---|---|---|---|
| `never` (default) | stop | stop | stop |
| `on-failure` | stop | restart | restart |
| `always` | restart | restart | restart |
| `unless-stopped` | restart | restart | restart |

`unless-stopped` restarts exactly like `always`; the two differ only in what a
daemon does with a service you stopped by hand — see
[Services you stopped by hand](#services-you-stopped-by-hand). In this
single-service foreground mode there is nothing to remember, so the two are the
same thing.

Between attempts the runner waits:

```text
delay = min(restart_delay * 2^attempt, restart_max_delay)
```

If the service stays up for at least `stable_after`, the attempt counter resets
and the next restart starts from `restart_delay` again. Once `max_restarts`
consecutive attempts are used up, the run fails with a non-zero exit code and a
clear message. An explicit shutdown (Ctrl+C or `SIGTERM`) **never** triggers a
restart, whatever the policy says.

### Shutdown behaviour

On `SIGINT` (Ctrl+C) or `SIGTERM` the runner:

1. sends the configured `shutdown_signal` (default `term`) to the service's
   **process group**;
2. waits up to `shutdown_timeout` (default `10s`);
3. escalates to `SIGKILL` for the whole group if anything is still alive;
4. reaps the child so no zombie is left behind.

Pressing Ctrl+C a second time skips straight to `SIGKILL`.

### Process-group guarantees

Every supervised service is spawned into its own process group. That matters
because services usually spawn descendants:

```text
servicrab
  └── npm            ← direct child, process-group leader
      └── node
          └── esbuild
```

Signalling only `npm` would leave `node` and `esbuild` running. Servicrab
signals the entire group instead, and sweeps it with `SIGKILL` after the child
exits, so no descendant outlives the run.

### Exit codes

| Situation | Exit code |
|---|---|
| Service exited normally | the service's own exit code |
| Service killed by signal *N* | `128 + N` |
| Stopped with Ctrl+C | `130` |
| Supervisor received `SIGTERM` | `143` |
| Restart limit exhausted, or a runtime error | `1` |

### Platform support and limitations

`servicrab run` supports **Linux and macOS**. Windows is out of scope for now;
the command reports an unsupported-platform error there.

---

## Running a command in a service's environment

```sh
servicrab exec api -- printenv DATABASE_URL   # what would the service see?
servicrab exec api -- npm run migrate         # a one-off, with the right env
servicrab exec db -- psql                     # interactive, with $PGPASSWORD
```

`exec` reproduces the environment a service *would* get — its merged `env`, its
`env_file` layers, and its working directory — and runs something else in it.
That is the same layering the supervisor applies, computed by the same code, so
what you see is what the service gets.

The command inherits servicrab's stdin, stdout and stderr, so interactive tools
work, pipes work, and nothing of servicrab's own ends up in the output. Its exit
status becomes servicrab's:

| Situation | Exit code |
|---|---|
| The command exited | the command's own exit code |
| The command was killed by signal *N* | `128 + N` |
| The command was not found | `127` |
| The command was found but not executable | `126` |
| Unknown service, or a config that does not load | `1` |

Everything after the service name belongs to the command, including its own
flags, so `servicrab exec api -- ls --all` passes `--all` to `ls`. Use `--` to
make that explicit; a value that could be mistaken for one of servicrab's own
options needs it.

Unlike `docker exec`, this does **not** enter a running process: there is no
namespace to join, and no daemon is involved. That cuts both ways. It works on a
stack that is not running, which is what makes it useful for debugging a service
that will not start — but it cannot show you anything the process changed about
its own environment after it started.

---

## Running a whole stack in the foreground

```sh
servicrab up                       # start every service with autostart = true
servicrab up api                   # start `api` and everything it depends on
servicrab up --no-restart          # ignore every configured restart policy
servicrab up --timestamps          # prefix each line with a UTC timestamp
servicrab up --no-prefix           # raw output, no service prefix
servicrab up --abort-on-failure    # tear the stack down when a service fails
```

Output is interleaved and prefixed with the service name, each service getting
its own colour:

```console
$ servicrab up
servicrab up acme-stack → redis, api
redis | ▶ started (pgid 30292)
api   | ▶ started (pgid 30293)
redis | Ready to accept connections
api   | listening on 0.0.0.0:3000
```

A service's own stdout is written to Servicrab's stdout and its stderr to
Servicrab's stderr, so `servicrab up > stack.log` keeps the two apart. Colour is
decided per stream: whichever of stdout and stderr is a terminal gets colour, so
`servicrab up 2> stack.err` leaves the file free of escapes and
`servicrab up | cat` still colours the progress on stderr. `--color=never` (or
`--no-color`) turns colour off, `--color=always` forces it on both streams, and
`CLICOLOR_FORCE` forces it for a redirected stream. `NO_COLOR` and `TERM=dumb`
disable it; `--color` wins over all of them.

### Which services are started

- with no arguments: every service with `autostart = true` that no profile
  holds back (see [Profiles](#profiles));
- with explicit names: those services only;
- with `--profile NAME`: the above, plus the services in that profile;
- in every case each transitive `depends_on` entry is pulled in, even when it
  has `autostart = false` or sits in a profile of its own.

### Start and stop ordering

Services start in the configuration's deterministic topological order, and a
service is only spawned once every service it depends on is **available**. What
"available" means is per dependency, and by default depends on the dependency
itself:

- a dependency without a health check is available as soon as its process is up;
- a dependency **with** a health check is available only after its first
  successful probe;
- a one-shot dependency (a migration, a build step) that has exited counts as
  available too — otherwise a stack containing one could never come up.

Spell the condition out to override that (see
[Dependency conditions](#dependency-conditions)), which is also the only way to
have the exit status of a one-shot checked.

Shutdown happens in reverse: dependents are stopped (and fully reaped) before
the services they depend on, each with its own `shutdown_signal` and
`shutdown_timeout`, escalating to `SIGKILL` per service exactly as `run` does.

If a dependency never comes up — it fails to spawn, or exhausts its restart
budget — its dependents are **skipped** rather than started against a missing
backend, and the run is reported as failed.

### Exit codes for `up`

| Situation | Exit code |
|---|---|
| Every service ended cleanly | `0` |
| Ctrl+C (SIGINT) | `130` |
| SIGTERM | `143` |
| A service failed or was skipped | `1` |

### Platform support

`servicrab up` supports **Linux and macOS**; on Windows it reports an
unsupported-platform error. `up` runs in the foreground and stops with your
shell — use [`servicrab start`](#running-in-the-background) for a detached
stack, and `servicrab down` to stop one.

---

## Health checks

Any service may declare a `[services.<name>.health]` block with exactly one
probe:

```toml
[services.db.health]
tcp = "127.0.0.1:5432"        # healthy when the port accepts a connection
interval = "2s"               # delay between probes          (default 2s)
timeout = "5s"                # per-probe timeout             (default 5s)
retries = 3                   # failures before unhealthy     (default 3)
start_period = "10s"          # failures ignored for this long (default 0s)
on_unhealthy = "restart"      # restart | ignore              (default restart)

[services.api.health]
http = "http://127.0.0.1:3000/healthz"   # healthy on any 2xx/3xx response

[services.worker.health]
command = ["./scripts/queue-ok.sh"]      # healthy when it exits with code 0
```

- **`command`** runs the given executable with the service's environment and
  working directory; exit code `0` means healthy.
- **`http`** speaks plain HTTP/1.1 — no TLS, no redirects. For anything else
  use a `command` probe such as `["curl", "-fsS", "https://…"]`.
- **`tcp`** succeeds as soon as a connection can be established. `[::1]:6379`
  works for IPv6 literals.

### What health checks do

1. **Readiness gating.** Dependents of a health-checked service wait for its
   first successful probe instead of merely for its process to exist. This is
   the difference between "postgres has been spawned" and "postgres accepts
   connections".
2. **Liveness.** Probing continues for as long as the process runs. After
   `retries` consecutive failures the service is declared unhealthy.
   With the default `on_unhealthy = "restart"` the process is stopped with its
   usual `shutdown_signal`/`shutdown_timeout` and the **restart policy**
   decides what happens next — so `restart = "never"` means an unhealthy
   service simply stops, while `on-failure`/`always` bring it back. Set
   `on_unhealthy = "ignore"` to only report the failure.

Failures during `start_period` never count, which gives slow starters time to
come up without burning their retry budget.

---

## Dependency conditions

`depends_on = ["db"]` says *what* to wait for but not *when* the wait is over.
The table form says both:

```toml
[services.api]
command = ["./api"]

[services.api.depends_on]
db = { condition = "service_healthy" }
migrate = { condition = "service_completed_successfully" }
cache = { condition = "service_started" }
```

| Condition | Available once |
|---|---|
| `service_started` | the process is up; the exit status is never consulted |
| `service_healthy` | a health probe has passed |
| `service_completed_successfully` | the service has exited with status `0` |

Both forms may be mixed across services, but not within one service: a service
either lists names or uses the table.

**Leaving the condition out is not the same as `service_started`.** An entry
without a condition waits for a probe when the dependency has a `[health]`
block, and for the process otherwise — so adding a health check to a service
automatically starts gating everything that depends on it.

`service_completed_successfully` is the one that exists for **migrations and
seed steps**: it is the only condition that looks at the exit status, so it is
the only one that stops a dependent from starting against a half-migrated
database. Under the other two conditions a one-shot that has exited counts as
available whatever its exit status, because a condition a finished process can
never meet again would deadlock the stack.

Conditions that can never be met are rejected at load time rather than at
2 a.m.: `service_healthy` on a service with no `[health]` block, and
`service_completed_successfully` on a service with `restart = "always"`, which
never stays exited.

A dependency that can no longer meet its condition — including a one-shot that
exited non-zero where success was required — leaves its dependents **skipped**,
exactly as an unavailable dependency does.

---

## Log files

Add a `[project.logs]` table and servicrab keeps a copy of everything its
services write:

```toml
[project.logs]
dir = ".servicrab/logs"   # relative paths resolve next to servicrab.toml
max_size = "10MB"         # rotate once a file grows past this (default 10MB)
max_files = 3             # how many rotated generations to keep (default 3)

[services.noisy.logs]
enabled = false           # this one service is not written to disk
```

Every service gets its own `<dir>/<service>.log`. When a file crosses
`max_size` it is rotated to `<service>.log.1`, the previous `.1` becomes `.2`,
and so on up to `max_files`; anything older is deleted. `max_files = 0` simply
truncates the file instead of keeping history.

Both `servicrab up` and `servicrab run` write these files, and neither changes
what you see in the terminal — the files are a copy, not a redirect.

Read them back with `logs`:

```console
$ servicrab logs
db     | starting postgres
db     | db up
worker | worker ready
worker | picked up job 41
$ servicrab logs -n 1
db     | db up
worker | picked up job 41
```

A single named service is printed without the prefix, since there is nothing to
tell apart:

```console
$ servicrab logs db
starting postgres
db up
```

```bash
servicrab logs                 # last 50 lines of every service, prefixed
servicrab logs api -n 200      # last 200 lines of one service, unprefixed
servicrab logs -f              # follow new output (Ctrl+C to stop)
```

`logs -f` notices rotation and keeps following the fresh file, prints each line
once and whole — a file that ends mid-line has the fragment held back until its
newline arrives — and tolerates output that is not UTF-8, replacing the
undecodable bytes rather than failing the command. Without `--follow` there is no
next pass, so a trailing fragment is shown.

Three outcomes are worth knowing, because two of them are failures and one is
not:

| Situation | Exit code |
|---|---|
| The log directory is empty | `0` — a stack that has not run yet is a state of the world, not a failed command |
| The config has no `[project.logs]` table | `1`, with a note saying how to enable capture |
| A **named** service has `[logs] enabled = false` | `1` — there is no file, so the command asked for something that cannot exist |

Sizes accept `B`, `KB`/`KiB`, `MB`/`MiB`, `GB`/`GiB` and `TB`/`TiB` suffixes;
all of them are powers of 1024. The accepted range is
[in the configuration reference](#projectlogs).

---

## Restart on file change

Add a `[watch]` block to a service and it restarts whenever anything under the
watched paths changes:

```toml
[services.api]
command = ["node", "server.js"]
cwd = "./api"

[services.api.watch]
paths = ["src", "package.json"]      # relative to the service's cwd
ignore = ["node_modules", "*.log"]   # names, "dir/prefix" or "*.ext"
interval = "1s"                      # how often the tree is scanned
debounce = "300ms"                   # quiet period before restarting
```

```bash
servicrab watch          # like `up`, but insists that something is watched
servicrab up             # honours [watch] too, without the check
servicrab start          # so does the background daemon
```

```console
$ servicrab watch
servicrab watch demo → api
watching for changes: api
api | ▶ started (pgid 30315)
api | ↻ server.js changed; restarting
api | ◼ stopping: stopped on request
api | ▶ started (pgid 30327)
```

`.git` and `.servicrab` are always ignored. The watcher polls rather than
using inotify or FSEvents: one code path on every platform, no extra
dependency, and a scan is just a comparison of file sizes and modification
times. A `cargo build` that rewrites a hundred files causes **one** restart,
because the watcher waits for `debounce` of quiet before acting.

A watch-triggered restart travels the same control channel as
`servicrab restart`, so it behaves identically — including under the daemon.
Trees larger than 20 000 files are reported once and scanned only up to that
limit; narrow `paths` or add `ignore` entries if you hit it.

---

## Environment files

Anything you would otherwise repeat in `[project.env]` or
`[services.<name>.env]` can live in a dotenv-style file instead:

```toml
[project]
name = "my-project"
env_file = ".env"                    # or a list: [".env", ".env.local"]

[services.api]
command = ["node", "server.js"]
env_file = [".env.api", ".env.api.local"]

[services.api.env]
PORT = "3000"                        # wins over anything in the files
```

Paths are relative to `servicrab.toml`. Files are read once, when the config is
loaded, and layered lowest to highest:

```
inherited shell environment
  → project env_file (in declaration order)
    → [project.env]
      → service env_file (in declaration order)
        → [services.<name>.env]
```

The file format is deliberately small:

```sh
# a comment
KEY=value
export KEY=value          # `export` is accepted and ignored
QUOTED="hello world"      # double quotes support \n \r \t \\ \" escapes
LITERAL='no $expansion'   # single quotes are literal
EMPTY=
PORT=3000                 # trailing comments are stripped
```

The file's own contents are never expanded: what is written is what the service
receives. (Values in `servicrab.toml` are — see
[Variables in the config](#variables-in-the-config).) A missing file, an
unterminated quote or a line without `=` is a configuration error, reported by
`servicrab check` with the file name and line number — the stack never starts
with a half-loaded environment.

---

## Profiles

Most repositories have more than one "everything": the services you always
want, and the extras you only sometimes do. `profiles` puts a service in a
group that has to be asked for by name:

```toml
[services.api]
command = ["node", "server.js"]        # no profiles: always part of the stack

[services.mailhog]
command = ["mailhog"]
profiles = ["dev"]

[services.seeder]
command = ["./seed.sh"]
profiles = ["dev", "test"]             # any one of them is enough
```

```sh
servicrab up                           # api
servicrab up --profile dev             # api, mailhog, seeder
servicrab up --profile test            # api, seeder
servicrab up --profile dev --profile test
servicrab start --profile dev          # same, in the background
servicrab list                         # shows each service's profiles
```

A service that declares no profiles is always started; one that declares any
waits to be asked. Naming a service starts it whatever its profiles say, so
`servicrab up mailhog` needs no flag — and because that is a second way of
saying which services to start, naming services and passing `--profile` in one
command is refused rather than silently resolved.

Two things follow from profiles selecting what to start *on its own*:

- **Dependencies come along regardless.** If an always-on service depends on a
  profiled one, the profiled one is started too — a service can never run
  without what it declares in `depends_on`. Put the dependent in the profile as
  well if it should stay out.
- **The daemon remembers.** `servicrab start --profile dev` records the set for
  the lifetime of the daemon, so `servicrab reload` re-plans that stack rather
  than the smaller one a bare `start` would have produced.
  `servicrab generate systemd --profile dev` writes the flag into the unit for
  the same reason.

A `--profile` no service declares is an error listing the ones that exist,
because a typo that silently started less than you asked for is the kind of
thing you notice an hour later.

---

## Splitting a config across files

A config that describes a dozen services is easier to live with in pieces, so
`include` pulls services in from other files:

```toml
version = 1
include = ["services/db.toml", "services/api.toml"]   # or a single path

[project]
name = "my-project"
```

```toml
# services/db.toml
[services.db]
command = ["postgres", "-D", "data"]
```

An included file holds `[services.<name>]` tables and, if it likes, an
`include` of its own. `version` and `[project]` stay in `servicrab.toml`: it is
the file every command is pointed at, and the project name decides where the
daemon keeps its socket.

**Relative paths in an included file belong to that file.** Both its own
`include` entries and the `cwd` and `env_file` of the services it declares
resolve against its directory, so `services/db.toml` can say `cwd = "."` and
mean `services/`, and a fragment can be moved together with the code it
describes.

Merging is not overriding. Each of these is a configuration error, reported by
`servicrab check` with both file names:

- two files declaring the same service — an `include` that quietly replaced a
  service would be a fine way to spend an afternoon wondering which file is in
  charge;
- an `include` cycle, printed as the chain of files that closes it;
- the same file included from two places;
- `version` or `[project]` in an included file.

`include` paths are not `${VAR}`-substituted, for the same reason the project
name is not: which files make up a config should not depend on who ran it.

---

## Variables in the config

Every **string-valued** field in `servicrab.toml` can refer to the environment of
whoever runs `servicrab`, so one committed config can serve checkouts that
disagree about where things live:

```toml
[services.api]
command = ["${NODE:-node}", "server.js"]
cwd = "${WORKSPACE}/api"

[services.api.env]
DATABASE_URL = "postgres://localhost:${PG_PORT:-5432}/app"
```

| Written | Expands to |
|---|---|
| `${VAR}` | the value; **an error** when `VAR` is not set |
| `${VAR:-default}` | `default` when `VAR` is unset or empty |
| `${VAR-default}` | `default` when `VAR` is unset |
| `$${VAR}` | a literal `${VAR}` |

An unset variable stops the load and names itself:

```console
$ servicrab check
error: /srv/demo/servicrab.toml has 1 error(s)
  • service "api": cwd refers to ${WORKSPACE}, which is not set; use ${WORKSPACE:-default} if it may be absent
```

That is the point of the feature: a `cwd` that quietly became `/`, or a
`command` that quietly lost an argument, is harder to diagnose than a config
that refuses to start.

Four details are worth knowing:

- **The braces are required.** A bare `$` is never special, so the shell
  snippets that fill a process manager's config keep working —
  `command = ["sh", "-c", "echo $HOME; echo $$"]` reaches the shell verbatim.
  This is the one place the syntax narrows Docker Compose's.
- **Only string fields are reached.** Expansion runs on the text of a value
  before TOML turns it into typed data, so a field that is not a string has no
  string for a `${VAR}` to live in: `max_restarts = "${MAX}"` is rejected as a
  string where a `u32` belongs. See
  [What `${VAR}` reaches](#what-var-reaches) for the full list.
- **Values come from the environment only**, not from `[project.env]`,
  `[services.<name>.env]` or an `env_file`. Those describe what the *service*
  will see; substitution happens earlier, while the config is still being read.
- **Names are not substituted.** The project name and the service names are
  literal, and so are table keys: `${...}` in an `[services.<name>.env]` key
  stays as written. The project name in particular decides where the daemon
  keeps its socket, and a control socket that moves with the environment would
  be a debugging trap.

Expansion happens before every other check, and it does not recurse: a value
that expands to `${SOMETHING}` is left at that.

---

## Shell completions

```bash
servicrab completions bash > /etc/bash_completion.d/servicrab
servicrab completions zsh  > ~/.zfunc/_servicrab
servicrab completions fish > ~/.config/fish/completions/servicrab.fish
```

`powershell` and `elvish` are supported too. The script is written to stdout,
so nothing is installed behind your back.

---

## Man pages

Release tarballs ship the pages under `man/`. To generate them from any build:

```bash
servicrab man                                    # the main page, in roff, on stdout
servicrab man -o /usr/local/share/man/man1       # one page per command
man servicrab
man servicrab-up
```

The pages are generated from the same command definitions as `--help`, so they
cannot drift from it; the sections clap cannot know about — files, environment
variables, exit codes — are written by hand.

---

## Running in the background

`up` is the interactive mode; `start` is the same stack without a terminal
attached:

```bash
servicrab start          # supervise the stack in a detached daemon
servicrab status         # a process table: state, pid, uptime, restarts, health
servicrab status --json  # the same, machine-readable
servicrab logs -f        # follow the captured output (needs [project.logs])
servicrab down           # stop every service in reverse order, then exit
```

Individual services can be driven without disturbing the rest of the stack:

```bash
servicrab stop worker       # stop it; the daemon and everything else stay up
servicrab start worker      # start it again
servicrab restart api db    # stop and start, one service at a time
```

A service stopped this way stays stopped — the restart policy does not bring
it back, because the stop was deliberate.

```console
$ servicrab status
SERVICE  STATE          PID    UPTIME  RESTARTS  HEALTH
api      stopped          -         -         0  -
db       running      83810        4s         0  -
worker   failed           -         -         1  -
  api: stopped (stopped on request), last status: exited with code 0
  worker: service "worker": giving up after 1 restart attempt(s)
```

The rows are the table; the indented lines below it are the last noteworthy
thing that happened to a service, and only services that have one get a line.
They are written for a person, so treat them as prose and not as a format to
parse — `status --json` carries the same field as `message`.

The `PID` column holds the service's **process group** id — every service runs
in its own group — so `kill -TERM -<that number>` reaches the service and its
descendants together. `status --json` calls the same number `pgid`, with `pid` as
a deprecated alias.

### Services you stopped by hand

A hand-stopped service stays stopped for as long as that daemon lives, whatever
its restart policy says. What the policy decides is whether the *next* daemon
remembers:

```toml
[services.api]
command = ["node", "server.js"]
restart = "unless-stopped"
```

```bash
servicrab stop api      # you are running api in a debugger instead
servicrab down          # end of the day
servicrab start         # api is still yours; the rest of the stack comes back
servicrab start api     # hand it back to servicrab
```

With `restart = "always"` that last daemon would have started `api` again,
because `always` means always. `unless-stopped` is the same policy plus a
memory, and only for the initial start: once running, the two behave
identically.

The memory is a list of service names in `.servicrab/stopped`, written by the
daemon whenever you stop or start a service. It is plain text on purpose —
deleting it, or a line from it, is a perfectly good way to forget a stop. Two
consequences worth knowing:

- **It only affects `unless-stopped` services.** The file records every stop,
  but every other policy starts as it always has, so adopting this changes
  nothing about an existing stack.
- **Dependents are held back too.** A service cannot run without what it
  declares in `depends_on`, so if a held-back service is a dependency, whatever
  depends on it starts out stopped as well rather than waiting for something
  nobody is going to start. `servicrab start` on the dependency brings it up;
  its dependents need starting too.

`servicrab up` ignores all of this: a foreground run has nothing to remember,
and there is no `stop` command while it is the thing holding your terminal.

### Waiting for the stack to be ready

`start` returns as soon as the daemon is up, which is not the same as the stack
being usable. `--wait` returns when it actually is:

```bash
servicrab start --wait                 # ... 60s by default
servicrab start --wait --timeout 2m    # give it longer
servicrab start api --wait             # wait for one service inside a running daemon
```

A service counts as ready when it is running and — if it declares a health
check — that check has passed. A one-shot service that has already exited counts
as ready too: a migration that finished is not something to keep waiting for.
That is the default a dependent waits for as well, so with the default
conditions `--wait` returns exactly when the last dependent would have been
released. A `depends_on` entry that spells out
[a condition](#dependency-conditions) is not reflected here: `--wait` asks
whether each service is ready, not whether it satisfies a particular dependent,
so it does not check the exit status of a one-shot. A service the daemon
deliberately left stopped is not waited for either — see
[Services you stopped by hand](#services-you-stopped-by-hand).

The exit code is the point:

| Exit code | Meaning |
|---|---|
| `0` | Every service is ready |
| `1` | A service gave up (failed, or stopped unhealthy), or the timeout ran out |

Either way the daemon is left running — a stack that came up wrong is easier to
diagnose alive, with `status`, `logs` and `events`. Which makes this a usable CI
step:

```bash
servicrab start --wait --timeout 90s || { servicrab status; servicrab logs -n 100; exit 1; }
./run-integration-tests
servicrab down
```

The daemon keeps its runtime state next to the config file, in
`.servicrab/`: `daemon.sock` (the control socket), `daemon.pid`, `daemon.log`
(its own diagnostics — service output goes to the log files described above),
and `stopped` (the services you stopped by hand). Add `.servicrab/` to your
`.gitignore`.

**The socket is the one file that can move out of the project directory.** A
Unix socket path has a hard length limit — 107 bytes on Linux, 103 on macOS —
and a deeply nested project can exceed it. When that happens the socket moves to
the first directory that is genuinely private to this user (`$XDG_RUNTIME_DIR`,
then the temp directory), under a name derived from the project directory, and
`servicrab start` prints the path it chose. Plain `/tmp` is refused: a
predictable name in a shared directory is exactly what the mode-`0600` socket is
protecting against. With nowhere private to move to, the long path stays and
`bind` fails, saying which candidates were rejected and why.

Each project gets its own daemon, so several stacks can run side by side, and
`start` refuses to launch a second daemon for the same config.

`servicrab daemon` runs the same thing in the foreground, which is what you
want under systemd, launchd, or in a container: it supervises the stack, serves
the socket, and stops the whole stack cleanly on `SIGTERM`.

### What survives a daemon that is killed, and what does not

On the graceful path — `servicrab down`, `SIGTERM`, `SIGINT` — the daemon stops
every service in reverse dependency order, reaps them, and unlinks its socket
and pidfile. That is the path worth relying on, and it is the only one.

**After `SIGKILL`, an OOM kill or a panic, the daemon's children are not
reconciled.** This is a deliberate 1.0 decision rather than an oversight, and
the consequences are worth stating plainly:

- **The services keep running, reparented to init.** Nothing stops them and
  nothing is supervising them any more. Their restart policies, health checks
  and log capture are all gone with the daemon.
- **The socket and the pidfile are left behind.** They are unlinked by the
  daemon's own shutdown code, which by definition did not run.
- **A later `servicrab start` succeeds and runs a second copy.** The stale
  pidfile is not an obstacle: the `flock` on it is released by the kernel when
  the process dies, so the next daemon takes the lock, removes the stale socket
  and starts the stack again — beside the orphans, not instead of them. Two
  copies of a service that binds a port will not both come up; two copies of a
  worker will both consume from the queue.

`servicrab status` and `servicrab down` both exit `3` in this state, because
there is genuinely no daemon to talk to. Neither one will find the orphans:
`down` has nothing to send a request to.

So the operator's job after a killed daemon is the part servicrab does not do:

```bash
servicrab status                     # exits 3 — confirms the daemon is gone
ps -eo pid,pgid,command | grep <something from your commands>
kill -TERM -<PGID>                   # the leading `-` signals the whole group
servicrab start                      # only once the orphans are gone
```

Every service runs in its own process group, so signalling the group id — the
`pgid` in `status --json` and in the `started` event, which is the number the
group was created with — reaches the service and its descendants together.

This is not hypothetical. While this documentation was being written, a check of
the development machine found **twenty** escaped `servicrab daemon` processes
from three different worktrees, every one of them still supervising services
whose configuration directory had long since been deleted, and the oldest of
them more than **twenty-four hours** old. Orphans do not announce themselves;
they are found by going to look. If you need supervision that survives its own
supervisor being killed, run `servicrab daemon` under something whose job that
is — systemd or launchd, with `Restart=` set — and see
[Running under systemd or launchd](#running-under-systemd-or-launchd). Servicrab
supervises your services; it does not supervise itself.

A quick way to go and look:

```bash
pgrep -fl 'servicrab daemon'
```

Each line names the config the daemon was started with. A config path that no
longer exists is a daemon nothing is going to reach.

### The socket protocol

Clients speak newline-delimited JSON — one request per line, one response per
line — so anything that can write to a Unix socket can drive servicrab:

```bash
echo '{"type":"status"}' | nc -U .servicrab/daemon.sock
```

| Request | Response |
| --- | --- |
| `{"type":"ping"}` | `{"type":"pong","project":"…","pid":123,"version":1}` |
| `{"type":"status"}` | `{"type":"status","services":[…]}` |
| `{"type":"shutdown"}` | `{"type":"ok","message":"…"}` |
| `{"type":"start_service","name":"api"}` | `{"type":"ok","message":"…"}` |
| `{"type":"stop_service","name":"api"}` | `{"type":"ok","message":"…"}` |
| `{"type":"restart_service","name":"api"}` | `{"type":"ok","message":"…"}` |
| `{"type":"reload"}` | `{"type":"ok","message":"…","changes":{"added":1,"changed":1,"removed":0}}` |
| `{"type":"subscribe"}` | `{"type":"ok","schema_version":1}`, then a `{"type":"event",…}` line per event |

`stop_service` and `restart_service` only answer once the service has actually
stopped (and, for a restart, been replaced), so scripts can rely on the reply
instead of polling.

**`message` is for people, not for programs.** It is a sentence written for
whoever is reading the terminal, and its wording may change in any release.
Everything a program should act on is a field of its own: `changes` on a
reload's `ok`, and on an `error`:

```json
{"type":"error","code":"validation_failed","message":"/srv/demo/servicrab.toml has 2 error(s); the stack was left untouched","errors":["service \"api\": command must not be empty","service \"web\": depends on unknown service \"nowhere\""]}
```

| `code` | Meaning |
| --- | --- |
| `unknown_service` | The request named a service this daemon does not supervise |
| `busy` | Another command for that service has not finished yet |
| `not_running` | Nothing is listening: no daemon is running for this project |
| `already_running` | The service, or the daemon, is running already |
| `validation_failed` | The configuration did not load, or did not validate — see `errors` |
| `unsupported` | This daemon does not support that request |
| `failed` | It failed for a reason with no code of its own |

The set can grow, so treat an unfamiliar code as `failed`. A response from a
0.x daemon has no `code` at all, which reads the same way.

`ServiceInfo` entries carry the running process's group id as **`pgid`**.
`pid` is a deprecated alias holding the same number: it always was a
process-group id — every service runs in its own group, whose leader is the
direct child — and signalling it as a pid would reach only that leader. `pgid`
is the name `Event::Started` has always used for it.

#### Reading a stream from a newer servicrab

`version` in the `ping`/`pong` exchange says which revision of this protocol
each side speaks. It is optional in both directions: a client from before it
existed sends nothing, and is answered normally. Nothing refuses to talk on the
strength of the number — it is there so that a version mismatch can be reported
rather than guessed at.

That is possible because both ends decode leniently instead. Anything a build
cannot name — a request type, a response type, an event `kind`, a service
`state`, a `health` verdict, a log `stream` — reads back as `unknown` rather than
failing the line. A client skips what it does not understand and keeps reading,
so a later release can add an event type without ending every older client's
stream, and one unfamiliar state does not cost you the rest of a `status`
snapshot.

Write your own client the same way: ignore a `type` or `kind` you do not
recognise rather than treating it as an error, and do not give a field the value
`unknown` — it is reserved for exactly this.

A request the daemon does not recognise is named back to you, with the set it
does accept, so a typo in a hand-rolled client is one round trip to diagnose.
The `unsupported` code is there for the program; the sentence is for you:

```console
$ echo '{"type":"strat"}' | nc -U .servicrab/daemon.sock
{"type":"error","code":"unsupported","message":"this daemon does not support the request \"strat\"; it supports: ping, status, shutdown, start_service, stop_service, restart_service, reload, subscribe"}
```

### Live event streaming

`subscribe` is the one request that turns a connection one-way: the daemon
answers `ok` and then keeps writing events until the client goes away. It takes
two optional fields — `services` (an allow-list; empty means all of them) and
`logs` (set it to `false` to leave captured output out).

```bash
$ echo '{"type":"subscribe","services":["api"]}' | nc -U .servicrab/daemon.sock
{"type":"ok","schema_version":1}
{"type":"event","service":"api","event":{"kind":"log","stream":"stdout","line":"listening on :8080"}}
{"type":"event","service":"api","event":{"kind":"state","state":"stopping"}}
```

`up --json` and `watch --json` print the very same lines without a daemon in
sight, so a wrapper can consume a foreground stack the same way it consumes a
background one — handshake included:

```console
$ servicrab up --json
{"type":"ok","schema_version":1}
{"type":"event","service":"api","event":{"kind":"state","state":"starting"}}
{"type":"event","service":"api","event":{"kind":"log","stream":"stdout","line":"listening on :8080"}}
```

In `--json` mode stdout carries nothing but event lines — the banner, warnings
and the closing summary stay on stderr — so `servicrab up --json | jq` works
unchanged.

`servicrab events` is the CLI on top of it, rendered like `up`:

```console
$ servicrab events
servicrab events demo → all services
api    | listening on :8080
worker | picked up job 41
api    | ▶ started (pgid 5512)
```

| Flag | Effect |
| --- | --- |
| `SERVICE...` | Only follow these services |
| `--json` | Print the raw protocol lines, one JSON object per line |
| `--no-logs` | Lifecycle events only — no captured stdout/stderr |
| `--no-prefix` | Drop the service-name column |
| `-t`, `--timestamps` | Prefix every line with a UTC timestamp |

The stream is a live feed, not a backlog: a subscriber sees what happens from
the moment it attaches (use `servicrab logs` for history). A client too slow to
keep up gets a `{"type":"lagged","skipped":N}` notice instead of silently
missing events, and the command exits cleanly when the daemon stops.

### Machine-readable output

Every `--json` output carries a `schema_version`. It is bumped only when a shape
changes in a way an existing reader would misread — adding an optional field is
not such a change — so a script can refuse a stream it was not written for
instead of guessing.

There are two shapes, and which one a command uses follows from what it is:

- **A whole document**, pretty-printed, for the commands that answer a question
  and exit: `check --json`, `list --json`, `status --json`. `schema_version` is
  a key at the top level, alongside the payload. `list` keeps its services in an
  array, but under a `services` key rather than as the top-level value — a bare
  array has nowhere to put the version.
- **NDJSON**, one object per line, for the commands that stream:
  `up --json`, `watch --json`, `events --json`. Each opens with the same
  `{"type":"ok","schema_version":1}` handshake the daemon answers `subscribe`
  with, then writes one event per line. The version is not repeated on every
  line: these streams can run for days.

**An absent value is an absent key, not a null.** A service that is not running
has no `pgid`, `pid` or `uptime_secs` in `status --json`, and one with nothing
noteworthy to report has no `message` — the keys are simply not there:

```console
$ servicrab status --json
{
  "schema_version": 1,
  "running": true,
  "services": [
    {
      "name": "worker",
      "state": "failed",
      "restarts": 1,
      "health": "none",
      "message": "service \"worker\": giving up after 1 restart attempt(s)"
    }
  ]
}
```

So read these fields as optional. `jq -r '.services[].pgid'` prints `null` for a
stopped service, which is usually what you want; `.pgid | tonumber` fails, which
is usually not.

Errors are JSON too, on **stderr**, so stdout carries nothing but the document
that was asked for:

```console
$ servicrab check --json
{"schema_version":1,"error":{"code":"validation_failed","message":"/srv/demo/servicrab.toml has 2 error(s)","errors":["service \"api\": command must not be empty","service \"web\": depends on unknown service \"nowhere\""]}}
```

`code` is the same stable set the [socket protocol](#the-socket-protocol) uses.
`message` is for people and may be reworded in any release.

### Exit codes for every command

This is the same set the man page's `EXIT STATUS` section documents, and it is
frozen for 1.x.

| Exit code | Meaning |
|---|---|
| `0` | Success. For `run` and `up` that means the services were shut down as asked, not that they never failed. `down` uses it when a daemon was there and stopped. |
| `1` | The command failed: an invalid configuration, an unknown service, a service that exhausted its restart budget, a per-service command the daemon refused, or a `start --wait` that timed out. |
| `3` | No daemon is running for this project. |
| `126`, `127` | `exec` could not run the command: found but not executable (`126`), or not found (`127`), as a shell would report it. |
| `129`, `130`, `143` | `up` and `watch` were cut short by a signal and shut the stack down cleanly: `SIGHUP` (129), Ctrl+C (130), `SIGTERM` (143). A clean shutdown, not a failure. |
| anything else | `exec` and `run` pass through the status of the process they ran: its own exit code, or `128+N` when a signal *N* killed it. |

A few `1`s are worth spelling out, because "it failed" is not obvious for a
command whose job is to report:

- `check` exits `1` on a config that does not load or does not validate. The
  report is the output; the code is what a CI step reads.
- `logs` exits `1` when the config has no `[project.logs]` table, and when a
  named service has `[logs] enabled = false` — in both cases there is no file to
  read, which is a mistake in the command rather than a state of the stack. An
  **empty** log directory is a state of the stack, so that exits `0`.

#### `3`, and the one breaking change in it

`3` means "there was nothing to talk to", which is a thing scripts routinely
want to handle rather than report. It comes from `status`, `down`, `reload`,
`stop`, `restart`, `start SERVICE` and `events`.

**`down` used to exit `0` when no daemon was running, and now exits `3`.** That
is deliberate, and it is a breaking change for anything chaining off it:

```bash
servicrab down && echo "ok"     # prints nothing on a stopped stack, as of 1.0
```

`down` still never *fails* because nothing was running — running it twice is
safe, which is the whole point of it — the code just distinguishes the two
outcomes, and its note goes to stderr with no `error: ` prefix for the same
reason. If you want the old behaviour, say so:

```bash
servicrab down; case $? in
  0) echo "stopped it" ;;
  3) echo "nothing was running" ;;  # not a failure
  *) echo "something went wrong"; exit 1 ;;
esac
```

### One error format

Errors always go to **stderr**, prefixed with `error: `, with the individual
problems as bullets below:

```console
$ servicrab check
error: /srv/demo/servicrab.toml has 2 error(s)
  • service "api": command must not be empty
  • service "web": depends on unknown service "nowhere"
```

Under `--json` the same error is a JSON object, still on stderr, so a caller
parsing stdout sees only the document it asked for.

There is exactly one exception to the `error: ` prefix, and it is deliberate:

```console
$ servicrab down
no daemon is running for demo
$ echo $?
3
```

`down` asks for a stack to be stopped, and a stack that is already stopped is a
state of the world rather than a failure, so the note is a plain sentence. It
still goes to stderr, and the exit code is still `3`, because a script needs to
be able to tell the two situations apart. `status` moved to stderr in the same
release but kept the prefix and the suggestion:

```console
$ servicrab status
error: no daemon is running for demo — start one with `servicrab start`
```

### Config hot-reload

`servicrab reload` makes the running daemon re-read its `servicrab.toml` and
apply only what changed:

```console
$ servicrab reload
✓ reloaded demo: 1 added, 1 changed, 0 removed
  from /srv/demo/servicrab.toml
```

| Difference | What the daemon does |
| --- | --- |
| A service was added | It is started (and becomes visible to `status`, `stop`, `restart`, …) |
| A service definition changed | It is restarted with the new definition |
| A service was removed | It is stopped and disappears from the stack |
| Nothing changed | Nothing at all — uptime and restart counters are preserved |

A service you stopped by hand stays stopped; it picks up its new definition the
next time you start it. Comparison is exact: any field that affects the process
(command, environment, env files, restart policy, health check, watch rules, …)
counts as a change.

If the new config is invalid the reload is refused and the stack keeps running
untouched, so a typo can never take a stack down:

```console
$ servicrab reload
error: /srv/demo/servicrab.toml has 1 error(s)
  • service "broken": depends on unknown service "ghost"
```

Project-level settings — `[project.logs]`, the log directory and rotation
rules — are bound to the daemon process and need a `servicrab down` +
`servicrab start` to change. File watchers (`[services.x.watch]`) *are*
re-created on every reload.

The wire types live in the `servicrab-protocol` crate, which depends on
neither the runtime nor Tokio.

---

## Running under systemd or launchd

`servicrab generate` writes a unit that starts `servicrab daemon` — the
foreground supervisor — so the init system supervises one process and
servicrab supervises the stack:

```console
$ servicrab generate systemd
[Unit]
Description=servicrab stack "demo"
…
$ servicrab generate systemd -o .            # writes servicrab-demo.service
$ servicrab generate launchd --scope user -o ~/Library/LaunchAgents/
```

Install instructions are printed to stderr, so `servicrab generate systemd >
demo.service` keeps the file clean.

| Flag | What it does |
| --- | --- |
| `--scope system` (default) | `/etc/systemd/system` or `/Library/LaunchDaemons`, started at boot |
| `--scope user` | `systemctl --user` or `~/Library/LaunchAgents`, started at login |
| `--user NAME` | Run the daemon as another account (system scope only) |
| `-o PATH` | Write to a file, or into a directory using the conventional name |

The generated unit contains no service definitions — it points at your
`servicrab.toml`. Adding a service means editing the config and reloading, not
regenerating the unit. On systemd, `ExecReload` is wired to `servicrab reload`,
so `systemctl reload servicrab-demo.service` applies config changes without
touching the services that did not change. `TimeoutStopSec` (and launchd's
`ExitTimeOut`) follow the slowest `shutdown_timeout` in your config, so the init
system never kills the daemon while it is still stopping the stack.

---

## Workspace layout

```
servicrab/
├── crates/
│   ├── servicrab-cli/      # Binary crate — clap CLI + Tokio async runtime
│   ├── servicrab-core/     # Library — config models, validation, lifecycle + process runtime
│   └── servicrab-protocol/ # Library — daemon request/response wire types
└── Cargo.toml              # Workspace manifest
```

There is no `servicrab.toml` in this repository: `servicrab init` writes one
into *your* project, and a personal config here would be committed by accident,
so it is in `.gitignore`.

---

## Development

```sh
# Format
cargo fmt --all -- --check

# Lint (warnings are errors in CI)
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Test
cargo test --workspace --all-features --locked

# Documentation (a broken intra-doc link is an error in CI)
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked

# Packaging, as crates.io will see it
cargo package --workspace --locked

# Run a specific test
cargo test -p servicrab-core config::tests

# Check against the minimum supported Rust version
rustup toolchain install 1.85
cargo +1.85 check --workspace --all-features --all-targets --locked
```

Those are the six checks the `CI` workflow runs on every pull request:
`fmt + clippy + test` on Linux **and** macOS (one job, a two-way matrix), the
MSRV check, the Windows stub build, the crates.io packaging dry run, and the
rustdoc build. `--locked` is on every one of them, because without it cargo may
resolve a newer transitive dependency and CI stops testing what `Cargo.lock`
ships.

The dependency audit is **not** one of them:

```sh
cargo deny check
```

It lives in its own `Audit` workflow, which runs on a schedule, on pushes to
`main`, and on release tags — and on a pull request only when `Cargo.toml`,
`Cargo.lock`, `deny.toml` or the workflow itself changed. Advisories are
published on the RustSec calendar rather than ours, so a new one should not fail
an unrelated documentation change, and it should be found in a week when nobody
opens a pull request at all.

---

## What 1.0 promises

Three surfaces are stable in 1.x. Additions are allowed; breaking any of these
means 2.0.

**1. The CLI surface.** The command and flag inventory, what each one does, and
the exit codes. A 1.x release may add a command, add a flag, or add a value to an
existing flag; it may not remove one, rename one, or change what one means.

**2. The socket protocol.** The request and response types, their field names,
and the `code` set on an error. A 1.x release may add a request type, a response
field, or an event kind — which is safe precisely because every enum on the wire
has an `unknown` fallback, so an older client skips what it cannot name instead
of failing the line. See
[Reading a stream from a newer servicrab](#reading-a-stream-from-a-newer-servicrab).

**3. The `--json` output.** The shapes described under
[Machine-readable output](#machine-readable-output), and the `schema_version`
that identifies them. An optional field may be added without a bump; a change an
existing reader would misread bumps the version.

### What is not stable

- **`servicrab-core` and `servicrab-protocol` carry no semver guarantee.** They
  are published so that `servicrab` can be, and their version numbers move with
  the workspace, but they are internal crates: their Rust API may change in any
  release, including a patch. If you depend on either directly, pin an exact
  version. The stable interface for a third-party program is the socket protocol
  and the `--json` output, both of which are contracts on the wire rather than in
  Rust.
- **The `message` field of a socket response, and the prose of any human-readable
  output.** Both are written for whoever is reading the terminal and may be
  reworded in any release. Everything a program should act on is a field of its
  own, or an exit code.
- **The exact bytes of `--help`.** The inventory and the wording of each flag's
  description are frozen; the *whitespace and column alignment* are not. `clap`
  aligns the description column to the longest flag on a page, so adding a flag
  necessarily shifts every other line on that page — and adding a flag is exactly
  what 1.x is allowed to do. A byte-exact snapshot would forbid what semver
  permits.

  That distinction is not a matter of interpretation:
  `crates/servicrab-cli/tests/help.rs` enforces it. It records every page's flag
  list and every description with runs of whitespace collapsed to one space, so a
  reword or a dropped flag fails the build while a realignment does not. That
  file is the specification of this paragraph — if the two ever disagree, the test
  is right.

### The line to script against

```bash
servicrab status --json | jq -e '.schema_version == 1' >/dev/null || exit 1
```

Read `schema_version`, refuse what you were not written for, and treat an
unfamiliar `type`, `kind` or `code` as something to skip rather than an error.
That is the whole forward-compatibility contract, and it is enough.

---

## Roadmap

### 1.0 — what is in it

Everything documented above:

- **Config**: one `servicrab.toml` or several via `include`, `${VAR}`
  substitution in string fields, profiles, `env_file` layering, dependency
  conditions, health checks, log capture with rotation, file watching.
- **Foreground**: `run` for one service, `up` for a stack, `watch` for a stack
  that restarts on change, `exec` for a one-off in a service's environment.
- **Background**: a daemon per project over a Unix socket, with `start`,
  `status`, `stop`, `restart`, `reload`, `events`, `down` and `daemon`, plus
  `start --wait` for CI.
- **Machine-readable**: `--json` on `check`, `list`, `status`, `up`, `watch` and
  `events`, all carrying `schema_version`; one error format, on stderr, JSON
  under `--json`.
- **Integration**: `generate systemd|launchd`, shell completions for five
  shells, man pages, prebuilt Linux and macOS binaries for `x86_64` and
  `aarch64`, published on crates.io.
- **Forward compatibility**: an `unknown` fallback on every wire enum, an
  optional `version` on `ping`/`pong`, and a refusal that names the request it
  did not recognise.

### Deferred to 1.1

- **An event envelope.** Every event would carry `timestamp`, `seq` and
  `schema_version` of its own, so a consumer could order, deduplicate and
  version-check a stream without inferring any of it. Deferred on purpose:
  adding fields to an existing shape is an additive change semver permits, so
  1.1 can do it without a break, and shipping it in 1.0 would have meant
  freezing a design that has not yet met a second consumer.
- **`status` from a killed daemon.** Reconciling orphaned children after a
  `SIGKILL` needs state on disk that survives the process, which is a design
  question rather than a patch — see
  [What survives a daemon that is killed](#what-survives-a-daemon-that-is-killed-and-what-does-not)
  for what an operator does today.

Windows supervision remains a non-goal.

---

## Non-goals

- **Not a container orchestrator** — use Docker Compose / Podman Compose / Kubernetes for containers.
- **Not a production server daemon** — use systemd, supervisor, or s6 for production workloads.
- **No TUI** — the focus is on simple CLI + a future HTTP/socket API; a TUI can be built on top.
- **No plugin system** — keep it small and auditable.

---

## License

Dual-licensed under **MIT OR Apache-2.0** — see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
