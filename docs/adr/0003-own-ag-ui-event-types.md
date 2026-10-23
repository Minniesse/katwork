# 0003. KatWork's own AG-UI event types, pinned to protocol 1.0

- Status: Accepted (2026-10-22)
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
