# Privacy

Usage has no backend and no account system of its own.

- Usage data stays on the local device except for the minimum provider request needed to retrieve quota information from that provider.
- No telemetry or analytics are collected.
- Usage does not read, upload, or inspect prompts, messages, chats, or conversation history.
- Usage does not sell or share user data.
- Provider authentication is used locally only where required to query that provider's quota service.
- Cached data contains normalized, non-secret quota percentages/timestamps only.

For Claude, an existing Claude Code OAuth access token is read into memory only for the quota request to Anthropic. It is not copied into Usage storage, displayed, logged, or sent to any other origin.

For Codex, Usage delegates authentication to the installed official Codex client through its local app-server interface and does not read raw Codex credentials.
