# Coding standards

The reviewer applies every rule here to every PR (see the `katwork-review` skill). **Mechanical** rules have a command or a guard that decides them; run it rather than eyeballing. **Judgement** rules need a reading of the diff. A broken rule is a finding, with its severity.

The plans' **Global Constraints** (`docs/plans/2026-10-22-sp1-core-runtime.md`, and `docs/plans/2026-10-24-sp2a-documents-evidence.md` for SP2a work) are part of these standards. Where a rule below names one, the plan's text is authoritative.

## Mechanical

- **CI is green on the reviewed head.** `gh pr checks <n>`. CI runs fmt, clippy with `-D warnings`, the workspace tests, MSRV, `cargo deny check`, the Compose checks and, once `web/` exists, the web tests, contrast check and build. A check the author couldn't run locally is named in the PR body.
- **The PR targets the right base.** Stacked on the branch of the task it depends on, or `main`; its body says `Stacked on #<n>` when the base isn't `main`. Merges are merge commits. `.claude/hooks/guard.py` refuses squash, rebase, `--auto` and `--admin` merges, a merge no review covers, and deleting a branch that open PRs still use as their base.
- **Commits carry the right identity.** Author email is a GitHub noreply address, and every commit has a `Co-Authored-By:` trailer naming the model that wrote it: `git log --format='%ae %(trailers:key=Co-Authored-By,valueonly)' origin/<base>..<head>`.
- **Migrations are append-only.** A migration file that has reached `main` is released: it is never edited, renamed or deleted, and a schema change is a new numbered file in `crates/katwork/migrations/`. `git diff --name-status origin/main...<head> -- crates/katwork/migrations` lists only `A` lines.
- **No secret-shaped literals.** No API key, token, password or private key appears as a literal anywhere in the diff, tests and docs included. GitGuardian scans every PR, and a test value that looks real fails it just like a real one. Tests build synthetic secrets at runtime, for example from `Uuid::now_v7()` or by joining pieces that match no detector, as `crates/katwork/tests/support/mod.rs` does for password hashes.
- **Two independent reviews and a verification cover the exact head** (spec §19.1). Neither reviewer wrote the PR: the deep reviewer is a fresh Opus session, the checklist reviewer a fresh Sonnet session working through the **checklist** below, and the verifier tries to refute each P0 and P1 finding. At most three fix rounds; after the second, P3 findings become follow-up issues, while P0, P1 and security findings always block. Code PRs merge on the merge gate: the coordinator (the agent that dispatches the workers) merges only when all three ran on the head SHA, nothing blocking is open and CI is green, and reads the full diff of a trust-core PR first. The founder approves plans and answers "NEEDS FOUNDER DECISION" items; the founder does not approve code PRs one by one. The commit trailers name the author model, which for a plan task is the model its **Builder** line names.

### Guard scope

`guard.py` stops agents' accidents, not deliberate evasion; the server-side ruleset on `main` (PR required, no force-push or deletion, merge commits only, admins included) is the boundary. Evasion classes the guard accepts are listed in `docs/agents/environment.md` under **Guard scope**. Review a change to the guard for accidents an agent could plausibly make; a new deliberate-evasion trick isn't a finding.

## Checklist

The checklist reviewer (Sonnet) answers every item, in this order, as **pass** or a finding with its file and line. An item that does not apply is marked so, with the reason.

1. CI is green on the reviewed head (`gh pr checks <n>`).
2. The base, the commit identity and the trailers pass the Mechanical rules above; the author model matches the task's Builder line.
3. Every file the task's **Files** list names is created or modified, and the diff touches no file outside that list without a line in the PR body saying why.
4. Every name, signature and type in the task's **Interfaces** block exists in the diff exactly as written, and every value the task pins (limits, codes, copy, defaults) is the one the plan gives.
5. Every test the task names exists under that name and asserts what the plan states; every test asserts something and fails without its fix (the PR body says how each was seen failing).
6. Each command in the task's steps was run, and its result in the PR body matches the step's expected output.
7. Every request-path transaction goes through `db::guarded`, and every new SQL entry path has its named cancellation test.
8. Every new tenant-owned table has `UNIQUE (id, workspace_id)` and composite foreign keys, and a cross-workspace request has a test that gets `404`.
9. Every row lock follows the plan's **Lock order**, including the artifact lock contract.
10. Every new user-facing string has en and th copy, and every new error code is in the plan's list.
11. No secret-shaped literal, real customer data, third-party source tree, personal name, private path or internal id is in the diff.
12. The PR body lists every **Ambiguity ruling**. A Sonnet-built task that met a case its task does not decide stopped and asked instead of choosing; a choice made silently is a finding.
13. A trust-core change says so, and the deep review covers its security claims.
14. **Spec conformance**, and the review's "Spec deviations" section (the `katwork-review` skill) for a plan, an ADR or a spec-touching PR. "NEEDS FOUNDER DECISION" is only for a product, scope or process choice that the spec and the founder decisions do not settle. A defect that breaks a spec rule (a tenant leak, a lock-order break, a wrong limit) is a finding with a severity, never a founder question.
15. **For a plan:** a Sonnet builder could follow each non-trust-core task with no guesses (exact files, signatures, tests with assertions, commands with expected output, no open choices).

## Judgement

### Tests

- **Tests come first, and they prove failure modes.** Every behaviour change arrives with tests that fail without it. Each feature has unit or integration tests for the ways it can fail, not only the happy path: bad input, missing auth, a cross-tenant id, a conflict or race, a timeout, a provider error, whichever the feature can meet. The task's Tests list and its Review Focus items are the floor.
- **Tests assert observed state, never elapsed time.** No assertion on wall-clock latency (`std::time::Instant` with an `elapsed()` bound): parallel runs make it flaky. Time-dependent logic runs under tokio's paused clock (`#[tokio::test(start_paused = true)]`, `tokio::time::advance`), where virtual-time assertions are exact. Wait for a condition by polling the state it produces (a row, a status, an event), with a deadline that only bounds a hang.
- **Never pause time around live database I/O.** Cancellation tests use PostgreSQL barriers and state polling with real deadlines; a paused Tokio clock can advance while SQL is pending.
- **Fixtures hash passwords cheaply.** Test Argon2 uses minimal parameters, as `crates/katwork-server/tests/support/mod.rs` does with `argon2::Params::new(1024, 1, 1, None)`; production parameters stay in the production path.
- **Tests are independent under parallel runs.** Each test owns its data: its own `#[sqlx::test]` database, fresh ids, no fixed ports, no shared global state, no ordering between tests.

### Schema and concurrency

- **Request-path transactions and writes go through `db::guarded`.** Acquire the guard before `BEGIN` or a statement, retain it through operation completion, and set the injected `Config.lock_timeout` before row locks. A dropped future closes its backend; pool release rolls back completed operations, including ordinary errors, and reuses healthy connections. Cleanup failure discards the connection without changing the operation’s result. Raw pool acquisition requires a reviewed Clippy exception with a reason; worker exceptions must drain SQL on cancellation.
- **Tenant isolation lives in the schema.** Every tenant-owned child row references its parent with a composite foreign key `(parent_id, workspace_id) REFERENCES parent (id, workspace_id)`, and every tenant-owned parent has `UNIQUE (id, workspace_id)` for it. Every query on tenant data filters by workspace, and a request for another workspace's object gets `404`, never data.
- **Row locks follow the global lock order.** Check every new or changed transaction that locks rows against the plan's **Lock order** constraint: the order of row classes, ascending id within a class, every lock taken at the start with `FOR NO KEY UPDATE` (`FOR SHARE` when read-only), no `FOR UPDATE`, no upgrade from `FOR SHARE`, no deletes outside the rows that constraint allows. A path that can't follow it is a Blocker.

### Security

- **The trust core gets an explicit security review.** Approval, policy, evidence and sandbox code is the trust core. A PR that touches it says so, and its review probes each security claim (see the `katwork-review` skill).
- **Model output is never executed as shell**, in the core or in checks; checks run as argv arrays, and tool ids are matched against a registry.
- **Secrets come only from the environment**, and `Debug` output of configuration redacts them, as the plan's Secrets constraint describes.
- **The repo holds no secrets, API keys, real customer files or third-party source trees.** Third-party clones stay outside it.
