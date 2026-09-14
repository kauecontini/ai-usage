# Usage

A tiny, local-first Windows utility that shows your remaining **ChatGPT / Codex** and **Claude** subscription usage without becoming another dashboard.

> V0.1.0 source is implemented. Real authenticated Windows runtime verification is a release gate and is tracked explicitly below; no quota readings in this repository are fabricated.

## What it shows

The compact surface is intentionally small:

`[OpenAI mark] 72% · 46%    [Anthropic mark] 81% · 33%`

Percentages are **remaining**, not consumed. The detail surface shows provider-reported usage windows and reset times when available.

## Features

- compact, draggable Windows 11 widget
- ChatGPT/Codex subscription usage through the local official Codex app-server
- Claude subscription usage through an isolated read-only adapter
- 5-hour and weekly/7-day windows when providers expose them
- system tray, hide/show, refresh, single-instance behavior
- optional always-on-top and launch-at-startup
- configurable 1 / 3 / 5 / 10 / 15 minute refresh presets (3 minutes by default)
- restrained 20% / 10% / 5% native notifications without repeat spam per quota window
- stale-cache indication instead of presenting old data as fresh
- no Usage backend, telemetry, analytics, accounts, or cloud database

## Supported providers

### ChatGPT / Codex

Usage talks to the installed official `codex` CLI through `codex app-server` and reads its account rate-limit snapshot. It does **not** make a model inference request and does not read raw Codex credential files.

A ChatGPT-authenticated Codex installation is required. API-key-only Codex use is not treated as ChatGPT subscription quota.

### Claude

Usage reuses the Claude Code OAuth session on Windows and queries Anthropic's usage service read-only. This interface is not a documented public Anthropic API, so this adapter is deliberately isolated and fails closed on schema changes.

## Installation

Release builds are intended to be downloaded from GitHub Releases as a normal Windows installer. End users do not need Node.js, Rust, Python, Git, or a terminal.

V0.1.0 remains **unreleased until the Windows release gate passes**. Building source today is for development verification only.

The initial V0.1.0 installer is **unsigned** unless a maintainer supplies an appropriate Windows code-signing certificate. Windows SmartScreen may therefore show an unknown-publisher warning on first launch. Usage does not bundle or purchase a certificate automatically; signed releases can be added once the project has a proper certificate and signing-key process.

## How it works

Provider-specific code lives entirely in the Rust side of the Tauri application. The React webview receives only normalized non-secret usage windows and status values. Provider credentials never cross the Tauri IPC boundary.

See [`docs/ADR-001-provider-integrations.md`](docs/ADR-001-provider-integrations.md) for the provider decision record.

## Privacy

Usage is local-first:

- no telemetry or analytics
- no cloud backend
- no prompt, message, or conversation collection
- no sale or sharing of user data
- only normalized quota values may be cached locally

See [`PRIVACY.md`](PRIVACY.md).

## Security

- provider adapters are read-only
- secrets are never written to Usage settings/cache
- sensitive values are excluded from diagnostics and passed through a central redaction layer
- the Claude bearer token is sent only to the fixed Anthropic HTTPS origin and redirects are refused
- Codex is invoked with fixed structured arguments; no arbitrary user input is passed to a shell

See [`SECURITY.md`](SECURITY.md) and [`SECURITY_REVIEW.md`](SECURITY_REVIEW.md).

## Development

Requirements:

- Windows 11 recommended for full desktop verification
- Node.js 22+
- Rust stable compatible with Tauri 2.11
- WebView2

```text
npm install
npm run typecheck
npm run test
npm run lint
npm run tauri dev
```

Rust checks:

```text
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Architecture

```text
src/
  components/        compact/detail UI primitives
  lib/               presentation helpers
src-tauri/src/
  core/              types, normalization, settings, stale/backoff, redaction
  providers/openai   Codex app-server adapter
  providers/anthropic isolated Claude usage adapter
```

There is intentionally no server, database, Redux store, browser automation, or historical analytics layer.

## Known limitations

- Claude quota retrieval depends on an undocumented interface exposed by Anthropic's client/service and may require updates when that interface changes.
- Only Windows is a V0.1.0 distribution target.
- High-DPI and monitor-removal recovery needs final validation on real Windows hardware before the first release.
- The source repository cannot prove a user's authenticated provider behavior; the release gate requires an actual signed-in Windows run.
- The initial V0.1.0 installer is unsigned and may trigger Windows SmartScreen until a code-signing certificate/process is added.
- V0.1.0 does not include automatic updates. Tauri's signed updater is deferred until a signing key/release process exists rather than shipping an unsafe or incomplete updater.

## Troubleshooting

If a provider shows **Sign in required**, open the corresponding official client and sign in, then use **Refresh now**. If it shows **Stale**, Usage kept the last known non-secret percentages because the current refresh failed.

Do not attach provider credential files, tokens, cookies, or Authorization headers to bug reports.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Original project source is MIT licensed. See [`LICENSE`](LICENSE).

Provider names and logos are trademarks of their respective owners and are not licensed as Usage project artwork. See [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## Disclaimer

Usage is an independent open-source utility and is not affiliated with, endorsed by, or sponsored by OpenAI or Anthropic.
