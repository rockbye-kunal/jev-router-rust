# Verification record

Latest automatic-routing and executable acceptance results: [PRODUCTION-ACCEPTANCE.md](PRODUCTION-ACCEPTANCE.md). The earlier six-case smoke results below are historical; the latest unrestricted nine-case suite is AUTO-ROUTING-RESULTS.json.

Verified locally on 2026-10-18 on Apple Silicon macOS, using Rust 1.99.1. This is an independent implementation, not a certification or reproduction of OpenRouter's private selection policy.

## Build and automated checks

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --locked`: **81 passed, zero failed (80 system tests plus one executable integration scenario)**. See TEST-RESULTS.txt.
- `cargo build --release --locked`: passed.
- Production configuration validation and running release readiness check: passed.
- Cargo audit: zero known RustSec vulnerabilities and no advisory warnings for the locked dependency graph. See DEPENDENCY-AUDIT.json. Used the official RustSec advisory database at commit `999ec89d5d3f10f20be14750ec8c2d8b44e0ac9b` with `--no-fetch`; the crates.io index was unavailable, so this does not establish a complete yanked-package check.

Tests cover actual HTTP requests against local mock provider servers: routing and supported effort selection, hard exclusions, soft include fallback, dynamic model groups, capabilities, context/output bounds, tools, all three protocol adapters, fragmented SSE, model mismatch, total stream deadlines, cancellation, privacy, advisor bounds, tenant isolation, auth sanitization, public BYOK, CORS, atomic budgets, recovery, rate limits, outages and body limits.

A fixture-only load run completed **100/100 requests with 16 concurrent callers**, in 282 ms with approximately 67 ms p95 latency, and no remaining active reservations. See LOAD-TEST.txt. These measurements exclude real inference and network latency and are not production throughput figures.

## Live OpenRouter verification

The final release executable used the existing authorized local OpenRouter key without printing or copying it. One earlier finalization attempt encountered an upstream overload; another caught a false rejection by experimental model review. The latter is now opt-in and off by default. The final six-case smoke suite passed. Jev was served as `typesafe/jev-1.13-20260917` through OpenRouter's Decisions endpoint. Six checks passed; see LIVE-RESULTS.json:

1. Chat Completions: PONG from Gemini 3 Flash, effort none.
2. Responses: PONG from Gemini 3 Flash, effort none.
3. Messages: PONG from Claude Sonnet 4.6, effort none.
4. Tool calling: a required `get_weather` function call was returned from Sonnet; no tool was executed by the router. Effort uncertainty is now handled with a bounded two-choice runoff rather than jumping to maximum effort.
5. Structured output: Gemini returned schema-conforming JSON with `ok: true`.
6. Streaming Chat Completions: PONG, completion marker and reported cost verified from actual SSE.

## Regression fix: uncertainty and incorrect arithmetic

The original unrestricted example asked for 3 notebooks at ₹80 each plus 2 pens at ₹15 each. The old policy escalated directly to Sonnet because of a low-confidence many-way model choice. A focused adequacy review removed that automatic escalation, but live runs then exposed incorrect totals from the cheap winner. Both Jev and a generation-model review accepted incorrect arithmetic in testing. Neither is a reliable substitute for a task-specific validator.

The implemented exact check covers unambiguous single-message quantity × unit-price totals using integer cents, checked arithmetic and complete numeric-span coverage. It declines unsupported adjustments, mixed currencies, decimal quantities, extra numeric inputs, overflow and ambiguous formats. Other tasks are explicitly marked as lacking a deterministic validator. Fallible model review is optional and off by default: live protocol testing also exposed a false rejection of a valid short reply. This prevents that experimental check from breaking unrelated protocol requests.

The final unrestricted live regression **passed**: a wrong first answer was rejected, the one allowed repair used Gemini 3 Flash with medium effort, and the returned answer was `(3 × ₹80) + (2 × ₹15) = ₹240 + ₹30`, total **₹270.00**. Metadata reports `answer_repaired`, `deterministic_money_total` and two attempts. See ARITHMETIC-REGRESSION.json and ARITHMETIC-AUDIT.json for final output and accounting. The current normalized registry had 346 models before full-reservation budget filtering; the request used no include list. The candidate metadata shows final comparison groups, not every eligible entry.

New regressions cover focused review, justified escalation, inconclusive low-tier choices, malformed reviews, single-model pools, effort runoff, repair limits, excluded repairs, reviewer/model identity, missing cost estimates, failed-session binding, expensive catalog outliers, exact decimal money, overflow, contradictory totals and ambiguous arithmetic inputs. The arithmetic repair test deliberately makes the model reviewer accept everything; the exact validator still rejects a wrong total.

No claim is made that these changes fix every possible task or improve all costs. Review and repair add billed work. The unrestricted live example costs more in routing overhead than a direct small-model call; evaluate selection quality and economics before enabling a wide catalog for clients.

The release server was left running locally at `http://127.0.0.1:8788`. This process is a development convenience, not a persistent deployed service. It uses loopback-only automatic credentials; public deployment requires each caller's own OpenRouter key.

## Deployment limits and remaining acceptance work

- Docker and Compose files are supplied, but Docker was unavailable locally: image execution, Linux container operation and live TLS have **not** been verified. No public domain was deployed.
- The persistence and quota design supports one node. Distributed storage/admission, SaaS accounts and customer invoicing are not implemented.
- General-task routing quality, prompt-injection resistance, real-provider load, cost savings, long conversations and multimodal accuracy have not been benchmarked. Multimodal forwarding is covered by fixtures; no live image/audio/video suite was run.
- Live tests cover selected models and simple tasks, not every provider, future protocol field, Messages/Responses streaming variation or production SDK.
- Local cost reservations are estimates, not hard upstream billing guarantees. Configure OpenRouter key/account spending controls.
- Before a client rollout, execute the deployment acceptance steps in DEPLOYMENT.md, evaluate representative tasks against baselines, tune uncertainty/fallback policy, and verify backup restoration on the target host.

No credentials, prompts from real customers or customer answers are included in the source archive. The live report contains only synthetic test metadata and provider usage.
