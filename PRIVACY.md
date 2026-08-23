# Privacy

Codex Float follows a local-first design.

## Processed locally

The application may process the following data on the user's Windows device:

- allow-listed Codex quota fields;
- Token-count events from local Codex session records;
- daily aggregate Token totals;
- local window and application settings;
- compatibility and freshness states.

## Not collected

Codex Float does not provide default telemetry and must not collect or upload:

- prompts, responses or conversation content;
- email addresses;
- OAuth tokens, API keys or cookies;
- project file content;
- raw Codex app-server streams;
- pricing or billing estimates.

## Local retention

The MVP retains up to 30 days of daily aggregate Token totals. The application stores file hashes and scan cursors for idempotent local processing, not original session content. Users will be able to clear local statistics and cache data.

## Coverage

“Local Token usage” describes usage observable from supported local Codex records. It is not an OpenAI account invoice and may not cover web, mobile, another computer, delayed server-side usage, or unsupported Codex versions.
