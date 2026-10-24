# KatWork agent guide

- **Implementing a plan task** (or fixing review findings on one): use the `katwork-task` skill, with the model the task's **Builder** line names (Opus for the trust core, Sonnet for the rest). The plans are `docs/plans/2026-10-22-sp1-core-runtime.md` (SP1) and `docs/plans/2026-10-24-sp2a-documents-evidence.md` (SP2a); the design they implement is `docs/spec/katwork-design.md`.
- **Reviewing a PR**: use the `katwork-review` skill. Every PR gets two independent reviews from fresh sessions that did not write it, a deep reviewer (Opus) and a checklist reviewer (Sonnet, with the checklist in `CODING_STANDARDS.md`, whose rules every review enforces), and a verifier who tries to refute each P0 and P1 finding. At most three fix rounds; a PR merges only when all three ran on its exact head, nothing blocking is open and CI is green.
- **Any other change**: work on a branch in a worktree of your own and open a PR; the two reviews above cover it and the maintainer merges.
- **Tests that touch Postgres**: `scripts/test-db.sh start` gives this worktree a private cluster and prints its `DATABASE_URL`; `stop` deletes it.
- **Tool trouble** (cargo not found, a hook or sandbox blocked a command): `docs/agents/environment.md`.
- **Issues and labels**: GitHub Issues via `gh`; see `docs/agents/issue-tracker.md` and `docs/agents/triage-labels.md`.
- **Domain language**: `GLOSSARY.md` and `docs/adr/`; see `docs/agents/domain.md`.
