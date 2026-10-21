# Empty and truncated output recovery

The live production-planning example exposed a completed provider response with no visible output: Claude Sonnet 4.6 consumed all 3,500 completion tokens as reasoning and ended with `length`. Increasing the limit to 10,000 or reducing effort alone did not reliably solve that condition.

The Rust service now:

- Detects reasoning-only output in Chat, Responses and Messages, plus non-stream token-limit truncation.
- Makes at most one recovery generation, sharing the two-generation ceiling with answer repair.
- Prefers the operator-configured `output_recovery_model` (default `openai/gpt-5.4`), then the eligible fallback/repair models. A configured model can be repeated only at a strictly lower effort.
- Disables hidden reasoning for recovery when the model explicitly advertises `none`; otherwise uses a supported reduced effort. Caller token limits are preserved.
- Preserves the candidate pool, hard exclusions, provider/privacy controls, tenant identity and budget. Bound sessions/tool results cannot hop models. Refusals are returned unchanged.
- Reserves both generations before paid work even when answer verification is disabled, sums known costs, and retains conservative settlement if a cost is unavailable.
- Persists sanitized model/effort/finish-reason/token diagnostics before paid work and on cancellation. Empty/incomplete errors include request identifiers for tenant-scoped audit lookup. No keys, hidden reasoning or answer text are retained in those diagnostics.
- Keeps ambiguous network failures, rate limits and already-forwarded streams out of automatic replay. The default paid-work deadline is now 240 seconds; Compose permits 300 seconds for graceful draining.

The recovery availability fix passed the latest live complex request: HTTP 200, a completed JSON answer, and automatic routing followed by a recovery model. **Its answer correctness did not pass.** Independent exhaustive enumeration of 1,830 feasible plans proves the unique optimum is A=6, B=18, C=6, profit 2,790, with resource use (78,90,60). The model returned a plan violating the minimum C constraint. Other attempts returned feasible but suboptimal plans, incorrect arithmetic/proofs, or malformed requested JSON.

See `COMPLEX-RECOVERY-RESULTS.json`: `http_success` and `completed` are true, but `constraints` and `globally_optimal` are false. This failed quality result is intentionally retained, not counted as a pass. HTTP success and routing confidence are not evidence of correctness.

The exact optimizer was an independent test oracle, not a production router capability. Exact production optimization needs a deterministic solver exposed through a validated application tool or a typed optimization endpoint. This router forwards client tools and preserves their sessions; it does not execute arbitrary client code or implement a general optimizer. Universal task correctness, universal availability and production readiness for arbitrary tasks are not claimed.

Current automated evidence: 94 tests (93 system tests and one release-executable scenario), formatting, Clippy and a locked release build passed. All nine current live protocol acceptance cases passed on the updated local service; see AUTO-ROUTING-RESULTS.json. They cover three non-stream protocols, three SSE protocols, tool arguments, strict JSON and an exact money total. They do not establish arbitrary-task correctness. The older broad acceptance record does not override this failed optimization-quality result.

Provider reference: [OpenRouter reasoning-token budgets](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).
