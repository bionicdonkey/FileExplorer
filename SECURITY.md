# Security policy

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it privately through GitHub:
**Security → Report a vulnerability** on this repository (private vulnerability reporting).

Include what you found, the version or commit, and the smallest steps that reproduce it. This is a
one-person project, so expect an acknowledgement within about a week rather than hours; a fix and a
disclosure date will be agreed with you.

## What is supported

Only the latest release of each app, and `main`. There are no long-term-support branches.

| App | Supported |
| --- | --- |
| Lorin Explorer (Linux) | latest `linux-v*` release |
| Morin Explorer (macOS) | latest `macos-v*` release (none has been published yet) |

## What is most worth reporting

Both apps move, copy and delete the user's own files, so anything that makes them act on files the
user did not choose is in scope. In particular:

- **Privilege boundaries.** Each app can retry a permission-denied operation with elevated rights,
  and is built so that the app itself never runs elevated. On Linux it re-runs itself through
  `pkexec` to perform exactly one validated operation; on macOS a privileged helper registered with
  `SMAppService` does the same over XPC, and accepts only local absolute paths from a caller that
  satisfies its code-signing requirement. A way around either, such as an operation the helper
  should have refused, a path that escapes validation, or a caller that is not the app, is a
  vulnerability.
- **Archives.** Both apps read and write archives in-process (libarchive). Path traversal out of the
  extraction folder, writes through symlinks, and decompression bombs are in scope.
- **Persisted state.** Operation history and search history are redacted and bounded on purpose. A
  way to make them record paths or terms they should not, or to make an oversized or hostile state
  file crash or hang the app, is in scope.
- **Location handling.** Parsing and displaying URIs and paths (including percent-encoded and
  archive-addressed ones).

## What is out of scope

- Behaviour that needs an attacker who can already run code as the user or edit the app's own
  configuration folder.
- Remote locations (`smb://`, `sftp://` and similar) on macOS, which the app does not support.
- Findings against a build you modified yourself.
- Problems in the operating system, GTK, libadwaita, AppKit or libarchive themselves; please report
  those upstream (tell me too, if this app can be made to avoid the trigger).

## Known limits

Release signing and notarization of the macOS app have not been run yet, so a Mac build you make
yourself is ad-hoc signed and its helper cannot be approved the way a signed release's can. This is
stated in `macos/README.md`.
