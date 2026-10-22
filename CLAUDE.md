## Agent skills

### Issue tracker

GitHub Issues on `Minniesse/katwork` (use `gh`). See `docs/agents/issue-tracker.md`.

### Triage labels

Default five roles as GitHub labels. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` + `docs/adr/` at the root. See `docs/agents/domain.md`.

## How work happens here

- The design is `docs/spec/katwork-design.md`, and the implementation plans are in `docs/plans/`. Read the plan section your issue points to before you change code.
- Tests come first: write the failing test, make it pass, then refactor. CI must be green.
- Work on one issue per branch or worktree. Open a PR. The model that didn't write the PR reviews it, then the maintainer approves. Never push to `main` and never merge your own PR.
- Approval, policy, evidence and sandbox code is the trust core. Changes there need an explicit security review.
- Never commit secrets, API keys, real customer files or third-party source trees. Keep third-party clones outside this repo.
- Model output is never executed as shell. Tool ids are matched against a registry.
