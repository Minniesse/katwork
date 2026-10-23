# Coding standards

The reviewer applies every rule here to every PR (see the `katwork-review` skill). **Mechanical** rules have a command or a guard that decides them; run it rather than eyeballing. **Judgement** rules need a reading of the diff. A broken rule is a finding, with its severity.

The plan's **Global Constraints** (`docs/plans/2026-10-22-sp1-core-runtime.md`) are part of these standards. Where a rule below names one, the plan's text is authoritative.

## Mechanical

- **CI is green on the reviewed head.** `gh pr checks <n>`. CI runs fmt, clippy with `-D warnings`, the workspace tests, MSRV, `cargo deny check`, the Compose checks and, once `web/` exists, the web tests, contrast check and build. A check the author couldn't run locally is named in the PR body.
- **The PR targets the right base.** Stacked on the branch of the task it depends on, or `main`; its body says `Stacked on #<n>` when the base isn't `main`. Merges are merge commits. `.claude/hooks/guard.py` refuses squash, rebase and `--admin` merges, a merge without a review on the head commit, and deleting a branch that open PRs still use as their base.
- **Commits carry the right identity.** Author email is a GitHub noreply address, and every commit has a `Co-Authored-By:` trailer naming the model that wrote it: `git log --format='%ae %(trailers:key=Co-Authored-By,valueonly)' origin/<base>..<head>`.
- **Migrations are append-only.** A migration file that has reached `main` is released: it is never edited, renamed or deleted, and a schema change is a new numbered file in `crates/katwork/migrations/`. `git diff --name-status origin/main...<head> -- crates/katwork/migrations` lists only `A` lines.
- **No secret-shaped literals.** No API key, token, password or private key appears as a literal anywhere in the diff, tests and docs included. GitGuardian scans every PR, and a test value that looks real fails it just like a real one. Tests build synthetic secrets at runtime, for example from `Uuid::now_v7()` or by joining pieces that match no detector, as `crates/katwork/tests/support/mod.rs` does for password hashes.
- **The change is reviewed by the other model.** The commit trailers name the author. A Claude-written PR needs a Codex review; a Codex-written PR needs a Claude review and a separate review from a fresh Codex session.

## Judgement

### Tests

- **Tests come first, and they prove failure modes.** Every behaviour change arrives with tests that fail without it. Each feature has unit or integration tests for the ways it can fail, not only the happy path: bad input, missing auth, a cross-tenant id, a conflict or race, a timeout, a provider error, whichever the feature can meet. The task's Tests list and its Review Focus items are the floor.
- **Tests assert observed state, never elapsed time.** No assertion on wall-clock latency (`std::time::Instant` with an `elapsed()` bound): parallel runs make it flaky. Time-dependent logic runs under tokio's paused clock (`#[tokio::test(start_paused = true)]`, `tokio::time::advance`), where virtual-time assertions are exact. Wait for a condition by polling the state it produces (a row, a status, an event), with a deadline that only bounds a hang.
- **Fixtures hash passwords cheaply.** Test Argon2 uses minimal parameters, as `crates/katwork-server/tests/support/mod.rs` does with `argon2::Params::new(1024, 1, 1, None)`; production parameters stay in the production path.
- **Tests are independent under parallel runs.** Each test owns its data: its own `#[sqlx::test]` database, fresh ids, no fixed ports, no shared global state, no ordering between tests.

### Schema and concurrency

- **Tenant isolation lives in the schema.** Every tenant-owned child row references its parent with a composite foreign key `(parent_id, workspace_id) REFERENCES parent (id, workspace_id)`, and every tenant-owned parent has `UNIQUE (id, workspace_id)` for it. Every query on tenant data filters by workspace, and a request for another workspace's object gets `404`, never data.
- **Row locks follow the global lock order.** Check every new or changed transaction that locks rows against the plan's **Lock order** constraint: the order of row classes, ascending id within a class, every lock taken at the start with `FOR NO KEY UPDATE` (`FOR SHARE` when read-only), no `FOR UPDATE`, no upgrade from `FOR SHARE`, no deletes outside the rows that constraint allows. A path that can't follow it is a Blocker.

### Security

- **The trust core gets an explicit security review.** Approval, policy, evidence and sandbox code is the trust core. A PR that touches it says so, and its review probes each security claim (see the `katwork-review` skill).
- **Model output is never executed as shell**, in the core or in checks; checks run as argv arrays, and tool ids are matched against a registry.
- **Secrets come only from the environment**, and `Debug` output of configuration redacts them, as the plan's Secrets constraint describes.
- **The repo holds no secrets, API keys, real customer files or third-party source trees.** Third-party clones stay outside it.
