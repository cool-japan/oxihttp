# Security Policy

## Supported Versions

OxiHTTP is pre-1.0 software. Only the latest released `0.x` line (as published
on crates.io) is supported with security fixes. Older `0.x.y` patch releases
and unreleased branches are not backported to; please upgrade to the latest
release before reporting an issue.

## Reporting a Vulnerability

**Please do not file a public GitHub issue for a security vulnerability.**
Public issues are indexed and searchable, and a public report can put users
at risk before a fix is available.

Instead, report suspected vulnerabilities privately:

- **Email:** info@kitasan.io
- **Maintainer:** COOLJAPAN OU (Team Kitasan)

Please include, where possible:

- A description of the vulnerability and its potential impact.
- Steps to reproduce, or a minimal proof-of-concept.
- The affected crate(s) and version(s) (`oxihttp`, `oxihttp-client`,
  `oxihttp-server`, or `oxihttp-core`).
- Any known mitigations or workarounds.

## Our Process

- Reports are triaged privately by the maintainer.
- We will acknowledge receipt as soon as practical and keep you updated as
  the investigation progresses.
- Once a fix is ready, we will coordinate a release and, where appropriate,
  a public advisory that credits the reporter (unless anonymity is
  requested).
- Please give us a reasonable amount of time to address the issue before
  any public disclosure.

## Scope

This policy covers the crates published from this repository
(`oxihttp`, `oxihttp-client`, `oxihttp-server`, `oxihttp-core`) and their
default (Pure-Rust) feature set. Issues in upstream dependencies should be
reported to those projects directly, though we appreciate a heads-up so we
can track exposure in OxiHTTP.
