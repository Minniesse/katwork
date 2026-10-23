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

The AG-UI 1.0 schema and its reference client treat a stream as one run at a time: nothing may follow `RUN_FINISHED` or `RUN_ERROR` except a new `RUN_STARTED`, a second `RUN_STARTED` while a run is active is fatal, and a repeated `TEXT_MESSAGE_START` continues the closed message. A KatWork thread stream differs on purpose. Runs of different Dots overlap in one thread. `TEXT_MESSAGE_START` restarts a message's provisional text, because each model turn of a run streams under the run's single reply id. Intake writes `katwork.*` events outside any run. Cancel is `RUN_ERROR` with code `cancelled`, not `RUN_FINISHED` with a `cancelled` outcome, which keeps the cancel paths of the loop, the scheduler and the pipeline on one terminal event.

### Decision

- `katwork.run_closed` is appended immediately **before** each run's `RUN_FINISHED` or `RUN_ERROR`, in the same transaction, so every run's terminal event is its last event. It carries the error `code`, because `RUN_ERROR` has no run id.
- Every event object KatWork emits still validates against the AG-UI 1.0 JSON Schema, vendored under its MIT licence as a test fixture. That schema test replaces hand-written protocol fixtures. `RUN_STARTED` declares `protocolVersion: "1.0"`, and `RUN_ERROR.message` holds the stable code, never provider text.
- The thread stream as a whole is not a conformant AG-UI run stream, for the reasons above. Stock AG-UI clients and their reducers are not supported on it; KatWork's own web client folds it with the rules in the SP1 plan (Task 9).
- The stream's control frames, `ping`, `resync_required` and `stream_error`, are named SSE events with no `id`, outside the AG-UI event set and outside the log.
- The web client shows code `cancelled` as a neutral stop, never as an error.

### Consequences

- Pointing a stock AG-UI client at a KatWork thread stream needs an adapter that splits it per run and maps `TEXT_MESSAGE_START` and cancel; none is planned.
- A later move to `RUN_FINISHED` outcomes for cancel is a deliberate change to the loop, the scheduler, the pipeline and the web reducer, with this amendment updated.
