# ADR-001 — Provider usage integrations

Status: Accepted for V0.1.0  
Date: 2026-09-12

## Context

Usage must display **subscription quota**, not API billing, and must not spend model quota merely to discover quota state. Provider-specific behavior must be isolated and fail closed when upstream interfaces change.

## OpenAI / Codex

**Decision:** use the official local `codex app-server` over stdio and request `account/rateLimits/read`.

The current Codex app-server protocol exposes `rateLimits`, optional `rateLimitsByLimitId`, and primary/secondary windows containing `usedPercent`, `windowDurationMins`, and `resetsAt`. Usage launches the already-installed Codex CLI with fixed structured arguments, performs the normal initialize/initialized handshake, reads the rate-limit snapshot, and terminates the helper. It does not read Codex credential files and does not submit model work.

Failure mode: no CLI, no ChatGPT-authenticated subscription, protocol error, timeout, or schema drift becomes a typed unavailable/authentication/rate-limited state. The other provider remains usable.

## Anthropic / Claude

**Decision:** V0.1.0 uses an isolated, read-only **unofficial** adapter because no documented public Claude subscription-quota API or machine-readable `/usage` command is currently published.

On Windows, Claude Code officially stores credentials in `%USERPROFILE%\\.claude\\.credentials.json` (or under `CLAUDE_CONFIG_DIR`). Usage reads only the OAuth access token into memory and sends one GET request to the fixed Anthropic origin `https://api.anthropic.com/api/oauth/usage` with the OAuth beta header used by current Claude Code usage monitors. Redirects are refused. The response parser accepts the current self-describing `limits` shape and the legacy `five_hour`/`seven_day` shape.

The token is never copied to Usage settings/cache, never sent to the webview, never logged, and never uploaded anywhere except to Anthropic for the quota request. Usage never uses the refresh token.

Failure mode: unknown schema, auth failure, redirect, or unexpected HTTP state returns unavailable/authentication/rate-limited and does not guess.

## Consequences

- OpenAI integration has a cleaner first-party local interface and no direct credential handling.
- Claude support is more brittle and requires maintenance when Anthropic changes its private usage interface.
- Provider adapters remain independent, making upstream breakage contained.
- V0.1.0 documentation must disclose that provider interfaces may change without exposing credential extraction details to ordinary users.
