# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-10-22

### Added
- **Tiered routing pool.** `pool_tiers` (cheap, medium, hard) and `pool_per_tier` cap the candidates so model choice is a single Jev comparison instead of about ten. Each pattern resolves to its newest release by catalog `created` time. Hard picks must cost at least the priciest medium pick. Empty tiers are backfilled at the 10th, 50th or 85th cost percentile, skipping deep-research, search, image and `-pro` variants. Jev sees each candidate's tier.
- `max_model_age_days`: a hard rule that never uses models released longer ago.
- `jev-router pool` command to preview resolved tiers and stale patterns, which are also logged at startup and catalog refresh.
- `config.local.json` for loopback use with the same policy as production.
- Structured JSON logs on stderr: one access line per request, internal error causes, 5xx codes. `RUST_LOG` sets the level.
- `scripts/local-integration.sh` for offline checks and an opt-in billed end-to-end example.
- Release workflow that attaches Linux x86_64 and macOS arm64 binaries with SHA-256 checksums.

### Changed
- Shipped configs allow only closed models (OpenAI, Anthropic, Gemini, xAI) plus recent DeepSeek and Qwen. Amazon and older open-weight families are excluded.
- Fallback model is now `anthropic/claude-sonnet-5.5` and repair model `google/gemini-3.8-flash`.
- Caddy timeouts raised above the 240 s request deadline (read 260 s, write 290 s), with a 290 s drain grace period. Docker logs rotate.
- Documentation moved into `docs/`, with verification records in `docs/verification/`.

### Fixed
- Long non-stream generations were cut at the proxy after 135–170 s while still billed upstream.
- Endpoint-404 rebuilds now keep the tier cap.

### Security
- `rustls-webpki` 0.103.16 (RUSTSEC-2026-0382) and `webpki-roots` 1.0.10 (RUSTSEC-2026-0381).

## [0.1.0] - 2026-10-18

### Added
- Rust implementation of Jev-powered model and effort routing through OpenRouter, with Chat Completions, Responses and Messages protocols, SSE streaming, tools and structured output.
- Per-key BYOK tenants with SQLite quotas, request audit records and crash recovery.
- Rate-limit failover, per-key circuit breakers, empty and truncated output recovery, exact money-total validation, and session pinning.
- Docker image, Compose and Caddy TLS deployment, CI with tests, clippy, audit and container smoke checks.

[0.2.0]: https://github.com/rockbye-kunal/jev-router-rust/compare/ab4a20c...v0.2.0
[0.1.0]: https://github.com/rockbye-kunal/jev-router-rust/commit/ab4a20c
