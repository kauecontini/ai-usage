# V0.1.0 Security Review

Review date: 2026-09-12  
Scope: source architecture before real-Windows release verification

## Findings addressed in implementation

- **Token leakage to webview:** provider secrets remain Rust-only; IPC models contain normalized quota/status data only.
- **Token leakage to logs:** diagnostics accept sanitized messages and pass through central redaction. Raw provider bodies and credential payloads are never logged.
- **Plaintext duplication:** Usage settings/cache do not contain provider secrets. Claude's existing credential store is read-only.
- **Overbroad file access:** the Claude adapter resolves one documented credential file only; other local files are not scanned.
- **Unsafe subprocesses:** Codex is invoked as a fixed executable name with fixed structured arguments and no user input or `shell=true` equivalent.
- **Arbitrary destinations:** Anthropic networking is hardcoded to one HTTPS origin.
- **Redirect credential leakage:** redirects are disabled on the bearer-authenticated Anthropic client.
- **Provider failure isolation:** adapters refresh independently and merge into separate typed states.
- **Stale accuracy:** last-known values are marked stale after refresh failure and never silently presented as fresh.
- **CI secrets:** workflows require no provider credentials and have minimal default permissions.
- **Updater supply chain:** automatic updates are deliberately deferred because a signing-key process is not yet established.

## Remaining release-gate checks

These cannot be truthfully completed from a non-Windows/non-authenticated build environment and must be done before tagging V0.1.0:

- inspect a real Windows release process for console flashes and child-process behavior
- verify both providers on actual authenticated subscriptions
- inspect resulting logs after successful and failed provider requests for secrets
- validate DPI/multi-monitor position recovery, tray behavior, native notifications, startup registration, and installer UX
- measure idle CPU/RAM from the packaged build
- re-run a secret scan over the final release artifacts (the 46-file source tree passed a pre-push credential/path-pattern scan)

No claim of `READY_FOR_DAILY_USE` is made until those checks pass.
