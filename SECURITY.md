# Security Policy

## Reporting a vulnerability

If you believe you've found a security vulnerability in remora-etcher
(for example, something that could let a crafted image or disk lead to
memory corruption, arbitrary write outside the intended target, or
privilege escalation during flashing/provisioning), please **do not**
open a public GitHub issue.

Instead, report it privately using
[GitHub's private vulnerability reporting](https://github.com/sntns/remora-companion/security/advisories/new)
for this repository, or email **security@sentiens.fr** with:

- A description of the issue and its potential impact.
- Steps to reproduce, or a minimal proof-of-concept image/input.
- The affected version/commit and target OS/architecture.

We'll acknowledge your report and aim to keep you updated as it's
investigated and fixed. Please give us a reasonable amount of time to
address the issue before any public disclosure.

## Supported versions

This project is pre-1.0 and does not yet maintain separate maintenance
branches. Security fixes are applied to `main` and the latest tagged
release.
