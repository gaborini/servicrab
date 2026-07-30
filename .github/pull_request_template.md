<!--
Thanks for the pull request. The checklist is the same set of commands CI runs,
so a clean sweep locally means a green pipeline.
-->

## What this changes

<!-- What the change does, and why. Link the issue if there is one. -->

## How it was verified

<!--
Which tests cover it, and anything you checked by hand that a test cannot
(for example: killed the daemon mid-reload, watched the socket recover).
-->

## Checklist

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-features`
- [ ] Tests cover the change (new behaviour, or the bug that is now fixed)
- [ ] `CHANGELOG.md` updated under `## [Unreleased]`, if the change is
      user-visible
- [ ] Documentation updated (`README.md`, the `servicrab init` example config,
      or the command's doc comment), if the change is user-visible
