# 0006. Rename Dots to Kats

- Status: Accepted (2026-10-23)
- Decision owner: Founder

## Decision

The founder chose **Kats** as the name for KatWork's AI coworkers in both English and Thai. Use **Kat** for one coworker. KatWork, its crate names and prefixes, and the Lead role keep their names.

Apply the rename throughout Rust identifiers, database objects and domain values, API routes, events, UI copy, translations, seed templates and documentation. This changes names only; permissions, validation and runtime behavior stay the same.

## Consequences

- Migration `0005_rename_dots_to_kats.sql` upgrades existing databases in place. Historical migrations keep their original SQL and checksums. Migration tests retain legacy identifiers to verify populated upgrades.
- The API uses `/kats`, `{kat_id}` and the renamed fields and reason codes. This is a breaking change with no compatibility aliases because nothing has been released.
- Stored freeform names, instructions and message bodies remain user content; the migration does not rewrite them.
