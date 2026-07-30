# Servicrab

A **lightweight, cross-platform process supervisor** for local development stacks, homelabs, and small servers — written in Rust.

Think of it as a minimal, zero-dependency alternative to [overmind](https://github.com/DarthSim/overmind) or [Honcho](https://github.com/nicksylett/honcho), with a roadmap toward daemon-based management and a local API.

---

## Features (v0.1)

- Declare your entire local stack in a single `servicrab.toml`
- `servicrab init` — scaffold a config in seconds
- `servicrab check` — validate your config before running anything
- `servicrab list` — see all services and their restart policies at a glance
- `servicrab run <service>` — supervise a single service in the foreground with live stdout/stderr, restart policy, and process-group shutdown (Linux/macOS)
- `servicrab up` — run your whole stack in the foreground: dependency-ordered start, interleaved and colour-prefixed output, reverse-order shutdown (Linux/macOS)
- Health checks: `command`, `http` and `tcp` probes with readiness gating and automatic restart of unhealthy services
- `servicrab watch` — restart a service as soon as its sources change, with ignore rules and debouncing
- Log files: opt-in per-service capture with size-based rotation, plus `servicrab logs [-f]` to read and follow them
- Background daemon: `servicrab start` / `status` / `stop` / `restart` / `down`, with a documented JSON-over-Unix-socket protocol (Linux/macOS)
- `servicrab reload` — apply config changes to a running stack without stopping the services you did not touch
- Restart policies: `never`, `on-failure`, `always`, with exponential backoff
- Environment: per-service variables, working directories, and dotenv-style `env_file` layering
- Shell completions for bash, zsh, fish, PowerShell and elvish
- Dependency declarations and a deterministic start order

---

## Installation

### From source

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

## Command reference

| Command | Description |
|---|---|
| `servicrab init [--path PATH] [--force]` | Generate an example `servicrab.toml` |
| `servicrab check [--config PATH]` | Parse and validate the config file |
| `servicrab list [--config PATH] [--json]` | List all services with their restart policies |
| `servicrab run <SERVICE> [--config PATH] [--no-restart]` | Supervise one service in the foreground |
| `servicrab up [SERVICE...] [--config PATH] [--no-restart] [--no-prefix] [--timestamps] [--abort-on-failure]` | Supervise a whole stack in the foreground |
| `servicrab watch [SERVICE...] [--config PATH] [--no-restart] [--no-prefix] [--timestamps] [--abort-on-failure]` | Like `up`, and restart services when their watched files change |
| `servicrab logs [SERVICE...] [--config PATH] [-f] [-n N] [--no-prefix]` | Show (and follow) the captured log files |
| `servicrab start [--config PATH] [--no-restart]` | Start the stack in the background |
| `servicrab status [--config PATH] [--json]` | Show what the background daemon is doing |
| `servicrab stop <SERVICE...> [--config PATH]` | Stop individual services in the running daemon |
| `servicrab restart <SERVICE...> [--config PATH]` | Restart individual services |
| `servicrab reload [--config PATH]` | Re-read the config and apply the difference to the running daemon |
| `servicrab down [--config PATH]` | Stop the daemon and every service it supervises |
| `servicrab daemon [--config PATH] [--no-restart]` | Run the daemon in the foreground (for systemd/launchd/containers) |
| `servicrab completions <SHELL>` | Print a completion script for bash, zsh, fish, PowerShell or elvish |

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

```
servicrab up acme-stack → redis, api
redis | ▶ started (pgid 41234)
redis | Ready to accept connections
api   | ▶ started (pgid 41235)
api   | listening on 0.0.0.0:3000
```

A service's own stdout is written to Servicrab's stdout and its stderr to
Servicrab's stderr, so `servicrab up > stack.log` keeps the two apart. Colour is
disabled automatically when the output is not a terminal, when `NO_COLOR` is
set, or when `TERM=dumb`.

### Which services are started

- with no arguments: every service with `autostart = true`;
- with explicit names: those services only;
- in both cases every transitive `depends_on` entry is pulled in, even when it
  has `autostart = false`.

### Start and stop ordering

Services start in the configuration's deterministic topological order, and a
service is only spawned once every service it depends on is **available**:

- a service without a health check is available as soon as its process is up;
- a service **with** a health check is available only after its first
  successful probe;
- a one-shot dependency (a migration, a build step) that exited cleanly counts
  as available too.

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

### Platform support and current limitations

`servicrab up` supports **Linux and macOS**. Current limitations:

- `up` runs in the foreground and stops with your shell — use `servicrab start` for a detached stack;
- no `--json` event stream yet;
- `servicrab down` does not exist yet (Ctrl+C is the way to stop a stack).

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

```bash
servicrab logs                 # last 50 lines of every service, prefixed
servicrab logs api -n 200      # last 200 lines of one service, unprefixed
servicrab logs -f              # follow new output (Ctrl+C to stop)
```

`logs -f` notices rotation and keeps following the fresh file. Without a
`[project.logs]` table the command tells you how to enable capture rather than
printing nothing.

Sizes accept `B`, `KB`/`KiB`, `MB`/`MiB`, `GB`/`GiB` and `TB`/`TiB` suffixes;
all of them are powers of 1024.

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
api | ▶ started (pgid 40311)
api | ↻ server.js changed; restarting
api | ◼ stopping: stopped on request
api | ▶ started (pgid 40388)
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

There is no variable expansion: what is written is what the service receives.
A missing file, an unterminated quote or a line without `=` is a configuration
error, reported by `servicrab check` with the file name and line number — the
stack never starts with a half-loaded environment.

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

```
SERVICE  STATE          PID    UPTIME  RESTARTS  HEALTH
db       running      41231     4m30s         0  healthy
api      running      41244     4m12s         1  -
worker   backoff          -         -         3  -
  worker: stopped (exit code 1), last status: exit code 1
```

The daemon keeps its runtime state next to the config file, in
`.servicrab/`: `daemon.sock` (the control socket), `daemon.pid`, and
`daemon.log` (its own diagnostics — service output goes to the log files
described above). Add `.servicrab/` to your `.gitignore`.

Each project gets its own daemon, so several stacks can run side by side.
`start` refuses to launch a second daemon for the same config, and both the
socket and the pidfile are removed when the daemon exits, however it exits.

`servicrab daemon` runs the same thing in the foreground, which is what you
want under systemd, launchd, or in a container: it supervises the stack, serves
the socket, and stops the whole stack cleanly on `SIGTERM`.

### The socket protocol

Clients speak newline-delimited JSON — one request per line, one response per
line — so anything that can write to a Unix socket can drive servicrab:

```bash
echo '{"type":"status"}' | nc -U .servicrab/daemon.sock
```

| Request | Response |
| --- | --- |
| `{"type":"ping"}` | `{"type":"pong","project":"…","pid":123}` |
| `{"type":"status"}` | `{"type":"status","services":[…]}` |
| `{"type":"shutdown"}` | `{"type":"ok","message":"stopping the stack"}` |
| `{"type":"start_service","name":"api"}` | `{"type":"ok","message":"api started"}` |
| `{"type":"stop_service","name":"api"}` | `{"type":"ok","message":"api stopped"}` |
| `{"type":"restart_service","name":"api"}` | `{"type":"ok","message":"api restarted"}` |
| `{"type":"reload"}` | `{"type":"ok","message":"reloaded demo: 1 added, 1 changed, 0 removed"}` |

`stop_service` and `restart_service` only answer once the service has actually
stopped (and, for a restart, been replaced), so scripts can rely on the reply
instead of polling.

### Config hot-reload

`servicrab reload` makes the running daemon re-read its `servicrab.toml` and
apply only what changed:

```console
$ servicrab reload
✓ reloaded demo: 1 added, 1 changed, 1 removed
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
✗ servicrab.toml has 1 error(s); the stack was left untouched:
  • service "api": depends on unknown service "ghost"
```

Project-level settings — `[project.logs]`, the log directory and rotation
rules — are bound to the daemon process and need a `servicrab down` +
`servicrab start` to change. File watchers (`[services.x.watch]`) *are*
re-created on every reload.

The wire types live in the `servicrab-protocol` crate, which depends on
neither the runtime nor Tokio.

---

## Workspace layout

```
servicrab/
├── crates/
│   ├── servicrab-cli/      # Binary crate — clap CLI + Tokio async runtime
│   ├── servicrab-core/     # Library — config models, validation, lifecycle + process runtime
│   └── servicrab-protocol/ # Library — daemon request/response wire types
├── Cargo.toml              # Workspace manifest
└── servicrab.toml          # (generated by servicrab init)
```

---

## Development

```sh
# Format
cargo fmt --all

# Lint (warnings are errors in CI)
cargo clippy --all-targets --all-features -- -D warnings

# Test
cargo test --all

# Run a specific test
cargo test -p servicrab-core config::tests
```

---

## Roadmap

### Phase 1 (current) — Minimal CLI ✅
- [x] Cargo workspace setup
- [x] `servicrab.toml` config format
- [x] `init` / `check` / `list` commands
- [x] Restart policy types
- [x] Dependency declarations and deterministic start order
- [x] CI on Linux + macOS

### Phase 1.5 (current) — Foreground runner ✅
- [x] `servicrab run <SERVICE>` on Linux + macOS
- [x] Per-service process groups and group-wide shutdown
- [x] Restart policy enforcement with exponential backoff
- [x] Graceful shutdown with `SIGKILL` escalation

### Phase 1.6 (current) — Stack runner ✅
- [x] `servicrab up` — concurrent supervision of the whole stack
- [x] Dependency ordering on start, reverse order on shutdown
- [x] Interleaved, prefixed, colourised output

### Phase 1.7 (current) — Health checks ✅
- [x] `command`, `http` and `tcp` probes
- [x] Readiness gating: dependents wait for a healthy dependency
- [x] Unhealthy services are stopped and restarted by policy

### Phase 1.8 (current) — Log files ✅
- [x] Opt-in capture to `<dir>/<service>.log` with size-based rotation
- [x] `servicrab logs [SERVICE...] [-f] [-n N]`
- [x] Per-service opt-out via `[services.<name>.logs] enabled = false`

### Phase 2 — Background daemon ✅
- [x] Detached daemon per project with a Unix-socket JSON API
- [x] `servicrab start` / `status` / `down` / `daemon`
- [x] Status snapshot: state, pid, uptime, restarts, health
- [x] Per-service `start` / `stop` / `restart` through the daemon
- [ ] Live event streaming over the socket

### Phase 3 — Stack management ✅
- [x] `.env` file support per project and per service
- [x] Shell completions (`servicrab completions <SHELL>`)
- [x] `servicrab watch` — restart on file changes, with ignore rules and debouncing
- [x] Config hot-reload (`servicrab reload`)

### Phase 4 (current) — Platform integration
- [ ] systemd unit generation
- [ ] launchd plist generation
- [ ] Windows Service wrapper

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
