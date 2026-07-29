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
- Restart policies: `never`, `on-failure`, `always`, with exponential backoff
- Per-service environment variables and working directories
- Dependency declarations and a deterministic start order (ordering is enforced once multi-service supervision lands)

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

Current limitations:

- one service per invocation — `depends_on` is validated but not yet acted on;
- no background daemon, no `up` / `down`;
- no health or readiness checks;
- no log files or log rotation — output is inherited, so redirect it yourself;
- no `--json` event stream yet.

---

## Workspace layout

```
servicrab/
├── crates/
│   ├── servicrab-cli/      # Binary crate — clap CLI + Tokio async runtime
│   ├── servicrab-core/     # Library — config models, validation, lifecycle + process runtime
│   └── servicrab-protocol/ # Library — future daemon request/response wire types
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

### Phase 2 — Background daemon
- [ ] Background daemon process with Unix socket / named-pipe API
- [ ] `servicrab start` / `stop` / `restart` / `status` commands
- [ ] Dependency ordering on start
- [ ] Concurrent supervision of the whole stack
- [ ] `servicrab logs <service>` — stream live logs

### Phase 3 — Stack management
- [ ] `servicrab up` / `servicrab down` — whole-stack lifecycle
- [ ] `servicrab watch` — restart on file changes (à la `watchexec`)
- [ ] `.env` file support per service
- [ ] Health-check probes (HTTP + command)
- [ ] Config hot-reload

### Phase 4 — Platform integration (optional)
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
