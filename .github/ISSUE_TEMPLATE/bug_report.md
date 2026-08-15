---
name: Bug report
about: Report a bug in WinKit so we can fix it
title: "[bug] "
labels: bug
assignees: ''
---

<!--
Before filing: search existing issues, and confirm you can reproduce with the
latest code on `main`. Security issues must NOT be filed here — see
SECURITY.md.
-->

## Description

A clear, concise description of the bug.

## Steps to reproduce

1. Configuration used (`winkit.toml` or command line):
2. MCP client and the exact request:
3. Steps:

## Expected behavior

What you expected to happen.

## Actual behavior

What happened instead. Include the full stderr output if any.

## Environment

- WinKit version (`winkit --version`):
- Windows version and build:
- Rust toolchain (`rustc --version`):
- MCP client (OpenCode / Claude Code / other, with version):

## Additional context

- Does it reproduce with `cargo test --features mocks`?
- Any related logs, screenshots, or transcript snippets.

## Checklist

- [ ] I searched existing issues for this bug
- [ ] I have included a minimal reproduction
- [ ] This is not a security issue (those go through SECURITY.md)
