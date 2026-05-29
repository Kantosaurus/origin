<!--
Thanks for contributing to origin! Please fill out the checklist below.
Keep the PR to one logical change (see CONTRIBUTING.md).
-->

## Summary

<!-- What does this change do, and why? Link any related issue (e.g. "Closes #123"). -->

## Type of change

- [ ] `fix` — bug fix
- [ ] `feat` — new feature
- [ ] `refactor` / `perf` — no behavior change / performance
- [ ] `docs` — documentation only
- [ ] `chore` / `test` — tooling, CI, or tests

## Quality gates

> These are the same checks CI runs. Please run them locally before pushing.

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes (no new `#[allow]` without a one-line justification)
- [ ] `cargo test --workspace` passes
- [ ] No new `unsafe` (forbidden outside the audited `cas`/`tui`/`ipc` crates)
- [ ] Bug fixes include a **regression test** that fails without the fix
- [ ] Touched a hot path (IPC / CAS / render tick / agent loop)? Ran `origin-bench` and noted any perf delta
- [ ] Updated docs and the `## Unreleased` section of `CHANGELOG.md` if behavior, config, or a public API changed

## Notes for reviewers

<!-- Anything that needs special attention, trade-offs considered, follow-ups deferred. -->
