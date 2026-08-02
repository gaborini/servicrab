# Security policy

## Reporting a vulnerability

Please report security issues **privately**, through GitHub's private
vulnerability reporting:

**<https://github.com/gaborini/servicrab/security/advisories/new>**

(On the repository page: *Security* → *Report a vulnerability*.)

Do not open a public issue for something that could be exploited before there
is a fix.

Helpful details, roughly in order of usefulness:

- what an attacker gains, and what access they need to start with;
- the `servicrab.toml` and the commands that reproduce it (redact secrets);
- your operating system and `servicrab --version`;
- a patch or a rough idea of a fix, if you have one.

Servicrab is maintained in spare time, so please expect a first reply within a
week rather than within hours. You will get an acknowledgement, an assessment,
and — if the report is valid — a fix, a release, and a published advisory that
credits you unless you prefer otherwise.

## Supported versions

Only the latest release is supported. Fixes go onto `main` and into the next
release; there are no long-lived maintenance branches, and there is no backport
to an older line.

| Version | Supported |
| ------- | --------- |
| 1.0.x   | yes       |
| 0.x     | no        |

From 1.0 onwards the CLI surface, the socket protocol and the JSON output follow
semver, so upgrading to the current release to pick up a fix should not require a
change to your configuration or your scripts. See
[what 1.0 promises](README.md#what-10-promises).

## What is in scope

Servicrab starts and stops processes, so the interesting questions are about
who gets to make it do that, and with which privileges.

In scope:

- **The daemon socket.** `servicrab start` listens on a Unix socket
  (`.servicrab/daemon.sock` next to the config, or a file in the temp directory
  when that path would exceed the socket length limit). Anyone who can connect
  to it can start, stop, restart and reload the project's services, as the user
  running the daemon. The socket is created with mode `0600`, so the file
  permissions are the only thing standing between another local account and
  those commands — a way to bypass them is a vulnerability. There is no
  authentication beyond that, by design.
- **Privilege handling.** Servicrab may be run as root, for example from a
  generated systemd unit. Anything that lets a service, a config file or a
  socket client end up with more privileges than the operator intended is in
  scope. So is a generated unit or launchd job that is more permissive than the
  configuration asked for.
- **Secrets in output.** Environment values loaded from `env_file` should not
  turn up in `status`, `events`, the captured log files or the daemon log.
- **Path and file handling.** Log file and rotation handling, the pidfile, and
  the socket path — for example a symlink or race that makes servicrab write
  somewhere it should not.
- **Anything that crashes or hangs the daemon** from outside the config: a
  malformed protocol frame on the socket should be an error response, not a
  panic or a deadlock that leaves supervised services orphaned.

## What is not in scope

- **The configuration file is code.** `servicrab.toml` says which commands to
  run, with which environment and working directory. Loading someone else's
  config is equivalent to running their shell script; servicrab does not
  sandbox services and does not try to. Treat `servicrab.toml` with the same
  trust as a `Makefile`.
- **The services themselves.** What a supervised process does with its
  privileges is the service's business.
- **Resource exhaustion configured on purpose.** A restart policy that
  hammers a failing command, or a health check interval of a millisecond, is a
  configuration mistake and not a vulnerability.
- **Windows.** Supervision is unimplemented there; the runtime reports
  `UnsupportedPlatform`. Reports about the non-Unix stubs are welcome as normal
  issues.
- **Dependency advisories.** These are already checked by the `Audit` workflow
  (`cargo-deny`). A normal issue or pull request is the right channel, unless
  the advisory is exploitable *through* servicrab in a way the upstream
  advisory does not cover.
