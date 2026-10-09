# Privacy

Codex Float follows a local-first design.

## Processed locally

The application may process the following data on the user's Windows device:

- allow-listed Codex quota fields;
- allow-listed account daily Token aggregates returned by the local Codex runtime;
- Token-count events from local Codex session records;
- daily aggregate Token totals;
- local window and application settings;
- compatibility and freshness states.

Codex Float reads the local Codex OAuth data to check account status and asks the local Codex runtime for quota and, when available, account daily Token aggregates. The account email is not included in the app's UI or IPC snapshot. OAuth credentials remain in Codex's own local storage; requests for account information go to the configured first-party Codex/ChatGPT endpoints.

## Not collected

Codex Float does not provide default telemetry and must not collect or upload:

- prompts, responses or conversation content;
- email addresses in Codex Float's local database, UI, or IPC payloads;
- OAuth tokens, API keys or cookies;
- project file content;
- raw Codex app-server streams;
- pricing or billing estimates.

## Local retention

Codex Float retains up to 30 days of daily aggregate Token totals and expires quota-cache entries after 15 minutes. It stores file identifiers and scan cursors for idempotent local processing, not original session content. Users can clear local Token statistics; this does not delete Codex's source session files or OAuth data.

## Coverage

“Local Token usage” describes usage observable from supported local Codex records. “Account Token usage” is a separate, allow-listed daily aggregate returned by the local Codex runtime when available. Neither is an OpenAI account invoice; either may be delayed or incomplete depending on Codex version and data availability.
