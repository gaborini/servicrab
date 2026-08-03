# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-08-03

The first stable release. From here the CLI surface, the socket protocol and the
`--json` output follow semantic versioning; the Rust API of the internal crates
and the exact wording of human-readable output do not. See
[what 1.0 promises](README.md#what-10-promises) for the boundary.

Almost everything below is the release audit: the output contract, the exit
codes, the socket's privacy, and a supervisor that keeps the process-group
guarantee it always claimed. Read the breaking changes first.

### Breaking changes

A 0.x script can depend on all of these. Each one is deliberate.

- **`down` exits `3` when no daemon was running, where it used to exit `0`.**
  This breaks `servicrab down && …`: on an already-stopped stack the right-hand
  side no longer runs. `down` still never *fails* because nothing was there —
  running it twice is safe, which is the point of it — the code just
  distinguishes the two outcomes. The README gives the `case $?` replacement
  under
  [`3`, and the one breaking change in it](README.md#3-and-the-one-breaking-change-in-it).
- **"No daemon is running for this project" is exit `3` everywhere.** It was `1`
  for `status`, `stop`, `restart`, `start SERVICE`, `reload` and `events`, and
  `0` for `down`. A script that read `1` as "the command failed" now sees a
  different number for the one case it most often wants to handle rather than
  report.
- **That note is now a diagnostic on stderr**, for `status` and `down` alike; it
  used to go to stdout. `servicrab status > snapshot` on a stopped stack leaves
  the file empty instead of putting prose where a table belongs, and
  `status --json` still writes its object to stdout.
- **`list --json` prints an object, not a bare array.** The services are still an
  array, now under a `services` key alongside `project` and `schema_version`, so
  `jq '.[]'` becomes `jq '.services[]'`. A top-level array has nowhere to put a
  version.
- **Under `--json`, errors are JSON on stderr**, carrying `schema_version` and a
  `code`. They used to be a text `error: …` line even when the caller had
  explicitly asked for machine-readable output.
- **`status --json`'s "not running" reply goes through serde** like the running
  one, so the two cannot drift. It used to be a hand-written compact string;
  both are pretty-printed now and carry the same keys. Anything comparing that
  reply byte-for-byte will see a difference.
- **Every error is one format**: stderr, an `error: ` prefix, and the individual
  problems as bullets below it. `stop`, `restart` and `reload` used to print a
  bare `✗ …` with no prefix, and `check` printed its errors itself and then let
  the top level summarize them again. Output scrapers will need updating; `✗`
  remains in `check`'s human report, but not as an error marker.
- **`max_restarts = 0` now means unlimited restarts, not "give up on the first
  failure".** Zero was accepted with no validation and emptied the budget, which
  is what `restart = "never"` already says, and the opposite of what Compose and
  systemd mean by it. A config written from that habit silently got a supervisor
  that never retried. Configs that meant the old reading should say
  `restart = "never"`.
- **`health.retries = 0` is refused instead of being quietly rewritten to `1`.**
  A config that loaded before now fails validation. There is no sensible reading
  of zero — a service cannot be called unhealthy without one failed probe — and
  every other out-of-domain value in the validator already errored.
- **`servicrab logs` exits `0` on an empty log directory** and says so. A stack
  that has not run yet is a state of the world, not a command that failed. It
  still exits `1` when there is no log file to read at all: no `[project.logs]`
  table, or a named service with `[logs] enabled = false`.
- **The daemon's socket has moved for projects with long paths.** A project whose
  path overflowed `sun_path` used to put its socket in the shared temp directory
  under a name derived from a hash of the config path. That location is gone;
  the socket now goes to `$XDG_RUNTIME_DIR`, or to `$TMPDIR` when that is a
  directory this user owns with no group or other bits — plain `/tmp` fails that
  check by design. With no private directory available the long path stays put
  and the bind reports `ENAMETOOLONG` rather than trading a startup failure for
  a spoofable socket. When there is no daemon, `status` names the relocated
  socket, which is the moment someone starts looking for it. The length check
  now runs on the canonicalised path, so `-c a/servicrab.toml` and
  `-c ./a/servicrab.toml` are one project rather than two.
- **The daemon serves only its own uid.** It asks the kernel who connected and
  refuses anyone else with one error line naming both uids. `sudo servicrab
  status` against a user's daemon — a mistake the generated systemd unit's
  `User=` invites — now fails instead of working.
- **`SIGHUP` shuts the stack down instead of killing the supervisor.** Closing
  the terminal on a foreground `run` or `up` used to kill servicrab instantly
  and leave every service's process group running; it now performs the ordinary
  ordered shutdown and exits `129`. Like `SIGINT` and `SIGTERM` it counts as
  user-requested, so `restart = "always"` does not resurrect a service whose
  operator has gone away.
- **The reload error over the socket is one line plus a list.** It used to be an
  entire multi-line validation report — newlines, bullets and all — stuffed into
  a single JSON string, so a client that split that string on newlines will need
  to read `errors` instead.
- **In `servicrab-core`, a control command's acknowledgement carries a
  `ControlOutcome` or a `ControlRefusal` rather than `Result<String, String>`.**
  A source break for anything using the crate directly, which the 1.0 promise
  deliberately does not cover. Both types still `Display` as exactly the
  sentences they replaced.

### Deprecated

- `ServiceInfo.pid`, in favour of the new `pgid`, which carries the same number.
  It always was a process-group id — its own doc comment said so, and
  `Event::Started` calls the same value `pgid` — and signalling it as a pid
  reaches only the group leader. `pid` still ships, so nothing breaks yet.

### Added

- The `ping`/`pong` exchange now carries the revision of the wire format each
  side speaks. It is optional, because 0.3 spoke this format without naming it
  and "did not say" has to stay distinguishable from "said 0" — a client that
  read silence as a mismatch would refuse to talk to a 0.3 daemon. Nothing
  refuses on the strength of the number; the daemon logs a client that is
  behind, which is where an operator chasing version skew is already looking.

- `--color=auto|always|never`, and `--no-color` as a shorthand for
  `--color=never`. Both are global, so they can go anywhere on the command line.
  `CLICOLOR_FORCE` is honoured too, for colouring a stream that is a pipe. The
  order of precedence, from strongest: `--color`, `NO_COLOR`, `CLICOLOR_FORCE`,
  `TERM=dumb`, and finally whether the stream is a terminal.

- `servicrab check --json`, emitting the project name, the service count, the
  start order and the profile membership — and, when the config does not load,
  a structured error list rather than a paragraph. `check` is the most scripted
  command and its whole job is reporting problems, so those had to become data.
- Every `--json` output now carries a `schema_version`, so a script can refuse a
  stream it was not written for instead of guessing. Whole documents
  (`check --json`, `list --json`, `status --json`) carry it as a top-level key;
  the NDJSON streams (`up --json`, `watch --json`, `events --json`) open with
  the same `{"type":"ok","schema_version":1}` handshake the daemon answers
  `subscribe` with, rather than repeating the version on every event line.
- `Response::Error` gained a `code` field — `unknown_service`, `busy`,
  `not_running`, `already_running`, `validation_failed`, `unsupported`,
  `failed` — and, for a validation failure, an `errors` list. `message` was the
  only thing to match on before, and it is prose. A request the daemon has no
  variant for is classified `unsupported`, alongside the sentence naming it.
- `Response::Ok` for a reload gained `changes`, reporting `{added, changed,
  removed}` as numbers beside the sentence that used to be the only way to
  learn them.
- `ServiceInfo` gained a `pgid` field, carrying the process-group id that `pid`
  was always reporting. See Deprecated, above.
- Exit code `3`, meaning "no daemon is running for this project" — a dedicated
  code so that a script can tell "there was nothing to talk to" from a real
  failure without matching on the message. Which commands used to say what is in
  Breaking changes, above.
- Configuration warnings for the two settings that are accepted and do nothing:
  a `[services.<name>.logs] enabled` with no `[project.logs]` table, which
  captures nothing because the router is only built when the project declares a
  log directory, and an explicit `on_unhealthy = "restart"` alongside
  `restart = "never"`, where the unhealthy service is stopped and never comes
  back. `check` used to print a bare `✓` for both. The default `on_unhealthy`
  with `restart = "never"` is not warned about: that is the legitimate
  readiness-gating setup.
- A `LogLinesDropped { count }` event, rendered by `up`, by `events` and in the
  status registry, so a service that outruns the log path says how much was
  lost rather than losing it silently. See Performance, below, for the
  allowance it reports against.
- A `Dependabot auto-merge` workflow. `main` now requires a pull request with
  green checks and one approving review, which a bot cannot collect, so the
  weekly dependency bumps are approved and queued for auto-merge from CI
  instead. Major-version updates are left for a human, including a grouped
  update that contains one — the checks are a real review for a patch bump, and
  not much of one for a breaking change.

### Changed

The output and exit-code changes are all in Breaking changes, above, because
every one of them is something a 0.x script could be reading. What is left:

- `message` on a `Response` is documented as being for people, and explicitly
  not part of the API. The README no longer quotes the strings verbatim, since
  doing so was what made them one.
- `servicrab list` and `servicrab logs` print the configuration warnings they
  used to discard, on stderr, as `run`, `up`, `exec`, `generate`, `daemon` and
  `check` already did. The user who reaches for `list` or `logs` to find out why
  a service is behaving oddly was the least likely to be told that part of their
  config does nothing — and `logs` in particular fails on the very configs a
  warning would explain. `list --json` stays parseable on stdout.
- The hand-stop record in `.servicrab/stopped` is written atomically — to a
  temporary file, fsynced, renamed — and gains an optional version line, so the
  format can change later. A file written by 0.3, or one an operator edited the
  line out of, still reads. Startup now also drops remembered names the
  configuration no longer declares, so a renamed or deleted service stops
  accumulating; names are compared against every declared service rather than
  the started plan, because a profile that is inactive this run is not a service
  that is gone. Deleting a line is still a legitimate way to forget a stop.
- The man page's `EXIT STATUS` section documents every code in the contract. It
  covered only `exec` and `run`, and never mentioned the `129`/`130`/`143` that
  `up` and `watch` exit with on a signal.
- The crates.io description of `servicrab` no longer says "cross-platform". It
  names Linux and macOS, because supervision has never worked anywhere else and
  the description is the one line a prospective user reads before installing.
  `servicrab-core`'s description now mentions the process runtime it contains.

### Security

- The daemon socket is no longer placed in the shared temp directory under a
  predictable hashed name, and the daemon no longer serves other users. Both are
  described in Breaking changes, above, because the socket's location can move
  and a cross-uid client that used to work now does not. What they close: the
  old path was guessable by every local user and steerable through `$TMPDIR`, so
  squatting it was a permanent denial of service, and binding a listener there
  and answering `pong` made every `status`, `stop`, `down`, `reload` and `events`
  talk to the attacker — and leak project and service names on the way. The uid
  check is the second line: a filesystem that ignores Unix modes, or a directory
  an operator loosened, would otherwise hand over the whole stack.
- The socket is private from the instant it exists. Binding it and only then
  restricting the mode left it live and group-writable in between — `0775` on
  exactly the distributions that use private user groups with `umask 002` — and
  connecting is all it takes to start, stop or shut down every service in the
  project. The mode is now part of creating the inode, set by narrowing the umask
  around the `bind`; the `chmod` stays as a check for a platform that ignores the
  umask for sockets, not as the mechanism.
- One socket client can no longer exhaust the daemon. Everything past `accept`
  is reachable before a byte has been understood and none of it was bounded: an
  unterminated line grew a `String` until the OOM killer intervened, a silent
  client held a task and a descriptor for the daemon's life, malformed lines
  could be pumped in forever, and there was no cap on concurrent connections.
  There is now a 64 KiB frame limit, a 30-second idle timeout, a ceiling of 64
  live connections, and a connection is closed after three malformed lines — a
  client that then talks sense has its strikes forgiven, so a typo costs
  nothing. The `subscribe` filter became a size-limited `BTreeSet`; it was a
  client-supplied `Vec` rescanned for every event and every subscriber.


### Fixed

- A newer daemon could end an older client's event stream with a single line.
  `#[non_exhaustive]` is a promise to downstream *crates* and says nothing about
  a line of JSON: serde rejects an unrecognised tag outright, so one unknown
  event kind failed to decode and `servicrab events` exited with an error,
  mid-run, taking every event behind it. One new event type in a later release
  would have done that to every client of every earlier one. Each enum the
  daemon can widen now has an `unknown` fallback: a client skips what it cannot
  name, keeps reading, and passes the line through verbatim under `--json`, so a
  consumer that knows more than that build does loses nothing. The same applies
  to a `status` snapshot, where one unfamiliar state or health verdict used to
  fail the whole reply.

  A request a daemon does not recognise now gets `this daemon does not support
  the request "strat"; it supports: ping, status, …` rather than `malformed
  message: unknown variant …`, which read as "your client is broken" when the
  truth was "this daemon is older than your client". The name and the list are
  both there deliberately: deciding an unknown request is no longer a decode
  error also throws away what serde said about it, and a typo in a hand-rolled
  client is a far more common reason to see this message than a genuinely newer
  client is.

- A config written for a later schema reported one of its own keys as a typo
  instead of naming the version. The version check ran after the field-level
  parse, and that parse is fatal — so `version = 2` plus any key a future schema
  would add came back as `unknown field 'new_key_from_v2'`, with not a word
  about the version. The one message that tells an operator to upgrade servicrab
  was unreachable for exactly the files it was written for. `version` is now
  read first, by a pass that names nothing else. An unrecognised key inside a
  `version = 1` file is still fatal: there it is a typo, and a misspelled
  `comand` that loaded quietly would be far worse than one that refuses.

- Colour is decided from the stream being written to, rather than from stdout
  for everything. Most of what Servicrab renders — the `up` and `watch` banner
  and status lines, `events`, the `start --wait` progress — goes to stderr, so
  `servicrab up 2> stack.err` used to write ANSI escapes into the file, and
  `servicrab up | cat` used to drop the colour that stderr's terminal justified.
  Each stream is now asked about itself, and a run can legitimately colour one
  and not the other.

- `servicrab man` and `servicrab completions <SHELL>` no longer fail when the
  reader goes away. `servicrab man | head` reported a broken pipe as an error
  and exited 1; `servicrab completions bash | head` panicked inside the
  generator, which writes to the stream directly. Both now treat it as the
  reader having seen enough, and exit 0.

- `servicrab logs --follow` no longer prints lines twice. It sampled the file
  length, read to whatever the end was by then — which could be past that
  sample — and then rewound to the stale sample, so everything appended in
  between came out again on the next pass. The offset is now the position the
  read actually reached.

- `servicrab logs --follow` no longer prints half of a line and then the whole
  of it. A file that ends mid-line is what a service being written to looks
  like; the fragment is now held back until its newline arrives. Without
  `--follow` there is no next pass, so the fragment is still shown.

- `servicrab logs` tolerates output that is not UTF-8. One stray byte — a binary
  blob, another encoding, a multi-byte character caught mid-write — used to fail
  the whole command, and cut a `--follow` silently short at that line.
  Undecodable bytes are replaced and the rest of the log is readable.

- `servicrab list` no longer panics on a command containing multi-byte
  characters. The preview sliced `cmd_str[..29]` and `len()` counts bytes, so a
  config as ordinary as `command = ["echo", "x€€€€€€€€€"]` aborted with "end byte
  index 29 is not a char boundary" and exit 101. An accented path or an emoji in
  any argument was enough. The preview now cuts on a character boundary.

- Two daemons can no longer supervise one project. Checking whether the socket
  answers and then binding it is a time-of-check/time-of-use race, and a tokio
  runtime gets built in the window: interleaved, the second start unlinked the
  first daemon's live socket and bound its own, both supervised the whole stack —
  duplicate processes, duplicate port binds — and whichever exited first deleted
  the other's socket and pidfile. An exclusive `flock` on the pidfile, held for
  the daemon's whole life, gives mutual exclusion the kernel enforces and that
  survives `SIGKILL`. `servicrab start` had the same race one level up and now
  watches the daemon it spawned rather than only the socket, so the loser of a
  race stops reporting "daemon started" for a daemon it did not start — and a
  fifteen-second timeout becomes an immediate, accurate message.

- The process-group guarantee holds on the paths that used to skip it. `stop_all`
  aborted a supervision task that outstayed its grace period and left cleanup to
  `kill_on_drop`, which reaches only the direct child, so a grandchild such as
  `node` under `npm` was orphaned; the same gap sat behind every `?` in
  `ServiceRunner::run`. The supervisor now sweeps the last reported pgid before
  aborting, and a process handle sweeps its own group on drop unless it has
  already been swept.

- A `reload` no longer leaves a service's process group behind. `diff` reports a
  retiring slot as added and inserting it overwrote the still-winding-down
  task's stop channel and join handle, so the replacement service was
  unreachable to `stop_all` and its group outlived the supervisor. A revived slot
  keeps its channel and handle and defers the spawn, so exactly one process per
  service is ever alive.

- A service with no dependents recorded its readiness. `watch::Sender::send`
  returns without writing the value when there is no receiver, so a running,
  healthy service kept a stale `Pending`, and a later `reload` that gave it a
  dependent blocked on that stale value — for a stable service, indefinitely.

- A hand-stop is no longer lost to a concurrent one. Recording a stop was an
  unlocked read-modify-write over a `fs::write` that truncates first, so two
  stops arriving on two connections lost one of them, and a crash in the window
  left a file that reads back as "nothing was ever stopped".

- Shutdown signals are claimed before the daemon becomes reachable. The socket is
  bound before the async runtime exists, so `SIGTERM`, `SIGINT` and `SIGHUP` kept
  their default disposition in between and a signal there killed the daemon
  outright, leaving a socket file for the next start to misread.

- The socket is removed when the runtime itself fails to start; that early return
  used to leave the file on disk.

- A peer refused for its uid is told why, in one line naming both uids, instead
  of having the connection closed in silence — which every client reported as
  "the daemon closed the connection without answering". The commands that gate on
  "is a daemon running" no longer read a refusal as an absent daemon and send the
  operator looking for one to start.

- A socket that cannot be bound is explained once, up front, by every command
  that needs one — naming the directory that was refused and why. The rejections
  used to be reachable only from `bind`, so the daemon reported `ENAMETOOLONG`,
  every client reported `SUN_LEN` from `connect`, and the message that would let
  an operator fix it reached nobody.

- A failing per-service command reports what it printed, rather than only that it
  failed.

- `servicrab up` honours Ctrl+C when nobody is reading its output. Drawing is
  synchronous, so a consumer that stopped reading parked the renderer in a write
  that never returned, and shutdown joined that renderer — so `up` never exited
  at all, while the services were already stopped and the exit code already
  decided. The wait is now bounded: log files drain first because they are the
  durable half, and the terminal tail is best effort.

- A stopping service is reported promptly even with a health check. The monitor
  held a clone of the event sink between probes, keeping the service's event
  channel and its relay alive, which is what the supervisor waits on before
  reporting the run. That cost a fixed 2s on every stop of a health-checked
  service — 2.02s before, 0.02s after, measured with a 30s interval — and on an
  unbounded wait it exceeded the client's timeout and failed the command outright
  while the service had already stopped. A probe in flight is still allowed to
  finish, so no child is abandoned mid-run.

- An output reader that cannot drain is aborted rather than detached, so the stop
  can be reported. `tokio::time::timeout` consumes the `JoinHandle` and dropping
  a handle only detaches, so the reader kept its event sink, the relay never
  ended, and the report the daemon's `stop` and `restart` wait for never came —
  the client hit its 10-second read timeout and reported "Resource temporarily
  unavailable" for a service that had exited cleanly. This is what made the
  daemon suite fail roughly one run in ten.

- The filewatch debounce loop terminates. It waited for quiet unconditionally, so
  anything changing faster than `debounce` — a log inside the watched tree, a
  build directory — kept it waiting forever while it re-scanned the whole tree
  every period and never requested a restart. It now gives up waiting after ten
  rounds and restarts on what it has. Separately, the tree walk was recursive
  with no depth bound, so a deep tree overflowed the stack and aborted the
  supervisor; descent stops at 64 levels and says so.

- A dead health probe is no longer a perpetual event source.
  `HealthProbeFailed` went out on every failing tick with no ceiling, so
  `on_unhealthy = "ignore"` plus a permanently dead probe meant an event per
  interval, published to every subscriber and written into the status registry.
  The two reports that carry information are kept — the first failure and the one
  that exhausts the retry budget — and between them the reports thin out to one
  per hundred, with a recovery resetting the throttle.

### Performance

- File and directory work is off the async workers. The log router did
  `create_dir_all`, `open`, `write_all` and a flush per line straight from the
  pumps in `up`, `run` and the daemon collector, and the filewatch scan did a
  `read_dir` per directory plus a `symlink_metadata` for up to 20 000 entries,
  four times per round, inline. Those pumps share their threads with the
  supervisor, so a full or network-backed disk stalled the very threads driving
  child `wait()`s, health probes and the control channel. Both now run on
  blocking tasks; the log writer buffers and flushes when it catches up with the
  queue, and at least every 256 lines while a flood keeps it full.

- Captured output is bounded. Every internal channel was unbounded and captured
  stdout travels line by line, so a service that floods stdout grew the
  supervisor's heap without limit and told nobody. Log lines now have an
  allowance of 1024 queued per event channel and the newest beyond it is dropped
  — the newest, because it is the only one the sender can still choose to lose,
  so what survives stays in order. Everything the supervisor says *about* a
  service still goes through unconditionally: losing a state change would corrupt
  the status registry.

- A full log queue drops the line rather than stalling the pump that fills it.
  The queue was back-pressure, which reintroduced one stage further along exactly
  what moving the file work off the worker was meant to prevent — the daemon's
  collector also keeps the status registry current and feeds every `events`
  subscriber. A log with a hole in it that says where the hole is beats a
  supervisor that stops answering.

- A flood no longer reports its own losses without bound. One delivered line was
  read as the flood letting up, but it frees exactly one slot the flood refills
  at once, so the loss was announced once per delivered line: 1536 reports for
  2560 lines, measured. A let-up now means the consumer won back half the
  allowance.

### Documentation

The release audit found the documentation making claims the code does not
support, so this round verified each one against the binary and corrected it.
The substantive corrections, rather than the wording:

- **The README documents every configuration field's range.** Not one bound was
  written down before, so the only way to learn that `stable_after` has a floor
  of a second or that `max_files` stops at 100 was to be refused by `check`.
  There is a new reference table with every field's type, default and range.
- **`servicrab-core` is no longer described as free of I/O and async.** Its
  crate docs said "all I/O is delegated to callers" and `CONTRIBUTING.md` said
  to keep it "free of I/O and async dependencies", while the crate reads config
  files, walks `PATH`, opens TCP health probes, and depends on `tokio`. The
  boundary that does exist — core never formats output for a terminal — is now
  what all three documents describe.
- **The durability guarantee is stated as it is, not as intended.** The README
  said the socket and the pidfile are removed "when the daemon exits, however it
  exits". Only the graceful path unlinks them; after `SIGKILL`, an OOM kill or a
  panic both files survive and the supervised children are orphaned, because
  nothing reconciles them on the next start. There is a new section on what to
  do about that, including the `pgrep -fl 'servicrab daemon'` that finds the
  orphans — and a measured count of what that command found on one development
  machine, in place of the smaller anecdote that had been there.
- **`--profile` and the `-c` alias appear in the command reference**, which
  listed neither.
- The README no longer claims a `--json` event stream and `servicrab down` are
  unimplemented; both shipped in 0.1.0. `SECURITY.md`'s supported-versions table
  no longer stops at `0.1.x`. The bug-report template no longer suggests a
  two-year-stale version string. The "exactly what CI runs" list was missing the
  packaging dry run and counted `cargo deny`, which is not a required check.
- `${VAR}` substitution is documented as applying to string-valued fields, which
  is what it has always done — `max_restarts`, `logs.max_files`,
  `health.retries`, `autostart`, `logs.enabled` and `restart` are not strings.
- New sections state what v1.0 freezes (the CLI surface, the socket protocol and
  the JSON output; not the Rust API of the internal crates, and not the column
  alignment of `--help`), collect the exit codes in one table matching the man
  page, and replace the roadmap of five simultaneously-current phases.

A new test, `crates/servicrab-cli/tests/config_reference.rs`, keeps the range
table honest: for each documented bound it runs `check` on a config sitting on
the bound and one just past it, so widening a limit in `validation.rs` without
updating the README fails the suite. `tests/help.rs` and `tests/contract.rs` pin
the help text and the output contract the same way.

## [0.3.0] - 2026-07-30

### Added

- `servicrab exec <SERVICE> -- <COMMAND>...`, for the questions that start with
  "but what does the service actually see?":

  ```sh
  servicrab exec api -- printenv DATABASE_URL
  servicrab exec api -- npm run migrate
  servicrab exec db -- psql
  ```

  It runs the command with the service's merged environment, its `env_file`
  layers and its working directory, assembled by the same code that starts the
  service, so the two cannot drift apart. The command inherits servicrab's
  stdio — interactive tools and pipes work, and nothing of servicrab's own
  reaches the output — and its exit status is passed through, with `127` for a
  command that does not exist and `126` for one that is not executable, as a
  shell would.

  Unlike `docker exec` it does not enter a running process: no daemon is
  involved and no namespace is joined. That is the point. Debugging a service
  that refuses to start is exactly when there is nothing to attach to, and it is
  also the limit — a variable the process changed after startup is not visible
  here.

  Everything after the service name belongs to the command, including its own
  flags.

## [0.2.0] - 2026-07-30

Mostly about the config file: a stack of a dozen services can now be split
across files, carry per-checkout values, keep its optional parts out of the way
until asked for, and say what "ready" means on each dependency edge.

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

[1.0.0]: https://github.com/gaborini/servicrab/compare/v0.3.0...v1.0.0
[0.3.0]: https://github.com/gaborini/servicrab/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/gaborini/servicrab/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/gaborini/servicrab/releases/tag/v0.1.0
