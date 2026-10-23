# 0001. KatWork owns the agent loop; Rig is used only for model I/O

- Status: Accepted (2026-10-22)
- Spec: §5.1 (Executor), §5.2 (vendor picks), §6.5, §6.6

## Context

A Dot's agent loop alternates model calls and tool calls. Rig offers more than model clients: an agent runner, a tool server, hooks, typed-output retries and provider-hosted tools. Those parts would decide when a tool runs, how often a call is retried, what the model sees as a tool result and when a run ends.

KatWork's trust rules sit exactly there:

- a tool is allowed only if it is in the Dot's allowlist *and* the registry, checked at call time;
- destructive and outward tools stop before dispatch and become a Proposal;
- every tool call is recorded in a ledger before it runs, and a mutating call is never repeated after a crash;
- calls race a per-call timeout, the run's remaining time and a cancellation token;
- steps, tokens and wall-clock time are counted per attempt, retries included;
- data residency is checked against the Space before every model attempt;
- tool output never enters the system role.

None of this can be guaranteed if a third-party runner owns the loop.

## Decision

- KatWork's own guarded loop in `katwork::agent` is the executor. It returns facts; the policy function decides the job status.
- Rig is used only for its completion and streaming clients, wrapped behind KatWork's `ModelProvider` trait in one file (`models/rig_adapter.rs`). No Rig type leaves that file.
- Rig's agent runner, tool server, hooks and typed-output retries are not used. Tools are advertised to the model as definitions only.
- Provider-hosted tools, server-side fallbacks, raw extra request parameters and hidden continuation are turned off.
- Capabilities and residency come from KatWork's model registry, never from the provider name or base URL. The OpenAI Chat or Responses API is chosen explicitly per registry entry.
- The HTTP client has connect and read deadlines, caps on response, error-body and stream-frame size, and no redirects.
- The default log filter keeps Rig at `info`, because its trace level logs full prompts.

## Consequences

- KatWork maintains the loop code and its tests (turn validation, repeat detection, truncation, ledger, cancellation).
- Rig upgrades touch one adapter file. Offline fixture tests on recorded provider bytes catch behaviour changes before they reach the loop.
- Swapping Rig for another client, or adding a provider, changes the adapter and the registry, not the loop.
