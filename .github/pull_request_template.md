## Summary

<!-- What does this change do, and why? One paragraph. Reference the issue it
closes, e.g. "Closes #123". -->

## Changes

- <!-- bullet list of the concrete changes -->

## Verification

- [ ] `cargo check` passes
- [ ] `cargo test --features mocks` passes (full suite)
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo fmt --check` passes

## Security review

<!-- Answer each item. This is mandatory for all PRs. -->

- [ ] The change is read-only (no writes, deletes, or execution on the host)
- [ ] New tools (if any) declare a capability and are gated by the permission
      policy
- [ ] New tools (if any) have bounded output (result cap, payload cap,
      timeout)
- [ ] No secrets are captured or logged (URLs, console text, event payloads
      are truncated where relevant)
- [ ] Docs updated: `docs/`, `config/example.toml`, `README.md` if behavior,
      schemas, config keys, or thresholds changed

## Limitations

<!-- Anything not covered by tests, known edge cases, or follow-up work. -->
