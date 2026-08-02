# servicrab

A lightweight process supervisor for local development stacks, homelabs, and
small servers — one `servicrab.toml`, one binary, no daemon required.

Supervision runs on Linux and macOS. On other platforms the config commands
work and the runtime reports `UnsupportedPlatform`.

```toml
version = 1

[project]
name = "myapp"

[services.api]
command = ["cargo", "run", "--bin", "api"]
restart = "on-failure"

[services.web]
command = ["npm", "run", "dev"]
depends_on = ["api"]
```

```console
$ servicrab up
servicrab up myapp → api, web
api | ▶ started (pgid 30292)
web | ▶ started (pgid 30293)
api | listening on :8080
web | ready on :3000
```

- Dependency-ordered start, reverse-order shutdown, per-service process groups
- Restart policies (`never`, `on-failure`, `always`, `unless-stopped`) with
  exponential backoff
- Health checks (`command`, `http`, `tcp`) with readiness gating
- Log files with rotation, plus `servicrab logs -f`
- A background daemon with a documented JSON-over-Unix-socket API, live event
  streaming, and config hot-reload
- systemd unit and launchd plist generation

See the [project README](https://github.com/gaborini/servicrab#readme) for the
full documentation.

Dual-licensed under MIT OR Apache-2.0.
