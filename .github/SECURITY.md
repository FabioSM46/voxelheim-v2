# Security Policy

Voxelheim is pre-alpha software and does not currently have a supported production release.
Security reports are still welcome because the repository contains networked client/server code
and automation that handles repository credentials.

## Reporting a vulnerability

Do not open a public issue, discussion or pull request for a suspected vulnerability.

Use GitHub's **Security → Report a vulnerability** form instead. It opens a private security
advisory that is visible only to the reporter and repository collaborators while the report is
being assessed.

Include, when possible:

- the affected component and revision;
- reproduction steps or a minimal proof of concept;
- the expected impact;
- any suggested mitigation;
- whether the issue has been disclosed anywhere else.

You should receive an acknowledgement within seven days. Timelines for validation and remediation
depend on severity and project maturity. Please allow time for a fix before public disclosure.

## Scope

Reports about the authoritative Go server, Rust client, FlatBuffers protocol, GitHub Actions
workflows and repository automation are in scope. Reports that require access to credentials not
present in the repository, social engineering, denial-of-service testing against third-party
services or access to another person's account are out of scope.
