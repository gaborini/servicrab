# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The declared minimum supported Rust version is now **1.85**, up from the
  previously documented 1.75. The old number was never true: the dependency
  tree in `Cargo.lock` contains crates that require edition 2024, so a 1.75
  toolchain failed before compiling a single line. Nothing in servicrab itself
  changed — only the promise now matches reality.

### Added

- A CI job that checks the workspace on the declared MSRV with `--locked`, so
  the minimum supported version is verified on every push instead of being an
  unchecked claim in the manifest.

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
