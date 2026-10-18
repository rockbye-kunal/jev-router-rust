# Single-node public deployment

This deployment is for a BYOK API: each caller sends an OpenRouter inference key, and all inference is billed directly to that key's account. Do not put your personal key in a public web application. The Rust service processes supplied credentials in memory and never persists them.

## Before rollout

1. Run the documented Rust tests, formatting/lint checks and dependency audit. Build and test the Docker image on the target OS; local verification status is in VERIFICATION.md.
2. Review `config.production.json`: output/body limits, per-key quotas, estimated budgets, Jev confidence settings, model restrictions, fallback, data policy and provider availability.
   Review `repair_model`, `verify_text_answers` and `model_answer_review` too. Exact supported checks are enabled by default; fallible model review is off by default. If enabled, plain-text review can call the designated reviewer outside soft generation preferences, but never outside hard restrictions or admission limits. Ensure a structured-output-capable reviewer remains available under your hard policy. Unsupported/ambiguous arithmetic, structured output, tools, long output and streams do not receive a general correctness guarantee.
3. Evaluate general-task quality and cost against a fixed-model baseline and, if desired, the hosted official router. These smoke tests are not quality benchmarks.
4. Use a dedicated host/data volume and your domain. Install Docker/Compose and point DNS to the host. Publish only the Caddy TLS edge, not the Rust port.

## Start

```sh
export DOMAIN=router.example.com
docker compose up --build -d
```

The included Compose file intentionally does **not** inject an operator OpenRouter key into the public server. Requests without caller credentials receive 401. `Authorization: Bearer <caller OpenRouter key>` or the native Messages `x-api-key` is the credential; no extra router key is required.

Caddy terminates HTTPS, bounds header/body read times, and forwards streaming without buffering. Public CORS permits explicit caller OpenRouter keys without ambient cookies. The local automatic-key mode keeps CORS disabled. The service runs as UID 10001 with a read-only container filesystem; only `/app/data` and a bounded temporary mount are writable. Quotas, request records and sessions persist in the named `router_data` volume. Keep this volume private and back it up consistently; do not copy a live SQLite database file without its WAL or SQLite's backup procedure.

The same upstream account/key can carry additional OpenRouter account/workspace guardrails. Configure them for hard billing limits. Local admission budgets are conservative per-key controls, not invoices or a guarantee against every upstream/multimodal charge.

## Verify on the deployed host

```sh
docker compose exec router jev-router health
curl -fsS https://router.example.com/healthz
curl -fsS https://router.example.com/readyz
```

Then test authenticated generation, streaming, tools, exclusions, missing/invalid keys, and two separate caller keys. Confirm each key can see only its own `/v1/usage` and `/v1/requests/{id}` results. Do not publish actual keys in commands, screenshots, logs or issue reports.

## Operations

- **Single writer:** one router process per database. A lock prevents concurrent ownership. Do not increase replicas against this SQLite volume. Multi-node operation needs a distributed transactional backend and admission control.
- **Limits:** default 32 global in-flight generations/previews, four per-key requests, 60 admissions per key/minute, eight concurrent uncached key verifications, 16 MiB JSON body, 240-second paid-work deadline and a one-hour catalog freshness window. Put audience-appropriate IP/bot/WAF controls at the edge, especially against invalid-key floods.
- **Privacy:** prompts, answers and provider keys are not written by this application. Routing metadata, per-key fingerprints, session IDs, costs and statuses are stored. Audit retention is 30 days; session admission TTL defaults to one day. If clients use sensitive session IDs, use opaque identifiers. Data policies must be checked for both Jev and inference endpoints.
- **Catalog outage:** the last complete snapshot is used only while fresh. Refresh writes are atomic. A stale snapshot blocks readiness and paid work.
- **Request interruption:** unknown billing remains a conservative reservation estimate; active slots are released, and crash recovery finalizes abandoned records once. Reconcile upstream billing separately when exact amounts matter.
- **Repair and budget:** admission includes the bounded review/repair path before paid work. Catalog entries too expensive for the complete reservation are removed before selection, so an expensive outlier cannot block affordable models. A rejected completed text answer can trigger one known additional generation; ambiguous network failures are still not retried. Audit accounting includes both generations and any reviewer calls.
- **Provider circuit:** failures are isolated by caller-key fingerprint and operation. A failing key does not open another key's circuit. Generation is not retried on ambiguous network failure.
- **Shutdown:** SIGTERM stops acceptance and drains active requests/streams. Compose allows 300 seconds, exceeding the request deadline. Interrupted processes are recovered on the next exclusive-owner startup.
- **Rollback:** retain the prior binary/config and data backup. Current schema migrations only create initial tables. Future schema changes require migration/rollback planning before deployment.

No public domain has been deployed by this task. TLS, host/container operation, backup restoration, regional requirements and expected real-provider throughput need verification on your actual deployment environment.
