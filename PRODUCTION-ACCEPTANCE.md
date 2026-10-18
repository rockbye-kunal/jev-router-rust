# Production integration acceptance

Local acceptance completed on 2026-10-18. This is an independent Rust routing service; passing these tests does not certify every possible task or deployment environment.

## Verified results

- **81 automated tests passed:** 80 policy/HTTP integration tests and one executable integration scenario containing multiple protocol, lifecycle and isolation assertions.
- **9 real OpenRouter acceptance cases passed in public BYOK mode**, with no caller model preferences or effort hints. The same OpenRouter key paid for Jev and generation; no third credential was used.
- `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, locked release build and the locked dependency RustSec audit passed.
- The release executable itself passed the subprocess scenario via `JEV_ROUTER_E2E_BIN`, not just the in-process Axum application. Its local Apple Silicon SHA-256 is `73f6eb5a20068b740386133f5738470c083ca8543e48f06980433129790d8c32`.

| Live case | Selected/served model | Effort | Result |
|---|---|---|---|
| Chat | `amazon/nova-micro-v1` | `none` | Passed |
| Responses | `amazon/nova-micro-v1` | `none` | Passed |
| Messages | `amazon/nova-micro-v1` | `none` | Passed |
| Tool call with Pune argument | `openai/gpt-5-nano` | `low` | Passed |
| Strict JSON schema | `mistralai/mistral-small-24b-instruct-2501` | `none` | Passed |
| Exact money total | `google/gemini-3-flash-preview` | `medium` | Passed |
| Chat SSE | `amazon/nova-micro-v1` | `none` | Passed |
| Responses SSE | `amazon/nova-micro-v1` | `none` | Passed |
| Messages SSE | `amazon/nova-micro-v1` | `none` | Passed |

The public-mode server was temporarily bound on port 8789. Missing caller credentials were rejected, and readiness reported public BYOK mode. The normalized live registry contained 347 models before task, capability, restriction and budget filtering. The suite uses five-second spacing between cases; this is not a production load benchmark. An earlier unpaced run encountered an upstream 429, and that failure is not counted as a pass. Rate-limit errors now distinguish routing from generation, and ambiguous generation is never automatically replayed.

Actual live data: [AUTO-ROUTING-RESULTS.json](AUTO-ROUTING-RESULTS.json). Executable checks: [RELEASE-E2E-RESULTS.txt](RELEASE-E2E-RESULTS.txt). All automated checks: [TEST-RESULTS.txt](TEST-RESULTS.txt).

## Executable scenario

The test starts a separate server process against local HTTP provider fixtures and checks automatic task-dependent model/effort selection, three native protocols and their SSE formats, public authentication even with an operator key configured, CORS, tenant audit isolation, durable records, exclusive database ownership, interrupted-stream crash recovery, recovery idempotency and SIGTERM shutdown. It also proves a failing smoke command replaces a stale passing report. Fixture keys are synthetic; this test makes no paid calls.

## Gaps fixed during this review

- Capability checks now scan media across the entire forwarded input, including older messages and earlier content parts.
- The bounded classifier summary retains recent instructions rather than losing the latest turn behind old text. Responses instructions are included.
- Responses function-call outputs and other tool-result turns require a live tenant-scoped session binding and retain model/effort. Unknown, expired or conflicting bindings fail before paid work.
- Strict JSON schemas require structured-output capability. Requested generation parameters, including forced tool choice, are filtered against advertised support.
- Invalid model/effort types, conflicting effort hints, conflicting token limits and invalid tool arrays fail before paid inference. Router session and token aliases are normalized before forwarding.
- Per-key concurrency includes requests from the previous UTC day; usage reports the cross-day active count while cost counters remain scoped to the current UTC day.
- Request-body admission is bounded before JSON parsing. Saturation keeps health endpoints reachable and browser error responses readable through CORS.
- Circuit state is isolated by tenant, endpoint and model. An unavailable generator cannot open a healthy generator's circuit.
- Jev versions, generator identities and revision suffixes are checked; missing/invalid selections do not become fallback generation.
- Live-check reports record failure and partial progress instead of leaving old passing results.

These extend the existing tests for restrictions, soft preferences, reservations, crashes, quotas, malformed decisions, privacy, cancellation, deadlines, endpoint fallback and exact arithmetic repair.

## Performance and deployment limits

The local fixture load test completed 100/100 requests with 16 concurrent callers in 282 ms, p95 67 ms, with no accounting errors. It excludes real provider/network inference and is not a production capacity claim.

The implementation is single-node SQLite with one owning process. Docker/Compose and Caddy TLS files are supplied. Docker/Podman were unavailable locally, so local container execution, real domain TLS, host firewall configuration, backups/restoration and target-host operation have not yet been verified. CI includes release-process integration and a container startup/authentication/shutdown job; its actual run status must be checked separately.

No held-out general-task routing benchmark, exhaustive provider/SDK test matrix, real-provider stress test or live media suite was completed. Exact arithmetic checks cover a deliberately narrow unambiguous quantity × unit-price grammar; unsupported tasks are not claimed as verified. Optional model review is off by default because live tests exposed both missed errors and false rejections. Routing confidence is not answer accuracy. Review, routing and repair add cost; this wide tournament policy is not claimed to minimize cost or match OpenRouter's private rankings.

Use [DEPLOYMENT.md](DEPLOYMENT.md) for deployment-host acceptance. A public domain has not been deployed by this work. The updated local development service is available only on loopback at port 8788.

Published contract reference: [OpenRouter Jev Router documentation](https://openrouter.ai/docs/guides/routing/routers/jev-router). The implementation pins Jev 1.13 and uses independently implemented policy; newer catalog releases do not silently change that pin.
