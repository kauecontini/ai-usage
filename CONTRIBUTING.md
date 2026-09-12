# Contributing

Keep Usage small. A change should make the single job — showing remaining ChatGPT/Codex and Claude subscription usage on Windows — more reliable, safer, or clearer.

## Setup

Install Node.js 22+, Rust stable, WebView2, and the Tauri prerequisites for Windows. Then:

```text
npm install
npm run typecheck
npm run test
npm run lint
npm run tauri dev
```

Run Rust checks from `src-tauri`:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Architecture

Provider implementations live under `src-tauri/src/providers` and return the typed `ProviderUsage` contract. Provider-specific auth/protocol assumptions must not leak into UI code or another provider adapter.

## Provider changes

- prefer documented/official interfaces
- keep undocumented behavior isolated and documented in an ADR
- add sanitized fixtures/tests for response changes
- never commit authenticated raw responses
- fail closed on display accuracy

## Pull requests

Keep PRs focused. Include tests for parsing/normalization/security-sensitive behavior, run formatting/lint/tests, and call out any new dependency or credential/network behavior explicitly.
