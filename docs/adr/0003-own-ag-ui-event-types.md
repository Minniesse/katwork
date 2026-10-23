# 0003. KatWork's own AG-UI event types, pinned to protocol 1.0

- Status: Accepted (2026-10-22); amended 2026-10-23
- Spec: §5.2 (front end), §10

## Context

Every KatWork view reads one event stream, sent as AG-UI events over SSE. The available community Rust crate (0.1.0) wraps ids in UUIDs, rejects empty text deltas, has no fields for outcomes or usage, and does not track AG-UI protocol 1.0. KatWork's thread ids and per-thread sequence numbers must stay authoritative, because reconnecting clients resume from them.

## Decision

- KatWork defines its own serde types for the AG-UI events it emits, pinned to AG-UI protocol 1.0 and covering only that subset.
- KatWork-specific state travels in `CUSTOM` events named `katwork.*`.
- Fixture tests check each emitted event against the protocol's JSON.
- The community Rust crates are not used. The browser reads the stream with the native `EventSource`.

## Consequences

- A protocol upgrade is a deliberate change to KatWork's types and fixtures, never a transitive dependency bump.
- New event kinds need a type, a fixture and a test before they are emitted.

## Amendment (2026-10-23): the thread stream is KatWork's own, built from AG-UI shapes

### Context

AG-UI 1.0 treats a stream as one run at a time. Its lifecycle rules live in the specification text and its reference client, not in the JSON Schema, which checks each event object on its own and enforces no order. After `RUN_FINISHED` or `RUN_ERROR` they allow a new `RUN_STARTED`, and also `RUN_ERROR` after `RUN_FINISHED`, for a failure reported late; a second `RUN_STARTED` while a run is active is fatal, and a repeated `TEXT_MESSAGE_START` continues the closed message. A KatWork thread stream differs on purpose. Runs of different Kats overlap in one thread. `TEXT_MESSAGE_START` restarts a message's provisional text, because each model turn of a run streams under the run's single reply id. Intake writes `katwork.*` events outside any run. Cancel is `RUN_ERROR` with code `cancelled`, not `RUN_FINISHED` with a `cancelled` outcome, which keeps the cancel paths of the loop, the scheduler and the pipeline on one terminal event.

### Decision

- `katwork.run_closed` is appended immediately **before** each run's `RUN_FINISHED` or `RUN_ERROR`, in the same transaction, so every run's terminal event is its last event. KatWork keeps this stricter rule of exactly one terminal event per run and never sends the late `RUN_ERROR` the protocol allows. `run_closed` carries a `code`, because `RUN_ERROR` has no run id: the `RUN_ERROR` code, or, before `RUN_FINISHED`, the job's reason for any ending other than answered or ready.
- Every event object KatWork emits still validates against the AG-UI 1.0 JSON Schema, vendored under its MIT licence as a test fixture with the protocol's valid fixtures. `RUN_STARTED` declares `protocolVersion: "1.0"`, and `RUN_ERROR.message` holds the stable code, never provider text.
- KatWork's event types round-trip the protocol's valid fixtures for the 11 event types it emits, the `RUN_FINISHED` outcomes included (`success`, `interrupt`, `cancelled`), although KatWork itself never sends an outcome. The fixtures that use fields KatWork deliberately never sends are skipped, by name, with these reasons:
  - `result` (`RunFinishedEvent/valid/result-array.json`, `result-number.json`, `result-object.json`, `result-string.json`): the reply is the stored message
  - `usage` (`RunFinishedEvent/valid/usage.json`, `usage-cache-breakdown.json`; `RunErrorEvent/valid/usage-cache-breakdown.json`): usage is reported per model attempt in `katwork.usage`
  - `input` (`RunStartedEvent/valid/with-input.json`): runs start from intake, never from a client's run input
  - `subagentRunId` (`TextMessageContentEvent/valid/attributed-to-a-subagent.json`): no subagents in sub-project 1
  - `metadata` (`TextMessageContentEvent/valid/metadata-empty.json`, `metadata-reserved-key.json`, `metadata-value-null.json`; `TextMessageEndEvent/valid/metadata-prototype-named-key.json`), `rawEvent` (`TextMessageEndEvent/valid/raw-event-array.json`, `raw-event-number.json`, `raw-event-object.json`, `raw-event-string.json`) and `timestamp` (`TextMessageEndEvent/valid/timestamp-at-safe-integer-maximum.json`, `timestamp-at-safe-integer-minimum.json`): order and identity come from the per-thread sequence number
  - content parts (`ToolCallResultEvent/valid/file-source-part.json`, `parts-content.json`): tool results are text
- The thread stream as a whole is not a conformant AG-UI run stream, for the reasons above. Stock AG-UI clients and their reducers are not supported on it; KatWork's own web client folds it with the rules in the SP1 plan (Task 9).
- The stream's control frames, `ping`, `resync_required` and `stream_error`, are named SSE events with no `id`, outside the AG-UI event set and outside the log.
- The web client shows code `cancelled` as a neutral stop, never as an error.

### Consequences

- Pointing a stock AG-UI client at a KatWork thread stream needs an adapter that splits it per run and maps `TEXT_MESSAGE_START` and cancel; none is planned.
- A later move to `RUN_FINISHED` outcomes for cancel is a deliberate change to the loop, the scheduler, the pipeline and the web reducer, with this amendment updated.
- Sending any skipped field later removes its fixtures from the skip list in the same change.
