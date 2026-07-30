# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `restart = "unless-stopped"`, for the service you sometimes run yourself:

  ```toml
  [services.api]
  command = ["node", "server.js"]
  restart = "unless-stopped"
  ```

  It restarts exactly like `always`, except that a service stopped with
  `servicrab stop` stays stopped across `servicrab down` and the next
  `servicrab start`. A hand-stopped service was already left alone for as long
  as its daemon lived, whatever its policy; what this policy adds is the memory.
  `servicrab start api` hands the service back.

  The memory is a list of names in `.servicrab/stopped`, plain text so that
  deleting it — or a line of it — forgets a stop. It records every hand stop but
  only `unless-stopped` services act on it, so adopting this changes nothing
  about an existing stack. Dependents of a held-back service start out stopped
  too: a service cannot run without what it declares in `depends_on`, and
  starting one to wait for something nobody will start is not better than
  leaving it alone. `start --wait` does not wait for either kind, and
  `servicrab up` ignores the whole thing — a foreground run has nothing to
  remember.
- Profiles, for the services you only sometimes want:

  ```toml
  [services.mailhog]
  command = ["mailhog"]
  profiles = ["dev"]
  ```

  A service that declares no profiles is always started; one that declares any
  waits for `servicrab up --profile dev` (or `start`, `watch`, and `generate`,
  which writes the flag into the unit it produces). Several `--profile` flags
  add up, and a service in several profiles joins when any one of them is
  enabled. `servicrab list` shows the groups, in the table and in `--json`.

  Naming a service starts it whatever its profiles say, so a profiled service
  never needs the flag to be targeted directly. Because that makes explicit
  names a second way of selecting, passing both is refused rather than silently
  resolved in favour of one.

  Dependencies come along regardless of their profiles: a service can never run
  without what it declares in `depends_on`. The daemon keeps the profiles it was
  started with, so `servicrab reload` re-plans the same stack instead of the
  smaller one a bare `start` would have produced. A `--profile` no service
  declares is an error listing the ones that exist.
- `include`, so a stack of a dozen services does not have to live in one file:

  ```toml
  version = 1
  include = ["services/db.toml", "services/api.toml"]
  ```

  An included file holds `[services.<name>]` tables and may include further
  files; `version` and `[project]` stay in the config every command is pointed
  at, not least because the project name decides where the daemon keeps its
  socket.

  Relative paths in an included file belong to that file — its own `include`
  entries, and the `cwd` and `env_file` of the services it declares — so a
  fragment can be moved together with the code it describes.

  Merging is not overriding: two files declaring the same service, an `include`
  cycle, the same file included twice, and `version` or `[project]` in an
  included file are all configuration errors, reported with the file names
  involved. `include` paths are not `${VAR}`-substituted, because which files
  make up a config should not depend on who ran it.
- `${VAR}` substitution in every value of `servicrab.toml`, so a committed
  config can serve checkouts that disagree about where things live:

  ```toml
  [services.api]
  command = ["${NODE:-node}", "server.js"]
  cwd = "${WORKSPACE}/api"
  ```

  `${VAR:-default}` falls back when the variable is unset or empty, `${VAR-default}`
  only when it is unset, and `$${VAR}` is a literal `${VAR}`. An unset variable
  with no default is a configuration error naming the variable and the field,
  not an empty string: a `cwd` that silently became `/` or a `command` that
  silently lost an argument is worse than a config that refuses to load.

  Unlike Docker Compose, the braces are required — a bare `$` is never special.
  Half the commands in a process manager are shell snippets, and eating the `$i`
  of `while ...; do echo $i; done` at load time, against the wrong environment,
  would cost more than it saves.

  Values are read from the environment servicrab runs in, not from
  `[project.env]`, `[services.<name>.env]` or an `env_file`, which describe what
  the *service* will see. Table keys and the project and service names are not
  substituted; the project name decides where the daemon keeps its socket, and a
  control socket that moves with the environment would be a debugging trap.
- Docker-Compose-style conditions on `depends_on`, spelled with the table form:

  ```toml
  [services.api.depends_on]
  db = { condition = "service_healthy" }
  migrate = { condition = "service_completed_successfully" }
  ```

  `service_completed_successfully` is the one that was missing: it is the only
  condition that looks at the exit status, so a dependent is no longer started
  against a half-migrated database when the migration exits non-zero. The other
  two conditions keep treating a one-shot that has exited as available whatever
  its status, because a condition a finished process can never meet again would
  deadlock the stack.

  The list form and its behaviour are unchanged, and leaving the condition out
  is deliberately *not* the same as `service_started`: it still means "healthy
  if the dependency declares a health check, up otherwise", so adding a health
  check keeps gating everything that depends on that service. Spelling out
  `service_started` is now the way to opt out of that for one edge.

  Conditions that can never be met are rejected at load time: `service_healthy`
  on a service with no `[health]` block, and `service_completed_successfully` on
  a service with `restart = "always"`, which never stays exited.
- `servicrab start --wait` returns only once every service is ready — running,
  and health-checked if it declares a health check — with `--timeout` to bound
  the wait (60s by default). A one-shot service that has already exited counts
  as ready, which is the same definition the supervisor uses to release a
  dependent. The exit code says whether it worked, so a CI script can stop
  guessing with `sleep`. The daemon is left running on failure, because a stack
  that came up wrong is easier to diagnose alive.
- `servicrab man` prints the man page in roff, or writes one page per command
  into a directory with `--output`. Release tarballs now ship the generated
  pages under `man/`. The pages come from the same command definitions as
  `--help`; files, environment variables and exit codes are documented by hand
  on the main page.
- A dependency audit (`cargo-deny`) with the policy in `deny.toml`: RustSec
  advisories, unmaintained crates, licences and package sources. It runs in its
  own `Audit` workflow — on dependency changes, and weekly, because advisories
  are published on someone else's schedule.
- Dependabot for the cargo and github-actions ecosystems, grouped into one
  pull request per week.
- A CI job that compiles the workspace on Windows. Supervision is still
  Unix-only, but the `cfg(not(unix))` stubs are now built by something other
  than hope.
- Community files a public repository is expected to have: `SECURITY.md`,
  `CODE_OF_CONDUCT.md`, issue forms for bugs and features, and a pull request
  template.
- A CI job checks the workspace on the declared MSRV with `--locked`, so the
  minimum supported version is verified on every push instead of being an
  unchecked claim in the manifest.

### Changed

- `servicrab list --json` reports each dependency as an object rather than a
  bare name, so the condition being waited for is machine-readable:
  `"depends_on": [{"service": "db", "condition": "service_healthy"}]`. The
  condition is the effective one, resolved for entries that omit it. The human
  output names it too.
- The daemon socket is now created with mode `0600`. It was left to the process
  umask, which is `022` on most systems but `002` on distributions that give
  each user a private group — and connecting to the socket is enough to start
  and stop every service in the project.
- CI runs clippy and the test suite with `--locked`. Without it a transitive
  dependency could be silently upgraded, and CI would stop testing what
  `Cargo.lock` ships.
- The declared minimum supported Rust version is now **1.85**, up from the
  previously documented 1.75. The old number was never true: the dependency
  tree in `Cargo.lock` contains crates that require edition 2024, so a 1.75
  toolchain failed before compiling a single line. Nothing in servicrab itself
  changed — only the promise now matches reality.

### Fixed

- A flaky `up` test. It asserted the start order by having the dependent sleep
  300ms and then look for a marker file written by the dependency, which
  measures how the two shells happen to be scheduled rather than what the
  supervisor did; on a loaded machine it failed while the supervisor had behaved
  correctly. It now reads the start order out of the event stream.

## [0.1.0] - 2026-07-30

The first release. Supervision runs on Linux and macOS; on other platforms the
config commands work and the runtime reports `UnsupportedPlatform`.

### Configuration

- `servicrab.toml` with a validated schema: project metadata, services,
  dependencies, environments, health checks, log settings and watch rules.
  Unknown keys are rejected rather than silently ignored.
- `servicrab init` writes an annotated example config; `servicrab check`
  validates one; `servicrab list` prints the services and their policies.
- Per-project and per-service `env_file` layering on top of the process
  environment.
- Dependency declarations with a deterministic topological start order and
  cycle detection.

### Running services

- `servicrab run <SERVICE>` supervises one service in the foreground.
- `servicrab up [SERVICE...]` supervises a whole stack: dependency-ordered
  start, interleaved and colour-prefixed output, reverse-order shutdown,
  `--abort-on-failure`.
- `servicrab watch` restarts services when their watched files change, with
  ignore rules and debouncing.
- Restart policies `never`, `on-failure` and `always`, with exponential
  backoff, a restart ceiling, and a stability window.
- Per-service process groups: shutdown signals the whole group and escalates
  to `SIGKILL` after `shutdown_timeout`, so no orphans are left behind.
- Health checks (`command`, `http`, `tcp`) with readiness gating for
  dependents and automatic restart of unhealthy services.

### Logs

- Opt-in capture to `<dir>/<service>.log` with size-based rotation, and a
  per-service opt-out.
- `servicrab logs [SERVICE...] [-f] [-n N]` reads and follows them.

### Background daemon

- `servicrab start` / `status` / `stop` / `restart` / `down` / `daemon`,
  one detached daemon per project.
- A documented newline-delimited JSON protocol over a Unix socket, published
  as the `servicrab-protocol` crate.
- `servicrab reload` applies config changes to a running stack without
  touching the services that did not change; an invalid config is refused and
  leaves the stack running.
- `servicrab events` follows the daemon's live event stream, as text or JSON,
  optionally filtered by service.

### Platform integration

- `servicrab generate systemd|launchd` writes a unit that runs the stack, with
  `systemctl reload` wired to `servicrab reload`.
- `servicrab completions <SHELL>` for bash, zsh, fish, PowerShell and elvish.
- `servicrab up --json` and `servicrab watch --json` emit the same event lines
  the daemon streams, for scripts and wrappers.

[Unreleased]: https://github.com/gaborini/servicrab/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/gaborini/servicrab/releases/tag/v0.1.0
