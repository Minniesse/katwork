# KatWork agent guide

- **Implementing a plan task** (or fixing review findings on one): use the `katwork-task` skill. The SP1 plan is `docs/plans/2026-10-22-sp1-core-runtime.md`; the design it implements is `docs/spec/katwork-design.md`.
- **Reviewing a PR**: use the `katwork-review` skill. The rules every review enforces are in `CODING_STANDARDS.md`.
- **Any other change**: work on a branch in a worktree of your own and open a PR; the other model reviews it and the maintainer merges.
- **Tests that touch Postgres**: `scripts/test-db.sh start` gives this worktree a private cluster and prints its `DATABASE_URL`; `stop` deletes it.
- **Tool trouble** (cargo not found, a hook or sandbox blocked a command, Codex setup): `docs/agents/environment.md`.
- **Issues and labels**: GitHub Issues via `gh`; see `docs/agents/issue-tracker.md` and `docs/agents/triage-labels.md`.
- **Domain language**: `GLOSSARY.md` and `docs/adr/`; see `docs/agents/domain.md`.
