# Model wire fixtures

These sanitized, synthetic wire recordings exercise the real Rig clients through a local TCP HTTP server. They contain no API keys or customer data. Test credentials are generated at runtime. No fixture test contacts a live provider.

The three `oversized_*` fixtures use small injected limits so their committed bytes stay small. They cover the whole response, capped HTTP error bodies, and unterminated SSE frames. HTTP chunk boundaries are chosen by the test, including a split inside a Thai code point.

`responses_final.sse` deliberately changes finalized text in the terminal response. KatWork re-decodes Rig's final response document, without another HTTP call. Rig also repairs duplicate call IDs; KatWork checks the preserved wire IDs and refuses that turn instead. Tool events remain withheld until the entire turn passes validation.

`anthropic_replay.sse` includes a synthetic thinking signature. It must survive replay to Anthropic and disappear when history is sent to OpenAI. Callers must keep replay state in the same workspace and thread; the adapter checks provider identity, format version, and canonical message content.

`chat_arguments.sse` carries only tool-argument fragments. The adapter unit tests feed each SSE frame through Rig at 20-second intervals under Tokio's paused clock, asserting one request, one usage record, and a completed turn. The mock replaces only HTTP delivery so socket scheduling cannot advance the virtual clock prematurely. Other timed fixtures cover reasoning, usage, and unknown events for all adapters.

`chat_no_finish.sse` covers missing finish reasons; the duplicate `chat_unfinished.sse` was removed. `chat_truncated.sse` covers unfinished arguments on a length stop without exposing an executable call, and `chat_malformed.sse` covers malformed arguments on an accepted stop.

The ignored live smoke test additionally requires `KATWORK_LIVE_MODELS=1` and `OPENAI_API_KEY`. Do not enable it in CI.

Registry defaults were checked on 2026-10-23. OpenAI uses [GPT-4.1 nano](https://developers.openai.com/api/docs/models/gpt-4.1-nano), [mini](https://developers.openai.com/api/docs/models/gpt-4.1-mini), and [GPT-4.1](https://developers.openai.com/api/docs/models/gpt-4.1), which support Chat Completions with function calling without extra reasoning options. Anthropic uses Haiku 4.5, Sonnet 4.6, and Opus 4.6, all [active models](https://platform.claude.com/docs/en/about-claude/model-deprecations). These are initial defaults, not automatic model upgrades; prices remain unset.
