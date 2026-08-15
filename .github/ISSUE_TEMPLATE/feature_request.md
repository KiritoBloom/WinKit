---
name: Feature request
about: Suggest a new capability or improvement for WinKit
title: "[feature] "
labels: enhancement
assignees: ''
---

## Problem statement

What can't you do today with WinKit? Describe the agent workflow this would
unblock.

## Proposed solution

What should the new tool, adapter, or behavior look like?

- Tool name (if a new MCP tool): 
- Capability it should require: 
- What it returns:
- How output is bounded (result cap / payload cap / timeout):

## Alternative approaches

What else did you consider, and why is this approach better?

## Notes on read-only policy

WinKit v1 is strictly read-only. If your request involves writing, modifying,
deleting, or executing on the host, it belongs to the future action-capability
track and should be framed as such.

## Checklist

- [ ] I searched existing issues for this feature
- [ ] This feature is compatible with WinKit's read-only security model
- [ ] I have described how the output stays bounded
