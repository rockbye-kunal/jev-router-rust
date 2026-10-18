# Jev Router — Rust

An independent Rust implementation of Jev-powered model and reasoning-effort routing through OpenRouter. The API, CLI, routing engine, persistence, streaming and tests are Rust. There is no Node.js or Python runtime dependency.

This is not TypeSafe's or OpenRouter's official router source. It implements documented controls with an inspectable independent policy. It does not reproduce undisclosed market-spend rankings, private model quality scores or their precise selection decisions. See [COMPATIBILITY.md](COMPATIBILITY.md), [PRODUCTION-ACCEPTANCE.md](PRODUCTION-ACCEPTANCE.md) and [VERIFICATION.md](VERIFICATION.md).

## Run

Requires a current stable Rust toolchain. This checkout was built using Rust 1.99.1. `Cargo.lock` is included.

```sh
cargo build --release --locked
cp .env.example .env
# Edit .env locally with OPENROUTER_API_KEY; never paste it into chat.
./target/release/jev-router serve
```

Local address: `http://127.0.0.1:8788`. The existing TypeScript service, if running, remains on port 8787. To reuse an existing local env file without copying its contents:

```sh
./target/release/jev-router --env-file ../jev-router/.env serve
```

Commands:

```sh
jev-router catalog                       # Refresh public model capabilities
jev-router check-config                  # Validate defaults
jev-router --config config.production.json check-config
jev-router doctor                        # Configuration status; never prints keys
jev-router smoke                         # Eight live checks with model preferences; billed
jev-router acceptance                    # Nine live checks with NO model/effort preferences; billed
```

The smoke command reads `OPENROUTER_API_KEY` from `.env` or the specified `--env-file`, calls the running local router, and saves sanitized results to `LIVE-RESULTS.json`.

`acceptance --base-url https://YOUR_DOMAIN` checks automatic routing without caller model or effort preferences, including all three protocols with and without streaming, tool arguments, strict JSON output and an exactly checked arithmetic case. It spaces cases by five seconds by default (`--delay-ms` changes test pacing), writes `AUTO-ROUTING-RESULTS.json`, and records failed/partial runs rather than leaving a stale passing report. Pacing this acceptance suite does not establish production throughput or remove upstream rate limits.

The toolchain installed for this workspace lives outside this project, under `../../work/rust-toolchain`. It is not part of the production runtime. A prebuilt local executable is under `target/release/jev-router` after the release build.

## Public callers: one OpenRouter key

A public deployment requires every caller to supply **their own OpenRouter inference key**. The same key is used for Jev and generation; callers pay OpenRouter directly. No separate TypeSafe key, router key, registration token or operator-funded public endpoint is required.

```sh
curl https://YOUR_DOMAIN/v1/chat/completions \
  -H "Authorization: Bearer $OPENROUTER_API_KEY" \
  -H 'Content-Type: application/json' \
  -H 'X-OpenRouter-Metadata: enabled' \
  -d '{
    "model": "typesafe/jev-router",
    "plugins": [{"id":"jev-router","cost_tier":"medium"}],
    "messages": [{"role":"user","content":"Explain a mutex versus a semaphore."}],
    "max_tokens": 4096,
    "stream": false
  }'
```

A configured server key can be used without an Authorization header **only** when both the bind address and actual TCP peer are loopback. A public bind such as `0.0.0.0` never falls back to that key. Origin checks prevent cross-origin browser use of automatic localhost credentials. The native Messages `x-api-key` header accepts the same OpenRouter key. Public mode supports browser CORS with explicit caller credentials and no cookies; localhost automatic-key mode does not enable cross-origin access.

Caller keys are verified through OpenRouter's current-key endpoint, retained only for request execution (except an explicitly configured local server key), and never persisted in the database, responses or logs. Verification is cached for 30 seconds by a SHA-256 fingerprint; outbound calls still enforce upstream revocation. The fingerprint namespaces sessions, limits and audit records. These are **per-key tenants**, not identities of human users: rotating to a different upstream key creates a new tenant namespace. Use OpenRouter account/workspace guardrails for person/account-level budgets.

The operator of the deployed server processes caller keys in memory and conversation content in transit. Deploy only under a trusted domain with TLS and appropriate access to the host. The service does not charge customer invoices or manage subscription accounts.

## API

| Endpoint | Purpose |
|---|---|
| `POST /v1/chat/completions` | Chat Completions, including tool calls and SSE |
| `POST /v1/responses` | Responses wire format |
| `POST /v1/messages` | Messages wire format |
| `POST /v1/route` | Jev selection preview; no generation, but Jev is billed |
| `GET /v1/models` | Router model identity and registry summary |
| `GET /v1/usage` | Only the caller key's quota counters |
| `GET /v1/requests/{id}` | Only the caller key's decision/billing record |
| `GET /healthz` | Process liveness |
| `GET /readyz` | Catalog readiness; no paid provider probe |

Accepts `model: "typesafe/jev-router"`, `"jev-router"` or `"auto"`. Original message/content/tool/input objects are forwarded to the selected generator in the same protocol; the router does not execute client tools. Remote media is forwarded, never fetched by the Rust host itself. Images/files/audio/video require matching advertised input capabilities and must fit conservative context/cost admission bounds. Large base64 content can be rejected because input admission deliberately overestimates tokens rather than silently truncating the generator conversation.

Only routing fields are interpreted locally. Unsupported protocol fields can still be rejected by OpenRouter. Wire forwarding is not a claim that every model supports every upstream parameter. Provider parameter checks are enabled.

Plugin parameters: `id`, `cost_tier` (`low`, `medium`, `high`), `models`, `allowed_models`, `excluded_models`. Unknown plugin fields return 400. Include lists are soft preferences, combined before model comparison; they are ignored only when no eligible included model remains. Exclusions and operator `hard_allowed_models`/`hard_excluded_models` are always enforced. A definite endpoint 404 triggers at most one pool rebuild/retry; network timeouts, 5xx responses and interrupted streams are not retried as generation may already be billed. A generation HTTP 429 is a definite pre-output rejection: OpenRouter provider fallback is enabled by default (`allow_provider_fallbacks`; a caller may send `provider.allow_fallbacks: false`), and the router then fails over at most `max_generation_failovers` times (default 2). It prefers the configured recovery, repair and fallback models, otherwise the eligible model nearest in estimated cost at or above the rejected one, and keeps the selected reasoning effort (or the nearest supported level). The rejected model cools down for that caller key only, for the provider `Retry-After` or the circuit cooldown, capped at 15 minutes. Account/key-level limits (`limit_source: openrouter_*`) and bound tool or session turns never fail over. Rejections are recorded in the request audit, and a final 429 returns `Retry-After` and `rate_limit_scope`. The retry still respects all hard constraints. Cost tiers are selection preferences, not effort levels or guaranteed spending ceilings. Completed non-stream responses with empty visible output or token-limit truncation receive at most one recovery on an eligible configured fallback/repair model. The dedicated `output_recovery_model` (default `openai/gpt-5.4`) is preferred, then the configured fallback and repair models if eligible; the same model may be used again only at a strictly lower effort. Recovery disables hidden reasoning when the model explicitly supports `none`; otherwise it uses low effort for output limits up to 4,096 tokens, otherwise medium when supported, and never raises the caller token limit. It preserves the active candidate pool, provider restrictions and budget; bound sessions and tool-result turns cannot switch models. Refusals are returned unchanged. Both generations are reserved and accounted for, even when answer review is disabled; recovery and answer repair share a two-generation limit. If recovery cannot complete, the API returns a specific error rather than presenting truncated text as a completed answer. Streams already forwarded to callers cannot be regenerated.

Top-level `reasoning.effort` or legacy `reasoning_effort` is a hint. Selection uses the model's known effort list; null and omitted effort metadata are distinguished. Budget-control models use an output-relative reasoning budget. Models without controllable effort report `default` or `none` rather than inventing support. `reasoning.max_tokens` and pro mode are rejected at the router interface.

## Routing policy

1. Verify the caller key and load a fresh model registry.
2. Enforce input/output/context requirements, tools/JSON capability, operator restrictions, caller exclusions and request cost admission.
3. Apply soft include preferences. If none remain eligible, rebuild without includes.
4. Reserve quota atomically in SQLite before any paid call.
5. Ask Jev to compare models in bounded groups (default 48). Group winners are compared again; scores from unrelated groups are not treated as globally calibrated probabilities. The winner's decision path influences uncertainty checks.
6. Ask Jev for a supported effort on the chosen model. Model/effort are independent bounded decisions, with task context and cost preference shared between them. This differs from the earlier TypeScript prototype's fixed joint-pair list.
7. Low model confidence triggers a focused adequacy review against the eligible configured fallback. A close many-model comparison does not automatically escalate to a premium model. If the focused review is inconclusive, low-tier requests return an explicit error; other tiers use the eligible conservative fallback. A single eligible model does not require a confidence-based substitution. Uncertain effort gets a runoff between the top two choices, then the stronger of those two if still unresolved. Jev failure never becomes a silent cheapest-model decision.
8. In an existing tool loop or stateful Responses continuation, preserve the tenant's model binding. A now-ineligible binding returns 409. Other session turns may retain or switch based on Jev assessment.
9. Forward generation, preserve tool identifiers/stream ordering and record usage. No hidden tool execution or recursive router invocation.
10. By default, bounded, completed plain-text answers receive a separate check before being returned. Unambiguous single-message quantity × unit-price total requests use exact integer arithmetic in cents; unsupported adjustments, ambiguous inputs, overflow and unsupported formats do not receive that exact-validation claim. Other tasks are marked `no_deterministic_validator`. Fallible structured model review is optional via `model_answer_review: true` and is off by default because live tests exposed both missed errors and false rejections. A rejected answer can be regenerated once using the eligible `repair_model` (default Gemini 3 Flash), or the eligible fallback. The replacement is checked again; failure returns an error. Both generations and reviews are accounted for. Repair does not widen generation preferences or relax hard exclusions, privacy or session bindings. When optional review is enabled, review-model access is restricted by hard policy and budget; soft generation preferences do not restrict the separate reviewer. Configure `verify_text_answers: false` only if you intentionally want to omit these checks.

Model review is not proof of factual correctness. Live tests showed both Jev and a generation-model reviewer accepting incorrect arithmetic; exact supported money-total checks therefore override model judgments. The exact check uses the full single-message request. Other reviews see the bounded routing summary and at most 8,000 bytes of answer text. Streaming, structured output, tool/mixed-content output, partial output, multiple choices and longer answers are not semantically reviewed; metadata/audit records report the review status. Use domain-specific validators, tool execution results and representative evaluations where correctness matters. Review and repair increase latency and costs; they do not guarantee savings compared with one fixed model.

Use `session_id` or `X-Session-Id` to enable tenant-scoped continuity. Conflicting values are rejected. `previous_response_id` requires an existing session binding. Session records store model/effort and identifiers, not conversation bodies; expired bindings are not reused.

Tool-result turns also require an existing binding, including Responses `function_call_output`. Send the session identifier on the initial request before executing tools. The router rejects unbound or expired continuations rather than selecting an unrelated model. Local session and routing controls are removed from the forwarded generator body. Media capabilities are checked across the complete forwarded conversation, while only bounded recent text reaches Jev. Requested supported-parameter requirements and strict-schema capability are checked before selection.

An optional operator `advisor_model` enables at most one bounded expert consultation on high-tier requests where Jev judges it useful. That model must remain eligible under the caller/operator restrictions. Advice is inserted as untrusted advisory context, not tool authorization. Default is off. If an advisor is requested but unavailable/disabled, metadata reports `max_fallback: "deep"`.

## Observability and estimates

Set `X-OpenRouter-Metadata: enabled` for routing metadata on JSON responses. All successful responses include `x-request-id`, `x-jev-model`, `x-jev-effort` and `x-jev-reason`. Streaming bytes are forwarded without protocol conversion or injected events; fetch `/v1/requests/{id}` for the decision record and settlement status. SSE frames are inspected incrementally, with a 1 MiB frame buffer cap, to collect reported usage and detect model changes/errors.

The ledger distinguishes exact reported provider costs from conservative estimates. Input estimates use serialized request bytes plus overhead; reserve calculations include Jev and optional advice. Provider token-price ceilings are attached to generation requests. Multimodal fees, provider billing behavior and ambiguous failures mean local estimates are not contractual account spending caps. Configure upstream key/account guardrails for hard billing controls.

An interrupted request, missing usage cost or stream with no completion/cost marker retains a conservative reservation charge for admission accounting. This prevents an attacker from getting free quota by disconnecting. It is not a claim that OpenRouter billed that amount. Restart recovery converts abandoned reservations once and frees active slots. Audit records and old quotas are pruned after 30 days; old sessions after seven days.

## Deployment

Use [DEPLOYMENT.md](DEPLOYMENT.md). A non-root Docker image and TLS reverse-proxy Compose deployment are provided. Public mode works without an operator OpenRouter key. The public key limit, body cap, global semaphore, tenant rate/concurrency limits and durable reservations protect one deployment; edge-level abuse protection should be configured for the intended audience.

**SQLite is deliberately a single-node deployment.** A file lock prevents two router processes from owning the same database. Do not scale Compose replicas against this volume. Horizontal deployment needs a shared transactional store and distributed admission state; that is not implemented. The server closes gracefully on SIGTERM; the provided container grants 300 seconds for the 240-second request/stream deadline.

## Verification

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
JEV_ROUTER_E2E_BIN="$PWD/target/release/jev-router" cargo test --locked --test process_e2e
cargo audit
```

Tests use real local HTTP provider fixtures; they make no paid calls. The live smoke command uses the actual configured key and charges OpenRouter. See [VERIFICATION.md](VERIFICATION.md) for what was actually run. Integration success is not evidence that the independent policy matches the official router's quality or cost; evaluate representative tasks and calibrate thresholds before a client rollout.

The executable integration test launches a real public-mode server process with fixture upstreams. It exercises task-dependent model/effort choices, all three native streaming protocols, BYOK, CORS, tenant records, exclusive database ownership, forced termination, restart recovery and SIGTERM shutdown. It also verifies that failed smoke tests overwrite old passing reports. CI includes release-process tests and a container smoke job; CI/container execution still needs verification on the deployment host when unavailable locally.

Primary references: [Jev Router controls](https://openrouter.ai/docs/guides/routing/routers/jev-router), [Decisions API](https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request), [model capabilities](https://openrouter.ai/docs/guides/overview/models), [reasoning](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens), [current key](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key).
