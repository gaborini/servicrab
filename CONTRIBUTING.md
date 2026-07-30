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
   same commands CI runs, so a clean sweep here means a green pipeline:
   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   ```
5. **Open a pull request** against the `main` branch.

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
- Keep `servicrab-core` free of I/O and async dependencies — it must stay usable from both the CLI and future daemon without pulling in the full Tokio runtime.
- Add unit tests in the same file as the code under test (in a `#[cfg(test)] mod tests { … }` block).
- Add integration tests under `tests/` in the relevant crate only when you need to exercise the binary.

---

## Commit messages

Use short imperative sentences:

```
Add restart-policy validation
Fix: handle empty command in run subcommand
Docs: update README roadmap
```

---

## Releasing

1. Update `version` in the workspace `Cargo.toml` and move the `Unreleased`
   entries in `CHANGELOG.md` under the new version heading.
2. Merge that to `main` and check CI is green.
3. Tag it and push the tag:

   ```sh
   git tag -a v0.2.0 -m "v0.2.0"
   git push origin v0.2.0
   ```

The `Release` workflow builds `x86_64` and `aarch64` binaries for Linux and
macOS, attaches them (with `.sha256` files) to a GitHub release, and takes the
release notes from the matching `CHANGELOG.md` section.

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
