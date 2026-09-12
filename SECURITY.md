# Security Policy

## Supported versions

Until V0.1.0 passes its Windows release gate, `main` is development software. After release, the latest minor release receives security fixes.

## Reporting a vulnerability

Please open a GitHub security advisory/private vulnerability report if enabled for this repository. If private reporting is unavailable, open a minimal issue asking for a private contact channel **without including secrets or exploit payloads that expose credentials**.

## Credential-handling principles

Usage:

- never logs access tokens, refresh tokens, API keys, cookies, Authorization headers, or credential file contents
- never persists another plaintext copy of a provider secret
- keeps provider adapters read-only
- does not send prompts or create/modify chats
- does not accept arbitrary network destinations
- does not use shell interpolation for provider commands
- refuses redirects on the bearer-authenticated Anthropic usage request

## Bug reports

Never upload:

- `.credentials.json` or other auth/session files
- OAuth tokens
- API keys
- cookies
- Authorization headers
- full authenticated provider responses

Safe diagnostics include the Usage version, Windows version, provider status enum, whether the official CLI is detected, sanitized log messages, and timestamps with account identifiers removed.
