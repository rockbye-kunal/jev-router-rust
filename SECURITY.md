# Security policy

## Supported versions

Security fixes go into the latest release on `main`.

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's private vulnerability reporting ("Report a vulnerability" under the repository's Security tab), or contact the maintainers privately.

Include what you found, how to reproduce it, and the impact you expect. You should get an acknowledgement within a few days.

## Scope and design notes

- Caller OpenRouter keys are held in memory only for the request. They are never written to the database, responses or logs. Tenants are identified by a SHA-256 fingerprint.
- Prompts and answers are not stored. Request records hold routing metadata, costs and status for 30 days.
- A server key in `.env` is used only when both the bind address and the TCP peer are loopback. Public binds always require the caller's own key.
- Dependencies are checked with `cargo audit` on every CI run.
