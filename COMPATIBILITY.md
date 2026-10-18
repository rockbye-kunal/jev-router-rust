# Compatibility and independent policy

Reference checked: OpenRouter Jev Router documentation on 2026-10-18. This service is not affiliated with or distributed by OpenRouter or TypeSafe. The accepted model name is a local compatibility alias, not a claim that this binary is their private implementation.

| Behavior | Rust implementation |
|---|---|
| One OpenRouter key for Jev and inference | Implemented; per-caller BYOK for public use |
| Chat Completions, Responses, Messages endpoints | Implemented as original-protocol forwarding |
| Streaming | Raw SSE forwarding with incremental accounting; no replay after start |
| Tools and structured output | Preserved; capability filtering; tools execute in caller applications |
| Multimodal input | Metadata capability filtering and original-content forwarding; no local media fetch/conversion |
| Low/medium/high cost tiers | Accepted and supplied to independent policy; not official selection weights |
| Include preferences and include fallback | Implemented before group comparison; one rebuild on definite endpoint 404 |
| Hard exclusions and operator allowlists | Enforced at every selection and rebuild |
| Exact slugs and dated revisions | Implemented; plain slugs do not implicitly admit variants |
| Wildcard matching | Case-sensitive glob matching |
| Published latest-family aliases | Resolved using available public catalog alias/canonical metadata; private or missing aliases are not invented |
| Effort hint and legacy field | Accepted; actual selected supported control reported |
| Unknown plugin values and unsupported reasoning settings | Rejected |
| Session adaptation | Caller supplies session_id; prior model/effort available to Jev; tool/stateful turns remain pinned |
| Expert advisor | One optional operator-configured consultation; off by default |
| Independent answer checks | Exact checks for a narrow unambiguous money-total grammar; otherwise optional fallible model review for bounded completed plain text (off by default); at most one constrained repair |
| Routing metadata | JSON pipeline plus response headers; streaming audit record fetched by request ID |
| Official market-spend ranking and exact policy thresholds | Not reproduced: insufficient public implementation/data |
| list_tier_cap and official capability-tier scores | Not implemented; no invented quality score is returned |
| Official author router aliases (`anthropic/auto`, etc.) | Not exposed; operator/caller patterns provide author filtering |
| EU/regional guarantees | Not provided; unsupported regional routing fields rejected |
| SaaS registration, subscriptions and platform billing | Not implemented; users pay OpenRouter directly |
| Horizontal multi-node quota/session state | Not implemented; single-node SQLite with exclusive process lock |

The model comparison uses provider descriptions and cost estimates with Jev in a tournament. It does not claim a globally optimal model, identical outcomes to the hosted router, or measured general-task accuracy. Grouping/order and lossy task summaries can change selection. Tournament winners are judged again; probabilities from unrelated groups are not ranked against each other. A representative held-out evaluation is required to calibrate this policy for a specific audience.

Jev receives a bounded recent-text summary with modality/tool signals. The generator receives the complete original request. Thus a million-token-capable generator does not mean the classifier reads the entire million-token conversation. Media payloads are not interpreted locally by the classifier. Context admission uses conservative byte counts; very long or large media requests may fail admission even when an underlying model might accept them with a more exact tokenizer.

A listed model may be inaccessible to a particular upstream key. Key verification does not prove access to every model. The router tries at most one definite-404 rebuild and never ignores hard exclusions. Uncertain decisions without an eligible configured fallback fail explicitly rather than silently broadening access.

Native protocol/version/beta features still depend on OpenRouter and the selected model. Test the precise SDK/features used by your client before rollout. Protocol passthrough is not a blanket claim of compatibility with every future upstream field.
