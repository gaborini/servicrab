# Contributing to Servicrab

Thank you for your interest in contributing! Servicrab is in early development, so there's plenty of opportunity to shape the project.

---

## Code of Conduct

Be respectful and constructive. We follow the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct).

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
4. **Make changes**, then run the checks locally before pushing:
   ```sh
   cargo fmt --all
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all
   ```
5. **Open a pull request** against the `main` branch.

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

Please include:
- Your operating system and Rust version (`rustc --version`)
- The `servicrab.toml` you are using (redact secrets)
- The exact command you ran and the output

---

## License

By contributing you agree that your contributions will be licensed under the same **MIT OR Apache-2.0** dual license as the rest of the project.
