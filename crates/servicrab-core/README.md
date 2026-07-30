# servicrab-core

Configuration models, validation, and the service lifecycle runtime behind
[servicrab](https://github.com/gaborini/servicrab) — a lightweight process
supervisor for local development stacks, homelabs, and small servers.

This crate parses and validates `servicrab.toml`, resolves the dependency
graph, and supervises processes (process groups, restart policies with
exponential backoff, health probes, log capture, file watching). It never
formats output: it publishes structured events and lets the CLI render them.

The process runtime is Linux and macOS only; on other platforms the runtime
entry points return `UnsupportedPlatform`.

Most people want the [`servicrab`](https://crates.io/crates/servicrab) CLI
instead of this crate.

Dual-licensed under MIT OR Apache-2.0.
