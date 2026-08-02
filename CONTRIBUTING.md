# Contributing to Servicrab

Thank you for your interest in contributing! Servicrab is in early development, so there's plenty of opportunity to shape the project.

---

## Code of Conduct

Be respectful and constructive. This project follows the
[Contributor Covenant](CODE_OF_CONDUCT.md); reporting instructions are in that
file.

---

## Getting started

1. **Fork** the repository on GitHub.
2. **Clone** your fork locally:
   ```sh
   git clone https://github.com/<your-username>/servicrab
   cd servicrab
   ```
3. **Create a branch** for your change:
   ```sh
   git checkout -b feat/my-feature
   ```
4. **Make changes**, then run the checks locally before pushing. These are the
   same commands CI runs, `--locked` included — without it cargo may resolve a
   newer transitive dependency and you would be testing something other than
   what `Cargo.lock` ships:
   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   cargo test --workspace --all-features --locked
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
   cargo package --workspace --locked
   ```
5. **Open a pull request** against the `main` branch.

---

## What happens to your pull request

`main` is protected, so a change reaches it through a pull request that has:

- **the six CI checks green** — `fmt + clippy + test` on Linux and macOS, the
  MSRV check, the Windows stub build, the crates.io packaging dry run, and the
  rustdoc build with `-D warnings`;
- **one approving review** from a maintainer;
- **every review conversation resolved**.

The `Audit` workflow's `cargo-deny` job is not one of the six. It runs on a pull
request only when `Cargo.toml`, `Cargo.lock` or `deny.toml` changed, on the
grounds that a newly published advisory should not fail an unrelated change.

Your branch does not have to be up to date with `main` before merging: this is a
small project, and forcing a rebase for every unrelated commit costs more than it
catches. If `main` moved in a way that actually conflicts with your change, CI
will say so after the merge, and the fix belongs to whoever merged it.

Force-pushing to `main` and deleting it are blocked for everyone, maintainers
included. Your own branch is yours: force-push it as much as you like.

### Dependency updates

Dependabot's weekly pull requests are the exception to the review rule: a bot
cannot collect an approval, so the `Dependabot auto-merge` workflow approves them
and turns on auto-merge. They still wait for the same five checks, and the merge
only happens if those are green — which for a dependency bump is the review that
matters.

Major-version updates are excluded and wait for a human, including a grouped
update that contains a single major bump. A major version is where a dependency
is allowed to break us on purpose.

---

## Minimum supported Rust version

Servicrab builds on Rust **1.85** and newer. A CI job checks the workspace on
exactly that toolchain with `--locked`, so an accidental use of a newer feature
fails the pipeline rather than a user's build.

Raising the MSRV is allowed when there is a reason for it, but it is a
deliberate change: bump `rust-version` in the workspace `Cargo.toml` (the CI job
reads the number from there), mention it in the changelog, and say why in the
pull request. To reproduce the job locally:

```sh
rustup toolchain install 1.85
cargo +1.85 check --workspace --all-features --all-targets --locked
```

---

## Development tips

- The workspace uses a **resolver = "2"** so that feature flags are resolved per-crate.
- **File names are `snake_case`**, matching the module names they define
  (`validation.rs`, `stack_stub.rs`). There is no `camelCase` file in the
  workspace and no `*.test.rs` convention; if a tool tells you otherwise, it is
  guessing from a different language.
- **Imports are absolute**: `use crate::…` inside a crate,
  `use servicrab_core::…` across crates. `use super::*;` appears only inside
  `#[cfg(test)] mod tests`, to reach the module under test.
- Keep the CLI's concerns out of `servicrab-core`. The split is not "no I/O" —
  core reads config files, walks `PATH`, opens TCP health probes and runs on
  `tokio` — it is that **core never formats output for a terminal**: it returns
  typed values and publishes structured events, and the CLI decides how they
  look. So no `clap`, no styling, no terminal detection, and no `println!` in
  core; and no supervision logic in the CLI.
- Platform-specific process code lives behind `#[cfg]` in
  `crates/servicrab-core/src/runtime/`, with a stub that returns
  `RuntimeError::UnsupportedPlatform` elsewhere. Windows must keep compiling —
  CI checks it — so a new runtime entry point needs a stub alongside it.
- Add unit tests in the same file as the code under test (in a `#[cfg(test)] mod tests { … }` block).
- Add integration tests under `tests/` in the relevant crate only when you need to exercise the binary.

---

## Commit messages

A subject line is a short imperative sentence saying what the commit does, and
preferably why. There is no `type:` prefix convention — the history is plain
sentences, and a `feat:`/`fix:` prefix would be the odd one out:

```
Add restart-policy validation
Handle an empty command in the run subcommand
Read the config version before the parse that its own keys would fail
```

Use the body for the reasoning that will not be obvious in six months. That is
where this project puts the *why*, in prose.

---

## Releasing

For maintainers. The version lives in three places in the workspace
`Cargo.toml`: `[workspace.package]`, and the `version` of the two internal
dependencies at the bottom.

1. Bump all three, then `cargo update --workspace --offline` so `Cargo.lock`
   follows.
2. Move the `Unreleased` entries in `CHANGELOG.md` under the new version
   heading, with the date, and add the compare link at the bottom of the file.
3. Land that on `main` and wait for CI. A maintainer may push it directly;
   branch protection exempts admins from the pull-request requirement, and a
   release commit has nobody to review it.
4. Tag it and push the tag:

   ```sh
   git tag -a vX.Y.Z -m "servicrab X.Y.Z"
   git push origin vX.Y.Z
   ```

   The `Release` workflow builds `x86_64` and `aarch64` binaries for Linux and
   macOS, attaches them (with `.sha256` files and the generated man pages) to a
   GitHub release, and takes the release notes from the matching `CHANGELOG.md`
   section.

5. Publish to crates.io. One command does all three crates in dependency order
   and waits for the index in between:

   ```sh
   cargo publish --workspace --locked
   ```

Tag first, publish second: a crates.io version can never be deleted or reused,
so it should only ever describe a commit that is already tagged and green.

---

## Reporting issues

Use the [issue templates](https://github.com/gaborini/servicrab/issues/new/choose);
they ask for what a report usually needs:
- Your operating system and Rust version (`rustc --version`)
- The `servicrab.toml` you are using (redact secrets)
- The exact command you ran and the output

Found a security problem instead? Do not open an issue — see
[SECURITY.md](SECURITY.md).

---

## License

By contributing you agree that your contributions will be licensed under the same **MIT OR Apache-2.0** dual license as the rest of the project.
