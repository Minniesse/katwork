# Sub-project 1 — Core Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repo, tasks run as Orca workers; the model that didn't write a PR reviews it (spec §19.1).

**Goal:** A self-hosted KatWork server you can start with `docker compose up`. A person logs in (with password reset and demo accounts), sees a workspace with a default AI team, sends a chat message to a Dot, and watches a streamed reply. That reply is produced by a leased, restart-safe job running the Dot's agent loop through a model gateway.

**Architecture:**
- One Rust library crate, `katwork`, holds the config, the domain, persistence, auth, the model gateway, the agent loop, the policy function and the scheduler as focused modules. It never depends on axum.
- A binary crate, `katwork-server`, exposes them over Axum: a JSON API, AG-UI events over SSE, and the built React/Vite web app as static files. The axum extractors for `AuthUser` and `Tenant` live here.
- `katwork::runtime::Core` holds the shared runtime state: the pool, the config, the model registry and the event hub. The server, the worker pool and the pipeline all take it.
- State lives in PostgreSQL. The jobs, tasks and leases tables use `FOR NO KEY UPDATE SKIP LOCKED`, and every lease time comes from Postgres; every lease-expiry comparison uses `clock_timestamp()`.
- Every model call goes through a `ModelProvider` trait. Production uses a Rig adapter for model I/O only; KatWork owns the agent loop (ADR-0001). Tests and keyless demo mode use `FakeProvider`.
- The loop returns facts. One pure policy function turns facts into a job status (spec §5.1).

**Tech Stack:**
- Rust (edition 2024): tokio 1 (with `test-util` as a dev-dependency feature, for paused-clock tests), tokio-util (`CancellationToken`), axum 0.8, sqlx `=0.9.0` (pinned in Task 2; needs Rust 1.94.0 or newer; default features off, with postgres, runtime-tokio, tls-rustls-aws-lc-rs, macros, uuid, chrono, json, migrate; `tls-rustls-aws-lc-rs` gives rustls a single crypto backend, shared with reqwest), serde/serde_json, uuid 1 (v7), chrono, argon2 0.5, lettre 0.11, rig-core `=0.45.0` (pinned in Task 7; it and its sub-crates need Rust 1.97.1; used directly, with explicit reqwest and rustls features), reqwest (the version requirement Rig's reqwest transport declares, checked at Task 7 start, so the lock file holds one reqwest), async-trait, futures (`BoxStream`), unicode-normalization (Task 5), unicode-segmentation and pulldown-cmark (Task 6; pulldown-cmark with `default-features = false`), tracing, tracing-subscriber, thiserror 2; jsonschema as a dev-dependency (Task 9).
- Web: React 19, Vite, TypeScript, Vitest, Node 22 LTS; `use-stick-to-bottom`, and `eventsource-parser` for tests only (Task 13b), both MIT.
- Infrastructure: PostgreSQL 17, Qdrant (container only in SP1), Docker Compose with an optional Caddy HTTPS profile, GitHub Actions.

**Spec:** `docs/spec/katwork-design.md` (§2 decisions, §4 domain model and statuses, §5 architecture, §6 flow, §13 security, §15 testing, §19 sub-project 1).

**Decisions:** `docs/adr/0001-katwork-owns-the-agent-loop.md`, `docs/adr/0002-needs-input-is-final-in-sp1.md`, `docs/adr/0003-own-ag-ui-event-types.md`, `docs/adr/0004-operator-accounts-without-smtp.md`, `docs/adr/0005-local-model-provider.md`.

## Global Constraints

- All KatWork code is Rust; the UI is React + Vite in TypeScript, served by the Rust server (spec §2).
- Licence Apache-2.0. **No GPL/AGPL dependencies**: `cargo deny check licenses` runs in CI with those licences denied.
- Self-host via **Docker Compose** on one Linux host (spec §1.1 goal 1).
- Every cargo command in CI runs with `--locked`. The workspace `rust-version` is checked by the CI job `msrv` (Task 2): 1.94.0 from Task 2, 1.97.1 from Task 7.
- **Axum 0.8 path captures use `{name}`; `/:name` panics when the router is built.** Routes name their captures `{workspace_id}`, `{space_id}`, `{dot_id}`, `{team_id}`, `{thread_id}`, `{job_id}` and `{user_id}`; the `Tenant` extractor reads `{workspace_id}`.
- **English and Thai together.** Every user-facing string comes from `web/src/i18n/{en,th}.json`. API errors are JSON `{"code": "<stable_id>"}`, never prose; each task adds the `KatError` variants for the codes it names.
- **Words.** User-facing copy calls the coworkers "Dots" in both languages. Thai copy describes a Dot as ผู้ช่วย AI and the team as ทีม, and never says พนักงาน AI (spec §3).
- **The harness never runs model output through a shell, in the core or in checks; checks run as argv arrays. Tool ids are matched against a registry** (spec §5.1, §13). From SP2, commands inside a Dot's sandbox run only through the agentOS exec API, as a per-Dot capability that is off by default, bounded, gated like any tool call, and recorded.
- Every model call goes through `ModelProvider`; provider and Space data-residency rules are enforced in the gateway (spec §11).
- **Dependency logs.** The server's tracing subscriber blocks every event whose target starts with `rig` (the `filter_fn` `dependency_log_allowed`, Task 7), and `sqlx` likewise, whatever `RUST_LOG` says; the default filter is `info,rig=info`. No log line carries provider text: a provider error is logged once, by Task 7's adapter, as `model_provider_error` with only the error kind (the `ModelError` code), the HTTP status, the provider request id and the body's byte count; nothing else in KatWork logs a provider body, diagnostic or prompt. Task 14's smoke test checks the `rig` filter.
- The job status set is exactly `accepted | ready | answered | stopped | needs_input | cancelled | error` (spec §4). Only the policy function sets the first five; the harness sets `cancelled` and `error`. In SP1 a job can end as `answered`, `stopped`, `needs_input`, `cancelled` or `error` (`ready` needs an artifact, which arrives in SP2). `needs_input` is final in SP1: the person's answer starts a follow-up job linked by `parent_job_id` (ADR-0002).
- Agent loop limits, initial defaults:
  - at most 24 steps per run, where a step is one model attempt, retries included
  - run wall clock from the model entry (`ModelEntry.timeouts.run_wall_clock`, Task 7b): 90 s for hosted and demo entries, 300 s for local ones; token budget per run 200,000 input+output tokens charged over attempts (a missing count is charged conservatively, Task 8); task deadline 5 minutes from the task's first claim, not its creation; job deadline 10 minutes from the job's creation. Every model or tool call's timeout is `min(its own, the run's remaining time)`
  - tool timeout 60 s. Each model entry carries its own stream timeouts: a hosted entry gets 30 s to its first content chunk and at most 30 s between content chunks; a local entry gets 120 s and 60 s (Task 7b). At most 1 retry per model call, only before its first content chunk (a non-empty `TextDelta`, `Progress`, `ToolCall` or `Done`; Task 8)
  - loop detection: the same tool with the same canonical arguments 3 times in a row, **or** 5 times within the last 10 calls, stops the run before that call runs
  - tool results capped at 8,000 Unicode scalar values, the truncation marker included
  - the history passed to a run is built when the run starts, not when the message arrives: the thread's messages up to the run's request, without `System` notices, cut from the oldest end to 24,000 characters with the request kept whole (Task 12)
- **Secrets.** Install secrets come only from environment variables; `.env.example` lists them and never holds values. `Config` never prints a secret (its `Debug` redacts them). Per-Dot credentials (SP2) live in an encrypted vault, never in env. Every child process gets an env allowlist, never the server's environment.
- **Demo mode with no keys must work.** Synthetic replies carry `messages.synthetic = true`, set only on the `FakeProvider::demo` path; the UI badge reads the flag (spec §15).
- **Reply vs send.** A *reply* goes to the thread the request came from; the harness fills the target and it is not gated. A *send* goes to any other target, or is a proactive post; it is `Outward` and produces a Proposal (SP3). In SP1 the only write is the reply, written once by `job::conclude` (Task 10).
- **Tenant isolation in the schema.** Every tenant-owned child row references its parent with a composite foreign key `(parent_id, workspace_id) REFERENCES parent (id, workspace_id)`, and each tenant-owned parent has `UNIQUE (id, workspace_id)` for it, so a row can never point into another workspace.
- **Roles before bodies.** Every write route that needs an owner or admin calls `Tenant::require_admin()` before it parses the request body, so a member gets 403 `forbidden` whatever the body holds. The Dot `PATCH` (Task 5), whose required role depends on which keys the body sets, checks the role right after reading the keys and before validating any value.
- **Lock first, then check.** Never decide on time or lease validity inside a locking `SELECT`: lock the row first, then re-read it and check `lease_epoch`, state and `clock_timestamp()` expiry in a separate statement of the same transaction. A locking select evaluates its `WHERE` before it waits for the lock, and Postgres re-checks it after the wait only when the row changed, so a lease that expires during the wait, or one renewed after the snapshot, is misjudged. `fence::check`, `fence::lock` (Task 2) and `lease::claim` (Task 11) follow this rule.
- **Time windows.** A quota or rate window uses one cutoff for the whole statement, `statement_timestamp() - interval '24 hours'` (or `'60 seconds'`), never a per-row `clock_timestamp()`: Postgres cannot bound an index scan with a volatile expression, so a per-row cutoff visits every historical row. The window's statement runs after the lock it depends on (the Dot row for quotas), so `statement_timestamp()` is sampled after any lock wait. Lease-expiry checks keep `clock_timestamp()`.
- **Migrations.** A task that changes the schema adds the next numbered file in `crates/katwork/migrations/`: `0002_email_unique.sql` and `0003_email_lowercase.sql` (Task 3), `0004_team_membership.sql` (Task 5), `0005_chat_integrity.sql` (Task 6), `0006_invite_tokens.sql` (Task 3b, which merges after Task 6, so its number follows Task 6's), `0007_task_first_claim.sql` (Task 11), `0008_scheduler_indexes.sql` (Task 12). Tasks 7b and 13b add none.
- **No NUL in Postgres text.** Postgres `text` and `jsonb` cannot hold U+0000. User input containing it is rejected (Task 6); model and tool text has every U+0000 replaced with U+FFFD before it is stored (`log::append` in Task 9, the ledger in Task 8, `job::conclude` in Task 10).
- **Canonical JSON.** Rig turns on serde_json's `preserve_order` feature for the whole workspace, so a `serde_json::Map` keeps insertion order and serializing it does not sort keys. Every canonical form or hash KatWork computes from JSON (`guards::canonical_args`, and through it `args_canonical`, `args_sha256` and the repeat key, in Task 8; `Dot::config_hash` in Task 5) sorts object keys explicitly with `Value::sort_all_objects()` before serializing, never relying on map order; Task 8's `canonical_args_do_not_depend_on_key_order` pins it.
- **Bounded lock waits.** Intake (Task 12) and every `PgEventSink` transaction (Task 9) first run `db::set_lock_timeout(conn, config.lock_timeout)` (`KATWORK_LOCK_TIMEOUT_MS`, default 5,000). SQLSTATE `55P03` (lock not available) converts to `KatError::Busy`, which the API sends as 503 `busy` with `Retry-After: 1`. A paused tokio clock cannot bound a Postgres wait, so the tests set a short timeout instead.
- **Lock order.** Every transaction that waits for row locks takes them in one global order: workspace row → job row → task row, then its `task_leases`, `runs` and `tool_calls` rows → thread row (the row `log::append` updates for `last_event_seq`) → Dot row, then its `dot_spaces` rows. Within one class, rows are locked in ascending id. A path that needs a later lock takes every lock it needs at its start, in this order, with `SELECT … FOR NO KEY UPDATE` (`FOR SHARE` when it only reads the row), before any write. A transaction that will update a row locks it `FOR NO KEY UPDATE` from the start and never upgrades from `FOR SHARE`; a plain `UPDATE` that leaves the key unchanged takes the same lock. **No path uses `FOR UPDATE`, deletes a workspace, user, job, task, thread, Dot, message or run row, or changes any row's key.** The only deletes are `task_leases` rows and non-default `dot_spaces` grants, which no row references. Foreign-key checks on insert and update take `FOR KEY SHARE` on the parent row, which conflicts only with `FOR UPDATE`, a delete or a key change, so a foreign-key check never waits on a lock any path holds. Inserting a row whose parent sits earlier in the order, such as a follow-up job referencing its parent job or any tenant row referencing its workspace, is therefore safe after later locks. Rows a transaction inserts are invisible to others until commit, unique-index checks run under the thread or job lock that already serializes their writers (Task 12), and a `SKIP LOCKED` select never waits. Every remaining wait is for a lock later in the order than any lock already held, so no cycle of waits, and no deadlock, can form. Tasks 2, 4, 8–12 and 13b name the locks each path takes.
- Default team roles: Lead, Researcher, Writer, Analyst, Scheduler, Reviewer (spec §2). The Lead's default `max_concurrency` is 3 and each specialist's is 1 (initial defaults; Task 5 follow-up).

## Review Focus

1. **Cross-tenant access.** A user of workspace A requests workspace B's Dots, threads, jobs or event stream by id, or creates a thread on B's Space or with a team from another Space. Expected: 404, never data, and never a row lock on B's rows. (Task 4, Task 6, Task 9, Task 12.)
2. **Lease double-run and repeated side effects after a crash.** Two workers poll at the same time, and a worker dies mid-run. Expected: a task never runs twice at once. An expired lease is re-queued at most once, and only if the task has not started a non-Read side effect; otherwise it ends `interrupted`. A restart mid-run gives exactly one Dot reply and one job status, or `stopped/interrupted_review_required`. (Task 11, Task 12, Task 14.)
3. **SSE reconnect with `Last-Event-ID`.** The browser drops and reconnects mid-reply, including a Thai reply. Expected: no duplicate and no missing events, no gap when two appenders write to the same thread at once, and text never split inside a grapheme cluster. A page load resumes from the messages snapshot's `event_seq`, a stream the browser gave up on (a 502 during a restart) is reopened with `?after=`, and a server shutdown ends every open stream. (Task 9, Task 13b.)
4. **Password reset and set-password link misuse.**
   - A reset requested for an unknown email gets the same response as a known one.
   - A reset token used twice, or after 30 minutes, fails, whether it came from a reset mail or `reset-link`. An invite token used twice, or after 72 hours, fails, whether it came from `create-user` or a Members-page invite. Neither kind works at the other's route.
   - A reset ends all of that user's existing sessions.
   - An admin invites an email that already has an account. Expected: the membership is added and no link is returned or mailed, so an admin of one workspace can never take over an account another workspace created.
   - No reset or set-password token appears in any log.

   (Task 3, Task 3b, Task 13b.)
5. **Provider down, limited or no keys.** The model provider errors, times out, rate-limits, runs out of quota, rejects the key, or isn't configured. Expected: the job ends `stopped` with reason one of `provider_unavailable | provider_rate_limited | provider_quota | provider_auth_failed` within the timeout, the thread shows a localized message, nothing hangs. In demo mode the reply has `synthetic: true`. (Task 7, Task 12.)
6. **Tool gate bypass.** A tool outside the Dot's allowlist, a destructive or outward tool, a tool call from a failed or `MaxTokens` turn, a call after the allowlist changed mid-run, or a write repeated after its first outcome was lost. Expected: never dispatched; an error result, `needs_input/approval_pending`, `stopped/policy_changed` or `stopped/outcome_unknown`. (Task 8.)
7. **Cross-site and wrong-host requests.** A POST from another origin, or any request with an unknown `Host`. Expected: 403 before a handler runs. (Task 3.)
8. **Duplicate or stranded messages.** A client retries a post, two retries race, a retry reuses the `client_msg_id` with a different body, or the server crashes mid-intake. Expected: one message, one job and one task, or none at all; never a message without its job; a changed body is 409 `client_msg_id_conflict` and writes nothing. (Task 6, Task 12.)
9. **Deadlocks between terminal writers.** Cancel racing completion, a recovery claim racing a claim for a paused Dot, a quota rejection racing a claim. Expected: every path takes its locks in the global order, so none fails with SQLSTATE `40P01`. (Tasks 11 and 12.)
10. **Residency with a local and a hosted provider configured.** A local-only Space's job, including its retries and a provider change between attempts. Expected: every attempt goes to the `Local` entry and no request reaches a hosted provider. (Task 7b, Task 8.)

---

## File Structure

```
Cargo.toml                      workspace: crates/katwork, crates/katwork-server
rust-toolchain.toml             channel = "stable"; workspace rust-version checked by the msrv CI job
deny.toml                       licence + advisory policy (GPL/AGPL denied)
.env.example                    DATABASE_URL, KATWORK_SECRET, POSTGRES_PASSWORD, KATWORK_DEMO, KATWORK_PUBLIC_URL,
                                KATWORK_ALLOW_DEMO_ON_HTTPS, KATWORK_TRUSTED_PROXIES, KATWORK_HEADER_TIMEOUT_SECONDS,
                                KATWORK_IDLE_TIMEOUT_SECONDS, KATWORK_MAX_CONNECTIONS, KATWORK_MAX_CONNECTIONS_PER_IP,
                                KATWORK_DEMO_PASSWORD, KATWORK_BIND, KATWORK_HTTPS_HOST, SMTP_*, OPENAI_*, ANTHROPIC_*,
                                KATWORK_LOCAL_BASE_URL, KATWORK_LOCAL_MODEL, KATWORK_LOCAL_API_KEY, KATWORK_LOCK_TIMEOUT_MS
docker-compose.yml              postgres:17 + qdrant on an internal network; backup → migrate → katwork
docker-compose.https.yml        the Caddy profile: caddy service + KATWORK_TRUSTED_PROXIES and KATWORK_PUBLIC_URL (Task 14)
docker-compose.demo.yml         the offline demo profile (Task 14)
Caddyfile                       used by docker-compose.https.yml (Task 14)
Dockerfile                      multi-stage: web build (node 22) → cargo build --locked → distroless runtime
.github/workflows/ci.yml        fmt, clippy, test, msrv, deny, web, e2e
scripts/init-env.sh             writes .env with random secrets (Task 14)
scripts/demo.sh                 one-command offline demo (Task 14)
scripts/smoke.sh                compose smoke (Task 14)
scripts/compose-backup.sh       pg_dump before migrate, run by the compose backup service (Task 14)
crates/katwork/
  migrations/0001_init.sql      all SP1 tables (Task 2)
  migrations/0002_email_unique.sql, 0003_email_lowercase.sql   (Task 3)
  migrations/0004_team_membership.sql                          (Task 5)
  migrations/0005_chat_integrity.sql                           (Task 6)
  migrations/0006_invite_tokens.sql                            (Task 3b)
  migrations/0007_task_first_claim.sql                         (Task 11)
  migrations/0008_scheduler_indexes.sql                        (Task 12)
  src/lib.rs
  src/config.rs                 Config (Task 1)
  src/error.rs                  KatError + stable codes
  src/ids.rs                    typed UUIDv7 ids, EventSeq, LeaseEpoch
  src/types.rs                  shared domain types (ModelTier, SpacePolicy, Locale, DotStatus, SideEffect, ReasonCode)
  src/runtime.rs                Core
  src/fence.rs                  Fence: the lease check inside every run-scoped write
  src/db.rs                     pool + migrate
  src/auth/{mod,password,session,reset,mailer}.rs
  src/auth/accounts.rs          operator-created accounts and set-password links (Task 3b)
  src/auth/invite.rs            invite tokens: issue, link, accept (Task 3b)
  src/tenancy/{mod,workspace,space}.rs
  src/tenancy/members.rs        member list, invites and role changes (Task 13b)
  src/team/{mod,dot,team,roles}.rs
  src/chat/{mod,thread,message,mentions}.rs
  src/events/{mod,agui,log,hub,sink,coalesce}.rs
  src/models/{mod,provider,registry,guard,fake,rig_adapter}.rs   (Task 7; Task 7b adds the local provider here)
  src/agent/{mod,tool,ctx,registry,builtin,run,guards,ledger}.rs
  src/policy.rs                 decide_job: facts → verdict (pure)
  src/jobs/{mod,job,task,lease,worker,quota}.rs
  src/pipeline.rs               message → job → task (SP1 routing and history)
  tests/*.rs, tests/fixtures/{models,agui}/   (agui/ holds the vendored AG-UI 1.0 schema, MIT, Task 9)
crates/katwork-server/
  src/main.rs                   `serve` | `migrate` | `seed-demo [--reset-password]`
                                | `create-user --email <e> --workspace <name|id> --role <owner|admin|member>
                                  [--space-policy <local|hosted>]`
                                | `reset-link --email <e>` (the last two from Task 3b)
  src/lib.rs src/config.rs      config.rs re-exports katwork::config
  src/app.rs                    AppState { core: Arc<Core> }, build_router
  src/error.rs                  ApiError: KatError → HTTP status + {"code"}
  src/extract.rs                FromRequestParts<AppState> for AuthUser and Tenant; ApiJson<T> (Task 6)
  src/guards.rs                 Host, Origin, Sec-Fetch-Site, JSON-only, body limit, request timeout,
                                concurrency limit, security headers
  src/transport.rs              HTTP/1 serving: header-read and idle timeouts, connection caps (Task 3)
  src/peer.rs                   client IP behind trusted proxies, IPv6 /64 buckets (Task 3)
  src/routes/{health,auth,workspaces,dots,teams,threads,events,jobs,members}.rs
  src/static_files.rs
  tests/*.rs                    HTTP-level tests via tower::ServiceExt; tests/fixtures/sse/
web/
  package.json vite.config.ts tsconfig.json index.html
  scripts/check-contrast.ts
  src/main.tsx src/app.tsx src/router.tsx src/api.ts src/sse.ts src/theme.css src/theme.ts
  src/i18n/{index.ts,en.json,th.json}
  src/components/{Layout,Preferences,Announce,Transcript,Composer}.tsx      (Task 13a)
  src/pages/{Login,ResetRequest,ResetConfirm,Home}.tsx                      (Task 13a)
  src/pages/InviteAccept.tsx                                                (Task 3b)
  src/pages/{Team,Chat,Members}.tsx                                         (Task 13b)
  src/**/*.test.ts(x)
```

---

### Task 1: Workspace skeleton, CI and Compose (delivered in PR #16)

**Status:** implemented on branch `sp1/task-1-skeleton` (PR #16). Tasks 2+ build on what it contains; nothing below is redone.

**Files (as delivered):**
- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `deny.toml`, `.env.example`, `.dockerignore`, `.github/workflows/ci.yml`, `docker-compose.yml`, `Dockerfile`
- `crates/katwork/{Cargo.toml,src/lib.rs,src/config.rs,src/error.rs,tests/config.rs}`
- `crates/katwork-server/{Cargo.toml,src/lib.rs,src/main.rs,src/config.rs,src/app.rs,src/routes/mod.rs,src/routes/health.rs,tests/health.rs,tests/cli.rs}`

**Interfaces:**
- Produces:
  - `katwork::config::Config { database_url: String, secret: String, demo: bool, public_url: Option<String>, demo_password: Option<String>, smtp: Option<SmtpConfig>, openai: Option<ProviderEnv>, anthropic: Option<ProviderEnv> }`, re-exported as `katwork_server::config::Config`
  - `SmtpConfig { host: String, port: Option<u16>, username: Option<String>, password: Option<String>, from: Option<String> }` and `ProviderEnv { api_key: String, base_url: Option<String> }`
  - `Config::from_env() -> Result<Config, KatError>` and `Config::from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Config, KatError>`. `DATABASE_URL` must be non-blank and `KATWORK_SECRET` at least 32 bytes; `KATWORK_DEMO=1` enables demo mode; an empty optional value counts as unset; SMTP is configured only with `SMTP_HOST`, and a provider only with its API key.
  - `Config::redact_secrets(&self, message: &str) -> String`, and hand-written `Debug` impls for `Config`, `SmtpConfig` and `ProviderEnv` that print `[redacted]` for every secret and the database URL without its password
  - `katwork::error::KatError` (thiserror) with `code(&self) -> &'static str`; variants `ConfigInvalid`, `CommandInvalid`, `DatabaseUnavailable`, `Io`
  - `katwork_server::app::build_router(state: AppState) -> axum::Router` with `GET /healthz`
  - `AppState { pool: sqlx::PgPool, config: Arc<Config> }`; Task 2 replaces it with `AppState { core: Arc<Core> }`
  - `katwork-server` subcommands `serve | migrate | seed-demo`, the last two stubbed. A failing command prints its error code and its redacted causes.
- Compose, Docker and CI as delivered:
  - The `katwork` service runs with `cap_drop: [ALL]`, `security_opt: ["no-new-privileges:true"]` and `init: true`, and publishes `"${KATWORK_BIND:-127.0.0.1}:3000:3000"`. It receives `DATABASE_URL`, `KATWORK_SECRET`, `KATWORK_DEMO`, `KATWORK_PUBLIC_URL`, `KATWORK_DEMO_PASSWORD`, `SMTP_HOST`, `SMTP_PORT`, `SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_FROM`, `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL`.
  - `postgres:17` and `qdrant/qdrant:v1.19.2` sit only on the `internal: true` network `backend`, with no published ports.
  - CI jobs: `cargo fmt --check`; `cargo clippy --all-targets --locked -- -D warnings`; `cargo test --workspace --locked` with a `postgres:17` service, followed by `docker compose config -q` and `docker compose build`; `cargo deny check licenses advisories`; and the web job, skipped until `web/` exists.
  - `Dockerfile`: a `node:20-bookworm-slim` web stage (Task 13a moves it to Node 22), `cargo build --locked --release -p katwork-server`, and a `gcr.io/distroless/cc-debian12:nonroot` runtime.
- Tests: `health_returns_ok_and_version`; `config_from_lookup_validates_required_values`, `optional_configuration_requires_host_or_api_key`, `optional_configuration_reads_all_fields`, `smtp_port_is_optional_and_must_be_a_valid_port_when_set`, `config_debug_redacts_secrets`, `redaction_handles_encoded_query_and_malformed_database_passwords`; `maintenance_commands_are_stubs`, `rejects_unknown_commands_and_extra_arguments`, `serve_rejects_missing_configuration`, `serve_reports_database_cause_without_password`.
- Left for later tasks: the MSRV CI job (Task 2), HTTP guards (Task 3), Node 22 (Task 13a), backup before migrate and the HTTPS profile (Task 14).

- [x] **Step 1:** Failing test `health_returns_ok_and_version`, then the workspace, `Config`, `build_router` and the subcommands.
- [x] **Step 2:** Redacting `Debug`, the hardened Compose file, `--locked` CI and the Docker build in CI.
- [x] **Step 3:** Commits authored with the GitHub noreply address.

### Task 2: Schema, typed ids, shared types and `Core`

**Files:**
- Create: `crates/katwork/migrations/0001_init.sql`, `crates/katwork/src/{ids.rs,types.rs,db.rs,runtime.rs,fence.rs}`, `crates/katwork/tests/schema.rs`, `crates/katwork-server/src/error.rs`
- Modify: `Cargo.toml`, `Cargo.lock`, `crates/katwork/{Cargo.toml,src/lib.rs,src/error.rs}`, `crates/katwork-server/src/{app.rs,main.rs}`, `crates/katwork-server/tests/{health.rs,cli.rs}`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `Config`, `KatError`, `AppState`, `build_router` (Task 1).
- Produces:
  - ids: `WorkspaceId, SpaceId, UserId, DotId, TeamId, ThreadId, MessageId, JobId, TaskId, RunId, ToolCallRecordId, ModelCallId`, all UUIDv7 newtypes with `new() -> Self`, `sqlx::Type`, `Serialize`, `Deserialize`, `Display`, `Clone`, `Copy`, `Eq` and `Hash`
  - `EventSeq(i64)`, a per-thread sequence number (`Copy`, `Ord`, `Serialize`, `Display`), and `LeaseEpoch(i64)`
  - `types::ModelTier::{Fast, Standard, Strong}`
  - `types::SpacePolicy { allow_external: bool }`, with no `Default`: every Space states it
  - `types::Locale::{En, Th}`
  - `types::DotStatus::{Active, Paused}`
  - `types::JobStatus::{Accepted, Ready, Answered, Stopped, NeedsInput, Cancelled, Error}` and `types::HoldReason::{DotBusy, ProviderQuota, WorkspacePaused}`, each with `wire(&self) -> &'static str` matching the schema values; they live here because the event payloads (Task 9) carry them
  - `types::Question { question: String, options: Vec<String> }` (`Serialize`, `Deserialize`): a Dot's question to a person; the first option, when present, is the recommended one
  - `types::SideEffect::{Read, Navigate, WorkspaceRead, Draft, WorkspaceWrite, Exec, CredentialUse, Destructive, Outward(OutwardKind)}` and `types::OutwardKind::{Send, Publish, Pay, Share, ExternalApi}`, with `SideEffect::wire(&self) -> &'static str` giving `read, navigate, workspace_read, draft, workspace_write, exec, credential_use, destructive, outward_send, outward_publish, outward_pay, outward_share, outward_external_api`. `SideEffect` serializes as its single wire string (`"outward_send"`, never a nested object), and `SideEffect::from_wire(s: &str) -> Option<SideEffect>` reads it back.
  - `types::ReasonCode` with `wire(&self) -> &'static str` and `ReasonCode::ALL`: `provider_unavailable, provider_rate_limited, provider_quota, provider_auth_failed, model_protocol_error, time_limit, quota_exceeded, workspace_paused, step_limit, token_budget, output_truncated, residency_denied, loop_detected, policy_changed, approval_pending, question_pending, interrupted_review_required, dot_paused, retries_exhausted, cancelled, outcome_unknown, no_eligible_dot, dot_not_in_space, lease_lost`. Each value is a job `reason`, a notice body or a `RUN_ERROR` code, and has a message in both locales (Task 13a). Task 10 adds `too_many_requests`.
  - `db::connect(url: &str) -> Result<PgPool, KatError>` and `db::migrate(pool: &PgPool) -> Result<(), KatError>`
  - `fence::Fence { task_id: TaskId, epoch: LeaseEpoch }` and `fence::check(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError>`. Run inside the caller's write transaction, it runs two statements. The first locks the task row, `SELECT id FROM tasks WHERE id = $1 FOR SHARE`, and may wait for that lock. The second, in the same transaction, selects the task joined to its lease where `tasks.state = 'leased'`, `tasks.lease_epoch` and `task_leases.lease_epoch` both equal `f.epoch`, and `expires_at > clock_timestamp()`; no row is `lease_lost`. Checking in a second statement means a lease that expires while the first statement waits is rejected, even when the lock holder changed nothing and released its lock (issue #23, fixed in PR #25); a single locking select evaluates the expiry before the wait and never again. Every lease-expiry comparison uses `clock_timestamp()`, the time at that statement, never `now()`, which is fixed when the transaction starts and would keep a lease alive after a lock wait. Every path that ends a lease (complete, cancel, reclaim, abandon, and the deadline, retry and eligibility branches) changes `tasks.state` or increments `tasks.lease_epoch` in the same transaction, so the check can never pass for an ended lease. The share lock lasts until the write commits. Claim, cancel and every terminal transition take `FOR NO KEY UPDATE` on the task row, so they wait for an in-flight fenced write, and any write after them fails the check. `fence::lock(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError>` is the same two statements with the first taking `FOR NO KEY UPDATE`, for a caller that will update the task row (`ledger::start_tool_call` for a non-Read call, `lease::complete`, `lease::abandon`), because a lock is never upgraded from `FOR SHARE`.
  - `runtime::Core { pool: PgPool, config: Arc<Config> }` and `Core::new(pool: PgPool, config: Arc<Config>) -> Core`. Task 7 adds `registry: Arc<Registry>`, and Task 9 adds `events: EventHub` and `shutdown: CancellationToken`; all are filled inside `Core::new`, so its signature never changes.
  - `katwork_server::app::AppState { core: Arc<Core> }`
  - `katwork_server::error::ApiError(pub KatError)` with `From<KatError>` and `IntoResponse`: the body is `{"code": e.code()}` and the status comes from one `match` that each later task extends
- Tables, all in `0001_init.sql`. Every tenant-owned row carries `workspace_id UUID NOT NULL`, and every tenant-owned child row references its parent with a composite FK on `(parent_id, workspace_id)` (Global Constraints). `task_leases` is the one exception: it has no `workspace_id`, because a lease is only ever reached through its task id.
  - `workspaces(id, name, paused BOOLEAN NOT NULL DEFAULT false, created_at)`
  - `users(id, email UNIQUE, password_hash, display_name, locale, created_at)`; `memberships(workspace_id, user_id, role, PRIMARY KEY (workspace_id, user_id))` with `role IN ('owner','admin','member')`; `sessions(id, user_id, token_sha256 UNIQUE, created_at, expires_at, revoked_at NULL)`; `password_resets(id, user_id, token_sha256 UNIQUE, expires_at, used_at NULL)`
  - `spaces(id, workspace_id, name, allow_external BOOLEAN NOT NULL)`, with no column default
  - `dots(id, workspace_id, default_space_id UUID NOT NULL, name, role, persona, instructions, tool_allowlist TEXT[] NOT NULL, model_tier, autonomy, appearance JSONB NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','paused')), max_concurrency INT NOT NULL DEFAULT 1 CHECK (max_concurrency > 0), policy_version BIGINT NOT NULL DEFAULT 1, daily_token_budget BIGINT NULL)`; a NULL budget means no cap
  - `dot_spaces(workspace_id, dot_id, space_id, PRIMARY KEY (dot_id, space_id))`, then `ALTER TABLE dots ADD FOREIGN KEY (id, default_space_id) REFERENCES dot_spaces(dot_id, space_id) DEFERRABLE INITIALLY DEFERRED`, so a Dot's default Space is always among its grants
  - `teams(id, workspace_id, space_id, name)`; `team_members(workspace_id, team_id, dot_id, is_lead, position)`, a tenant-owned row, with a partial unique index that allows one lead per team
  - `threads(id, workspace_id, space_id, team_id, title, last_event_seq BIGINT NOT NULL DEFAULT 0)`
  - `messages(id, workspace_id, thread_id, author_kind CHECK (author_kind IN ('user','dot','system')), author_user_id NULL, author_dot_id NULL, body, client_msg_id TEXT NULL CHECK (char_length(client_msg_id) <= 64), idempotency_key TEXT NULL UNIQUE, synthetic BOOLEAN NOT NULL DEFAULT false, question JSONB NULL, job_id UUID NULL, created_at, UNIQUE (thread_id, author_user_id, client_msg_id))`, plus `CHECK ((author_kind = 'user' AND author_user_id IS NOT NULL AND author_dot_id IS NULL) OR (author_kind = 'dot' AND author_dot_id IS NOT NULL AND author_user_id IS NULL) OR (author_kind = 'system' AND author_user_id IS NULL AND author_dot_id IS NULL))`, so every user message has its `author_user_id` and the `client_msg_id` dedupe always applies to it; `question` holds a `Question` when the message asks one; the `job_id` FK is added once `jobs` exists
  - `jobs(id, workspace_id, thread_id, request_message_id UUID NOT NULL UNIQUE REFERENCES messages(id), requested_by UUID NOT NULL REFERENCES users(id), parent_job_id UUID NULL REFERENCES jobs(id), status NULL, reason TEXT NULL, decided_by TEXT NULL CHECK (decided_by IN ('policy','harness')), decision_seq BIGINT NULL, contract_sha256 TEXT NULL, created_at, finished_at NULL)`
    - `status` NULL means running, with `CHECK (status IS NULL OR status IN ('accepted','ready','answered','stopped','needs_input','cancelled','error'))`
    - `reason` holds a `ReasonCode` wire value; `decision_seq` is the per-thread seq of the job's `katwork.decision` event; `contract_sha256` is reserved and always NULL in SP1
    - one job per request message: the unique `request_message_id` means a retried or concurrent intake can never create a second job
  - `tasks(id, workspace_id, job_id, dot_id, dot_policy_version BIGINT NOT NULL, input JSONB, state, attempts INT NOT NULL DEFAULT 0, max_attempts INT NOT NULL DEFAULT 3, idempotency_key TEXT UNIQUE, effects_started_at TIMESTAMPTZ NULL, hold_reason TEXT NULL, lease_epoch BIGINT NOT NULL DEFAULT 0, pending_tool_call JSONB NULL, created_at)`
    - `CHECK (state IN ('queued','leased','done','failed','interrupted','cancelled'))` and `CHECK (hold_reason IN ('dot_busy','provider_quota','workspace_paused'))`
    - terminal states: `done` (concluded by the policy after a run), `failed` (the harness gave up: deadline or retries exhausted), `interrupted` (stopped after a side effect, or by a workspace pause), `cancelled` (its job was cancelled). Every move to a terminal state deletes the task's `task_leases` row in the same transaction.
    - `dot_policy_version` is the Dot's `policy_version` when the task was routed; claim and the loop compare against it, never against whatever the Dot holds when the run starts
    - `pending_tool_call` is `{tool_call_record_id, tool_call_id, name, args_canonical, args_sha256}`, kept when a run stops on `NeedsApproval`: the exact call, with its canonical arguments, bound to its ledger row, so sub-project 3 can rebuild and resume it after a restart (ADR-0002)
  - `task_leases(task_id PRIMARY KEY, worker_id, lease_epoch BIGINT NOT NULL, expires_at)`, with no `workspace_id`; `expires_at` is set from Postgres time, never the worker clock, and compared with `clock_timestamp()`
  - `runs(id, workspace_id, job_id, task_id, dot_id, kind, lease_epoch BIGINT NOT NULL, dot_config_hash TEXT NOT NULL, model TEXT NULL, state, stop_reason TEXT NULL, started_at, ended_at NULL)` with `kind IN ('initial','repair','recovery')` and `state IN ('running','finished','interrupted','cancelled','error')`. Repairs (at most 2, set by the policy from SP3) and recoveries (owned by the scheduler) are counted separately through `kind`.
  - `tool_calls(id, workspace_id, run_id, step INT, tool_call_id TEXT, tool_name TEXT, side_effect TEXT NULL, args_canonical TEXT NOT NULL, args_sha256 TEXT, state TEXT, attempts INT NOT NULL DEFAULT 1 CHECK (attempts IN (1, 2)), result TEXT NULL, started_at, ended_at NULL, UNIQUE (run_id, step, tool_call_id))`, with `side_effect` checked against the 13 `SideEffect::wire` values, `CHECK (side_effect IS NOT NULL OR state = 'refused')` (an unknown tool has no effect), and `state IN ('started','succeeded','failed','timed_out','outcome_unknown','refused')`. `result` holds the exact text passed to the next model call; `attempts` is 2 only after a `Read` retry, which reuses the row.
  - `model_calls(id, workspace_id, space_id, job_id, task_id, run_id, dot_id, provider_id, model, residency, attempt INT, input_tokens BIGINT NULL, cache_read_tokens BIGINT NULL, cache_write_tokens BIGINT NULL, output_tokens BIGINT NULL, usage_complete BOOLEAN NOT NULL, charged_tokens BIGINT NOT NULL, cost_micros BIGINT NULL, latency_ms INT, outcome TEXT, created_at)`: one row per model attempt, retries included; a missing count is NULL, never 0; `charged_tokens` is the attempt's budget charge (Task 8) and is never NULL
  - `events(thread_id, seq BIGINT, workspace_id, run_id NULL, kind, payload JSONB, created_at, PRIMARY KEY (thread_id, seq))`

- [ ] **Step 1: Pin versions and check the MSRV.** Pin `sqlx = "=0.9.0"` in `[workspace.dependencies]` with the features above; `tls-rustls-aws-lc-rs` keeps rustls on one crypto backend, the one reqwest uses from Task 7; `macros` must be listed, because default features are off and `migrate` does not enable `#[sqlx::test]` or the `sqlx::Type` derive. Add `uuid` (v7, serde), `chrono` (serde), `serde`, and tokio's `test-util` feature under `[dev-dependencies]`. Add the CI job `msrv`: `cargo +<rust-version> check --workspace --locked`. SQLx 0.9.0 needs Rust 1.94.0 or newer, so `rust-version` in `Cargo.toml` and the `msrv` job's toolchain both become 1.94.0.
- [ ] **Step 2: Write the failing tests** with `#[sqlx::test]` in `crates/katwork/tests/schema.rs`:
  - `migrations_apply_cleanly`
  - `job_status_rejects_unknown_value`: an insert with status `'done'` errors
  - `thai_text_round_trips`: insert and read back the Dot name `"นักวิจัย"` byte-equal
  - `task_state_accepts_interrupted_rejects_unknown`
  - `event_seq_is_per_thread_and_unique`: two threads both start at seq 1; a duplicate `(thread_id, seq)` errors
  - `dot_default_space_must_be_granted`: committing a Dot whose `default_space_id` has no `dot_spaces` row errors
  - `tool_call_unique_per_run_step_call`
  - `client_msg_id_unique_per_thread_and_author`
  - `job_request_message_is_unique`: a second job for the same `request_message_id` errors
  - `fence_check_fails_after_epoch_or_state_change`: passes for the live lease; fails after the epoch is incremented, after the task leaves `leased`, and after `expires_at` passes
  - `fence_check_rejects_expiry_during_lock_wait_after_commit`, `fence_check_rejects_expiry_during_lock_wait_after_rollback`, `fence_lock_rejects_expiry_during_lock_wait_after_commit` and `fence_lock_rejects_expiry_during_lock_wait_after_rollback` (PR #25): another transaction holds the task row while the lease expires, then commits or rolls back without changing it; both fence modes return `lease_lost`
  - `missing_usage_is_stored_as_null`
  - `space_allow_external_has_no_default`: an insert without `allow_external` errors
  - `side_effect_wire_values_pass_the_schema_check`: every `SideEffect::wire` value inserts into `tool_calls.side_effect`, and `'shell'` is rejected
  - `only_refused_calls_may_have_no_side_effect`
- [ ] **Step 3: Run** `cargo test -p katwork --test schema`. Expected: FAIL.
- [ ] **Step 4: Implement** the migration, the ids, `types.rs`, `db.rs`, `runtime.rs` and `ApiError`. Wire `katwork-server migrate` to `db::connect` + `db::migrate`, and make `serve` build `Core`. Switch `AppState` to `{ core: Arc<Core> }` and update `tests/health.rs`. In `tests/cli.rs`, `maintenance_commands_are_stubs` keeps only `seed-demo`.
- [ ] **Step 5: Run** `cargo test --workspace --locked`. Expected: PASS.
- [ ] **Step 6: Commit** with `feat: initial schema, typed ids, shared types`.

### Task 3: HTTP guards, password auth, sessions, reset, demo accounts

**Status:** merged in PR #20. The interfaces below match its code; Task 3b adds operator accounts and first-run convenience on top.

**Files:**
- Create: `crates/katwork/src/auth/{mod,password,session,reset,mailer}.rs`, `crates/katwork/migrations/{0002_email_unique.sql,0003_email_lowercase.sql}`, `crates/katwork-server/src/{extract.rs,guards.rs,transport.rs,peer.rs,routes/auth.rs}`, `crates/katwork-server/tests/{http_guards.rs,auth.rs,mailer.rs,security.rs,transport.rs,support/mod.rs}`
- Modify: `crates/katwork/src/config.rs`, `crates/katwork-server/src/{app.rs,main.rs}`, `crates/katwork-server/tests/{health.rs,cli.rs}`, `.env.example`, `docker-compose.yml`

**Interfaces:**
- Consumes: Task 2 tables and ids, `Core`, `AppState`, `ApiError`.
- Produces:
  - `Config::public_url_or_default(&self) -> &str`, which is `http://localhost:3000` when `public_url` is unset, and `Config::cookie_secure(&self) -> bool`, true when that URL starts with `https://`. `KATWORK_PUBLIC_URL` must be an `http` or `https` origin with no credentials, path, query or fragment, and is stored canonicalized.
  - `Config` gains `transport: TransportConfig` and `trusted_proxies: Vec<(IpAddr, u8)>`, with `TransportConfig { header_timeout: Duration, idle_timeout: Duration, max_connections: usize, max_connections_per_ip: usize }` read from `KATWORK_HEADER_TIMEOUT_SECONDS` (default 10), `KATWORK_IDLE_TIMEOUT_SECONDS` (default 60), `KATWORK_MAX_CONNECTIONS` (default 256) and `KATWORK_MAX_CONNECTIONS_PER_IP` (default 32); each must be a positive integer, otherwise `config_invalid`.
  - `KATWORK_TRUSTED_PROXIES` is a comma-separated list of CIDRs (Task 3b also accepts bare IPs). `Config::trusts_proxy(&self, ip: IpAddr) -> bool`. Only a socket peer inside one of them may supply `X-Forwarded-For`; the client IP is the first untrusted hop walking back from the peer, and a missing, repeated or malformed header falls back to the peer address. Unset trusts no proxy.
  - Demo mode on an `https://` public URL is `config_invalid` unless `KATWORK_ALLOW_DEMO_ON_HTTPS` is exactly `1`.
  - `katwork_server::transport::serve(tcp: TcpListener, router: Router) -> io::Result<()>` (and `serve_with_shutdown` for tests): HTTP/1 only, a header-read timeout of `header_timeout` (10 s) started before any byte arrives, an idle keep-alive connection closed after `idle_timeout` (60 s) without I/O, at most `max_connections` (256) open connections in total, and at most `max_connections_per_ip` (32) per peer bucket.
  - `peer::ip_bucket(ip: IpAddr) -> IpAddr`: an IPv6 address is bucketed by its /64 and an IPv4-mapped address by its IPv4 address; every per-IP limit and cap uses the bucket.
  - `katwork_server::guards::apply(router: Router, config: &Config) -> Router`, applied by `build_router`:
    - a Host allowlist: the hostname of `public_url_or_default()` plus `localhost` (port ignored); any other or missing `Host` is 403 `host_not_allowed`
    - a request that is not GET or HEAD needs an `Origin` exactly equal to the public URL's origin, otherwise 403 `cross_site_request`; `Sec-Fetch-Site: cross-site` on such a request is also 403 `cross_site_request`
    - a mutation with a body must be `application/json`, otherwise 415 `json_required`
    - `DefaultBodyLimit::max(1_048_576)`
    - a 30 s request timeout (408) and a global limit of 256 requests in flight. Task 9's SSE route is mounted outside these two layers, because a stream outlives both, but inside every other guard.
    - every response carries `X-Content-Type-Options: nosniff` and `Content-Security-Policy: default-src 'self'; frame-ancestors 'none'`
  - Login buckets (`LoginLimiter` in `routes/auth.rs`; the reset route's are `ResetLimiter`), in memory per process: per IP 100 attempts per 15 minutes, per email+IP 10 per 15 minutes, and per email a backstop of 100 per hour, counted only for pairs the first two admit. The reset route limits per IP 20 and per email 3 per hour, and always answers 202. Every bucket uses `ip_bucket`, holds at most 10,000 keys with least-recently-used eviction, and is process-local: a restart or an eviction resets it, and replicas do not share it. Over a limit, login is 429 `rate_limited`.
  - Migrations: `0002_email_unique.sql` adds a unique index on `lower(email)`; `0003_email_lowercase.sql` lowercases and trims stored emails and adds `CHECK (email = lower(email))`. Login, reset and every account path trim and lowercase an email before using it.
  - `password::hash(&str) -> Result<String, KatError>` and `password::verify(&str, &str) -> bool` (argon2id)
  - `katwork::auth::AuthUser { user_id: UserId }`
  - `session::create(pool, UserId) -> Result<SessionToken, KatError>`, `session::authenticate(pool, token: &str) -> Result<AuthUser, KatError>` and `session::revoke(pool, token: &str) -> Result<(), KatError>`, where `SessionToken(String)` is the raw token returned once and only its SHA-256 is stored
  - `reset::request(pool, mailer: &dyn Mailer, email: &str, link_base: &str) -> Result<(), KatError>` and `reset::confirm(pool, token: &str, new_password: &str) -> Result<(), KatError>`
  - `MailKind::{PasswordReset}`, `Mail { kind: MailKind, to: String, subject: String, body: String }` and `#[async_trait] trait Mailer: Send + Sync { async fn send(&self, mail: &Mail) -> Result<(), KatError>; }`
  - `mailer::from_config(config: &Config) -> Arc<dyn Mailer>` picks one backend:
    - `SmtpMailer` (lettre, from `Config::smtp`) when SMTP is configured. It logs only the mail kind and the delivery outcome, never the recipient, subject or body.
    - `DisabledMailer` when SMTP is unset and demo mode is off. It sends nothing and logs `mail_not_sent` with the kind and `smtp_unconfigured`; a reset request still returns the same 202.
    - `ConsoleMailer` only when SMTP is unset and `KATWORK_DEMO=1`: it prints the whole mail, reset link included, under a "demo only" banner, for local demos.
  - `CapturingMailer` for tests: keeps sent mail in memory, with `sent(&self) -> Vec<Mail>`
  - Neither `reset::request` nor any backend except `ConsoleMailer` writes a reset token or link to a log.
  - `impl FromRequestParts<AppState> for AuthUser` in `crates/katwork-server/src/extract.rs` (allowed by the orphan rule because `AppState` is local); no valid session is 401 `unauthenticated`
- Routes: `POST /api/auth/login`, `POST /api/auth/logout`, `POST /api/auth/reset/request`, `POST /api/auth/reset/confirm`, `GET /api/auth/me`.
- Session cookie `kw_session` is `HttpOnly; SameSite=Lax; Path=/`, expiring after 14 days, and `Secure` only when `cookie_secure()`. On an `http://` public URL, `serve` logs a startup warning that the cookie is not `Secure`.
- `seed-demo` needs `KATWORK_DEMO=1` (`config_invalid` before connecting otherwise). It creates `demo@katwork.local` and a demo workspace, idempotently. The password is `KATWORK_DEMO_PASSWORD` when set; otherwise it is random and printed when the user is first created. `seed-demo --reset-password` sets the password again (from `KATWORK_DEMO_PASSWORD`, or a new random one) and prints it.

- [ ] **Step 1: Write the failing guard tests** in `crates/katwork-server/tests/http_guards.rs`:
  - `cross_site_post_is_403`: `Origin: https://evil.example` gives 403 `cross_site_request`
  - `unrecognized_host_is_403`: `Host: evil.example` gives 403 `host_not_allowed`
  - `non_json_mutation_is_415`
  - `responses_carry_nosniff_and_csp`
  - `request_timeout_finishes_with_408_after_thirty_seconds` and `concurrent_request_limit_rejects_overload`
  - in `tests/transport.rs`: `partial_header_connection_closes_at_ten_seconds`, `keep_alive_connection_closes_at_configured_idle_timeout`, `one_peer_cannot_hold_more_than_thirty_two_connections`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test http_guards --test transport`. Expected: FAIL.
- [ ] **Step 3: Implement** `guards.rs` and apply it in `build_router`; `tests/health.rs` now sends `Host: localhost`. Run again. Expected: PASS.
- [ ] **Step 4: Write the failing auth tests** in `crates/katwork-server/tests/auth.rs`:
  - `login_with_correct_password_sets_cookie`
  - `login_with_wrong_password_is_401_without_detail`
  - `reset_request_same_response_for_unknown_email`: both return 202 with an identical body
  - `reset_token_single_use`: the second confirm returns 400 `reset_token_invalid`
  - `reset_token_expires_after_30_minutes`: the test moves `expires_at` into the past in SQL
  - `reset_revokes_existing_sessions`
  - `reset_token_never_appears_in_logs`: with a `CapturingMailer` and all tracing output captured, the token from the captured mail appears nowhere in the logs
  - `reset_without_smtp_returns_202_and_sends_nothing`: SMTP unset and demo off selects `DisabledMailer`
  - `cookie_secure_when_public_url_is_https`
  - `cookie_not_secure_on_http_public_url`
  - `seed_demo_creates_demo_user_and_workspace`: idempotent; with `KATWORK_DEMO_PASSWORD` set, login works with that password
  - `seed_demo_reset_password_allows_login`
  - `login_rate_limited_per_email_and_peer_ip`, `forwarded_ips_are_used_only_behind_trusted_proxies` (in `tests/security.rs`), `demo_on_https_requires_explicit_override` (in `crates/katwork/tests/config.rs`) and `migration_normalizes_existing_email_and_login_accepts_any_case`
- [ ] **Step 5: Run** `cargo test -p katwork-server --test auth`. Expected: FAIL.
- [ ] **Step 6: Implement** the modules and routes. Reset tokens are 32 random bytes in URL-safe base64, stored hashed, with `expires_at = clock_timestamp() + interval '30 minutes'` and `used_at`; the emailed link is `{public_url}/reset?token=…`. Login and reset use the buckets above. Delete `maintenance_commands_are_stubs` from `tests/cli.rs`.
- [ ] **Step 7: Run.** Expected: PASS.
- [ ] **Step 8: Commit** with `feat: http guards, password auth, sessions, reset, demo seed`.

### Task 4: Workspaces, Spaces and tenant scoping

**Status:** merged in PR #22. The interfaces below match its code.

**Files:**
- Create: `crates/katwork/src/tenancy/{mod,workspace,space}.rs`, `crates/katwork-server/src/routes/workspaces.rs`, `crates/katwork-server/tests/tenancy.rs`
- Modify: `crates/katwork-server/src/{extract.rs,main.rs}`, `Cargo.toml` and `crates/katwork/Cargo.toml` (`unicode-properties`)

**Interfaces:**
- Consumes: `AuthUser`, `types::SpacePolicy`.
- Produces:
  - `katwork::tenancy::Tenant`, sealed: its fields are private, and it exposes `workspace_id(&self) -> WorkspaceId`, `user_id(&self) -> UserId`, `role(&self) -> Role` and `require_admin(&self) -> Result<(), KatError>` (`Ok` for an owner or admin, otherwise `forbidden`). It is `Clone`, not `Copy`. Only `tenancy::resolve` constructs it, so the worker and every test get a `Tenant` from a real membership row. Later tasks read it only through these accessors.
  - `Role::{Owner, Admin, Member}` (`Copy`, serde and `sqlx::Type` as `owner | admin | member`)
  - `tenancy::resolve(pool, user_id: UserId, workspace_id: WorkspaceId) -> Result<Tenant, KatError>`: a non-member gets `NotFound`. The extractor uses it for the signed-in user, and the worker (Task 11) for a job's requester. It reads the membership on every request, so a removed or downgraded member loses access on the next request.
  - `tenancy::validate_name(name: &str) -> Result<&str, KatError>` (crate-visible), used for workspace, Space, Dot and team names: a name containing any character of Unicode category Cc, Cf, Zl or Zp is rejected, then the name is trimmed, and an empty name or one over 120 Unicode scalar values is rejected; every rejection is 400 `request_invalid`. It returns the trimmed name, which is what gets stored.
  - `impl FromRequestParts<AppState> for Tenant` in `extract.rs`: reads `{workspace_id}`, requires `AuthUser`, calls `resolve`; a non-member gets 404 `not_found`
  - `workspace::create(pool, name: &str, owner: UserId) -> Result<WorkspaceId, KatError>`
  - `workspace::list_for(pool, user_id: UserId) -> Result<Vec<WorkspaceSummary>, KatError>` and `WorkspaceSummary { id: WorkspaceId, name: String, role: Role, paused: bool }`
  - `workspace::set_paused(pool, &Tenant, paused: bool) -> Result<(), KatError>`: owner or admin, otherwise `forbidden`. It locks and updates only the workspace row, the first class in the lock order, with a plain update (`FOR NO KEY UPDATE`), which never blocks the foreign-key checks of rows inserted into that workspace. Running tasks see the pause through `guards::authorize` or their heartbeat and end through `lease::complete` (Tasks 8 and 11).
  - `space::create(pool, &Tenant, name: &str, policy: SpacePolicy) -> Result<SpaceId, KatError>`: owner or admin
  - `space::list(pool, &Tenant) -> Result<Vec<Space>, KatError>` and `space::get(pool, &Tenant, SpaceId) -> Result<Space, KatError>`
  - `Space { id: SpaceId, name: String, policy: SpacePolicy }`
- Routes:
  - `GET/POST /api/workspaces`
  - `GET/POST /api/workspaces/{workspace_id}/spaces`. `POST` takes `{name, allow_external}`; without `allow_external` it returns 422 `space_policy_required`, because the admin chooses local-only or hosted-allowed for every Space (spec §11).
  - `PUT /api/workspaces/{workspace_id}/paused` with `{"paused": bool}`
  - `POST …/spaces` and `PUT …/paused` call `require_admin()` before parsing the body (Global Constraints), so a member gets 403 even with an invalid body.
- `seed-demo` creates the demo Space with `allow_external = true`.

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/tenancy.rs`:
  - `member_lists_own_spaces`
  - `non_member_gets_404_not_403`: Review Focus 1
  - `member_cannot_create_space_admin_can`
  - `create_space_without_policy_is_422`
  - `seed_demo_space_allows_external`
  - `member_cannot_pause_workspace_admin_can`
  - `member_cannot_pause_workspace_with_invalid_body`: 403, not 400
  - `removed_membership_denies_every_tenant_route_on_next_request` and `downgraded_admin_cannot_write_on_next_request`
  - `empty_names_are_rejected`, `whitespace_only_names_are_rejected`, `names_over_120_characters_are_rejected`, `control_characters_in_names_are_rejected`, `format_characters_in_names_are_rejected`, `bidi_controls_in_names_are_rejected`, `line_and_paragraph_separators_in_names_are_rejected` and `names_are_trimmed_and_allow_120_unicode_characters`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test tenancy`. Expected: FAIL.
- [ ] **Step 3: Implement.** Every repository function takes `&Tenant` and filters by `workspace_id` in SQL.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: workspaces, spaces, tenant scoping`.

### Task 3b: Operator accounts and first-run convenience

This task comes after Task 4 and lands as its own PR, a small full-stack task: the backend plus one web page. It can be written in parallel with Task 5, but merges after Task 6, because its migration `0006_invite_tokens.sql` must follow Task 6's `0005_chat_integrity.sql`, and after PR #27 (Task 13a), which creates `web/`. It needs Task 4's `Role` and `tenancy::validate_name`, changes Task 3's code, and uses Task 13a's web shell. Accounts then work both ways: with SMTP through mail, and without SMTP through a link the operator prints (ADR-0004). Task 13b builds the admin Members page on it.

**Files:**
- Create: `crates/katwork/src/auth/{accounts.rs,invite.rs}`, `crates/katwork/migrations/0006_invite_tokens.sql`, `web/src/pages/InviteAccept.tsx`, `web/src/pages/InviteAccept.test.tsx`
- Modify: `crates/katwork/src/{config.rs,error.rs,auth/mod.rs,auth/reset.rs,auth/mailer.rs}`, `crates/katwork-server/src/{main.rs,app.rs,error.rs,guards.rs,transport.rs,routes/auth.rs}`, `crates/katwork/tests/config.rs`, `crates/katwork-server/tests/{cli.rs,http_guards.rs,auth.rs,transport.rs,mailer.rs,security.rs}`, `web/src/{router.tsx,api.ts}`, `web/src/i18n/{en,th}.json`, `.env.example`

**Interfaces:**
- Consumes: `Config`, `password`, `reset`, `Mail`, `Mailer`, `LoginLimiter` (Task 3); `Role`, `tenancy::validate_name` (Task 4); `types::{Locale, SpacePolicy}` (Task 2); `router.tsx`, `api.ts`, `i18n` and the `ResetConfirm` page (Task 13a, PR #27).
- Produces:
  - `KatError::ConfigInvalid { var: &'static str }` replaces the unit variant. `code()` stays `config_invalid`, and the line the CLI prints is `config_invalid (VAR)`. The variable's value never appears in the error, its causes or any log. Every `Config::from_lookup` failure names the variable that failed, and `seed-demo` without demo mode names `KATWORK_DEMO`.
  - `KatError::RateLimited { retry_after_secs: u64 }` replaces the unit variant; `ApiError` sends it as 429 `rate_limited` with a `Retry-After: <retry_after_secs>` header.
  - New codes `user_exists` (`KatError::UserExists`), `workspace_ambiguous` (`KatError::WorkspaceAmbiguous`) and `invite_token_invalid` (`KatError::InviteTokenInvalid`, 400).
  - `reset::issue(conn: &mut PgConnection, user_id: UserId) -> Result<String, KatError>`: inserts one `password_resets` row with Task 3's mechanics (32 random bytes in URL-safe base64, stored as its SHA-256, `expires_at = clock_timestamp() + interval '30 minutes'`, single use) and returns the raw token. `reset::request` calls it after its three-pending-tokens check and `reset-link` calls it directly, so that cap applies only to the public request route.
  - `reset::link(link_base: &str, token: &str) -> String`: `{link_base without a trailing /}/reset?token={token}`; the reset mail uses it too. `reset-link` prints a reset link: the person opens it and chooses a password through `POST /api/auth/reset/confirm`, and every reset rule applies (single use, 30 minutes, the account's sessions and other pending tokens ended).
  - `reset::confirm(pool, token: &str, new_password: &str) -> Result<String, KatError>`: now returns the account's stored (lowercase) email, so the route can clear that email's login buckets.
  - Invite tokens are their own kind, so a reset token and an invite token can never stand in for each other:
    - migration `0006_invite_tokens.sql`: `ALTER TABLE password_resets ADD COLUMN kind TEXT NOT NULL DEFAULT 'reset' CHECK (kind IN ('reset', 'invite'))`; every existing row and every token from Task 3 is `reset`
    - `reset::request`, `reset::issue` and `reset::confirm` write and accept only `kind = 'reset'`, and the three-pending cap counts only reset tokens
    - `invite::issue(conn: &mut PgConnection, user_id: UserId) -> Result<String, KatError>`: one `kind = 'invite'` row with the reset token format (32 random bytes in URL-safe base64, stored as its SHA-256), `expires_at = clock_timestamp() + interval '72 hours'`, single use
    - `invite::link(link_base: &str, token: &str) -> String`: `{link_base without a trailing /}/invite?token={token}`
    - `invite::accept(pool, token: &str, new_password: &str) -> Result<String, KatError>`: for an unused, unexpired `invite` token only, it sets the password, marks the token used, ends the account's sessions and its other pending tokens of both kinds, and returns the account's email; any other token is 400 `invite_token_invalid`. `reset::confirm` likewise ends pending tokens of both kinds.
    - route `POST /api/auth/invite/accept` with `{token, new_password}` → 204, under reset confirm's body, password and rate rules; on success it calls `LoginLimiter::clear_email` with the returned email.
    - `InviteAccept.tsx` at `/invite`, built like Task 13a's `ResetConfirm`: it takes the token from the link, removes it from the address bar, asks for the new password twice, and posts to `POST /api/auth/invite/accept`; `invite_token_invalid` shows a localized message that suggests asking for a new invite or using password reset. Its strings live in `web/src/i18n/{en,th}.json`.
  - `accounts::normalize_email(email: &str) -> Result<String, KatError>`: trims and lowercases; more than 254 bytes, or anything but exactly one `@` with text on both sides, is `request_invalid`.
  - `accounts::EnsuredUser { user_id: UserId, created: bool }` and `accounts::ensure_user(conn: &mut PgConnection, email: &str) -> Result<EnsuredUser, KatError>`: returns the account for the normalized email, or inserts one with `display_name` = the part before `@`, `locale = 'en'` and the Argon2id hash of 32 random bytes that nobody ever sees, so the account has no usable password until its link is used. It runs `INSERT … ON CONFLICT DO NOTHING RETURNING` and then selects the committed row, so concurrent calls for one email create one account.
  - `accounts::WorkspaceRef::{Id(WorkspaceId), Name(String)}` and `WorkspaceRef::parse(s: &str) -> WorkspaceRef`: a string that parses as a UUID is an id; anything else is a name.
  - `accounts::create_user(pool, email: &str, workspace: &WorkspaceRef, role: Role, space_policy: Option<SpacePolicy>, link_base: &str) -> Result<String, KatError>`, in one transaction:
    - an email that already has an account is `user_exists` and nothing changes; `reset-link` serves existing accounts
    - an `Id` must name a workspace, otherwise `not_found`
    - a `Name` matching exactly one workspace joins it; more than one is `workspace_ambiguous`; none creates a workspace with that validated name when `role` is `Owner`, otherwise `not_found`. A new workspace also gets its first Space, named `General`, with `space_policy`; without one, the command fails with `ConfigInvalid { var: "--space-policy" }` and nothing is created. `space_policy` is ignored when the workspace already exists. The new `General` Space gets its default team in the Task 5 follow-up (Step 9), once Task 5 is on main; until then it has none.
    - then `ensure_user`, the membership with `role`, and `invite::issue`; it returns `invite::link(link_base, &token)`, an invite link, because the person it is for often opens it much later
  - `accounts::reset_link(pool, email: &str, link_base: &str) -> Result<String, KatError>`: an unknown email is `not_found`; otherwise `reset::issue` and its link
  - CLI subcommands `katwork-server create-user --email <e> --workspace <name|id> --role <owner|admin|member> [--space-policy <local|hosted>]` and `katwork-server reset-link --email <e>`. `--space-policy` is required when `create-user` creates the workspace (`local` gives `allow_external = false`, `hosted` gives `true`) and is ignored otherwise. Each flag appears at most once, in any order; a missing required flag other than `--space-policy`, a repeated or unknown flag, any other role and any other space policy are `command_invalid`. They load `Config` like `migrate`, use `public_url_or_default()` as the link base, and need neither SMTP nor demo mode. On success the command writes only the link and a newline to stdout; the link never goes through `tracing`. `create-user` prints an invite link, valid 72 hours; `reset-link` prints a reset link, valid 30 minutes; each works once.
  - `LoginLimiter::reserve(&self, email: &str, ip: IpAddr) -> Result<LoginAttempt, KatError>`: counts the attempt in all three login buckets, or returns `RateLimited { retry_after_secs }` when one is full, where `retry_after_secs` is the time until the last full bucket's window ends, rounded up and at least 1. `LoginAttempt::succeeded(self)` gives the attempt back to the IP and email buckets and clears the email+IP bucket. A `LoginAttempt` dropped after a failure stays counted, so only failed attempts count, and concurrent attempts can never pass a limit. `LoginLimiter::clear_email(&self, email: &str)` removes every bucket of that email (each email+IP bucket and the backstop) and never an IP bucket.
  - `mailer::reset_mail(to: String, locale: Locale, link: &str) -> Mail`: a bilingual mail in the user's locale order. The subject is `ตั้งรหัสผ่าน KatWork ใหม่ / Reset your KatWork password` for `th`, and the two halves swapped for `en`. The body is the paragraph in the user's locale, then the other one, then the link alone on the last line. Thai: `ใช้ลิงก์นี้ภายใน 30 นาทีเพื่อตั้งรหัสผ่านใหม่ หากคุณไม่ได้ขอเปลี่ยนรหัสผ่าน ให้ละเว้นอีเมลนี้`. English: `Use this link within 30 minutes to set a new password. If you did not ask for this, ignore this email.`

- [ ] **Step 1: Write the failing operator-command tests** in `crates/katwork-server/tests/cli.rs`:
  - `create_user_prints_a_single_use_set_password_link`: stdout is one `/invite` link; its token sets a password through `POST /api/auth/invite/accept`, login then works, and a second accept is 400 `invite_token_invalid`
  - `create_user_creates_a_new_workspace_only_for_an_owner`: an unknown name with `--role owner --space-policy local` creates the workspace with the account as its owner and one Space `General` with `allow_external = false`, and `--space-policy hosted` gives `allow_external = true`; with `--role admin` it is `not_found` and nothing is created
  - `create_user_new_workspace_requires_space_policy`: an unknown name with `--role owner` and no `--space-policy` fails with `config_invalid (--space-policy)`, and no account, workspace or Space is created; joining an existing workspace without the flag succeeds
  - `create_user_joins_an_existing_workspace_by_id_or_name`
  - `create_user_rejects_an_ambiguous_workspace_name`: `workspace_ambiguous`
  - `create_user_rejects_an_existing_email`: `user_exists`; the account and its password are unchanged
  - `create_user_link_lasts_72_hours_and_reset_link_30_minutes`: the stored `expires_at` values are 72 hours and 30 minutes after issue, and each link fails once the test moves its `expires_at` into the past in SQL
  - `reset_link_prints_a_link_for_an_existing_account`: confirming it sets the new password and ends the account's sessions
  - `reset_link_for_an_unknown_email_is_not_found`
  - `operator_commands_need_no_smtp_or_demo_mode`
  - `operator_commands_reject_bad_arguments`: a missing or repeated flag, an unknown flag and `--role guest` are each `command_invalid`
  - `operator_links_never_reach_logs`: with all tracing output captured, the printed token appears nowhere in it
  - in `tests/auth.rs`: `invite_and_reset_tokens_cannot_be_swapped`: an invite token at `POST /api/auth/reset/confirm` is 400 `reset_token_invalid`, and a reset token at `POST /api/auth/invite/accept` is 400 `invite_token_invalid`
  - in `web/src/pages/InviteAccept.test.tsx` (Vitest, mocked `fetch`): `invite_accept_sets_a_password_and_hides_the_token`, and `invite_accept_explains_an_invalid_or_used_link` in both locales
- [ ] **Step 2: Run** `cargo test -p katwork-server --test cli --test auth && (cd web && npm test)`. Expected: FAIL.
- [ ] **Step 3: Implement** `accounts.rs`, `invite.rs` with migration `0006_invite_tokens.sql` and the accept route, the `kind` filter in `reset.rs`, `reset::issue`, `reset::link`, the two subcommands, and `InviteAccept.tsx` with its route in `router.tsx`. Run again. Expected: PASS.
- [ ] **Step 4: Loopback hosts.** Write `loopback_ip_hosts_are_allowed` in `tests/http_guards.rs`: `Host: 127.0.0.1:3000` and `Host: [::1]:3000` pass the Host check, and `Host: 127.0.0.2` is 403 `host_not_allowed`. Run it (FAIL). Make the allowlist accept `localhost`, `127.0.0.1` and `[::1]`, any port, beside the public URL's hostname. Run it (PASS).
- [ ] **Step 5: `/healthz` skips the Host check.** Write `healthz_skips_the_host_check`: `GET /healthz` with `Host: katwork:3000` (a container health probe) is 200 and still carries the security headers, while `GET /api/auth/me` with that Host is 403 `host_not_allowed`. Run it (FAIL). Exempt only `GET` and `HEAD` on `/healthz`. Run it (PASS).
- [ ] **Step 6: An Origin that matches the allowed Host.** Write `origin_matching_the_allowed_host_is_accepted`: a `POST` with `Host: 127.0.0.1:3000` and `Origin: http://127.0.0.1:3000` passes the Origin check, while `Origin: http://127.0.0.1:4000` and `Origin: https://127.0.0.1:3000` are 403 `cross_site_request`. Run it (FAIL). Accept an `Origin` that equals the public URL's origin, or equals `<the public URL's scheme>://<the request's Host header>` once that Host has passed the allowlist; `Sec-Fetch-Site: cross-site` stays 403. Run it (PASS).
- [ ] **Step 7: Log Host and Origin rejections.** Write `host_and_origin_rejections_log_the_rejected_value`: with tracing captured, `Host: evil.example` logs `host_not_allowed` with `host = "evil.example"`, a cross-site `POST` logs `cross_site_request` with `origin = "https://evil.example"`, and a 1,000-byte Host value is logged cut to 256 bytes. Run it (FAIL). Emit one `warn` event per rejection, with the rejected value as a Debug-escaped structured field cut to 256 bytes on a UTF-8 boundary, and no other header. Run it (PASS).
- [ ] **Step 8: `config_invalid` names its variable.** Write `serve_config_error_names_the_variable_not_its_value` in `tests/cli.rs`: with `KATWORK_PUBLIC_URL=ftp://private-marker.example`, stderr contains `config_invalid (KATWORK_PUBLIC_URL)` and never `private-marker`. Run it (FAIL). Implement `ConfigInvalid { var }` through `Config::from_lookup` and update every match on the old variant, Task 7's base-URL check included (whichever of PR #21 and this PR merges second adapts). Run it (PASS).
- [ ] **Step 9: Login lockout counts only failures.** Write in `tests/auth.rs`:
  - `successful_logins_never_lock_out`: 30 correct logins from one IP within 15 minutes all succeed
  - `a_successful_login_clears_the_failure_count`: 9 failures, 1 success, then 10 more failures all get 401, and the 11th gets 429
  - `reset_confirm_clears_login_lockout`: after 10 failures the correct password gets 429; after a reset confirm, login with the new password succeeds
  - `login_429_sends_retry_after`: `Retry-After` is an integer from 1 to 900

  Run them (FAIL). Implement `reserve`, `LoginAttempt::succeeded` and `clear_email`: the login route calls `succeeded` only after the password verifies, and the reset confirm route calls `clear_email` with the email `reset::confirm` returns. Run them (PASS).
- [ ] **Step 10: Trusted proxies and dropped connections.** Write:
  - `trusted_proxies_accept_bare_ips_and_cidrs` in `crates/katwork/tests/config.rs`: `10.0.0.2` means `10.0.0.2/32`, `fd00::2` means `fd00::2/128`, `10.0.0.0/8` still works, and `10.0.0.2/33` is `config_invalid (KATWORK_TRUSTED_PROXIES)`
  - `trusted_proxy_is_exempt_from_the_per_peer_cap` in `tests/transport.rs`: 40 connections from a trusted peer all stay open while an untrusted peer's 33rd is closed; both still count toward the global cap of 256
  - `dropped_connections_are_logged`: a connection refused by the per-peer cap logs `connection_dropped` at `warn` with `reason = "per_peer_cap"` and the peer bucket, at most once per bucket per 60 s

  Run them (FAIL). Implement in `config.rs` and `transport.rs`, and describe both forms in the `KATWORK_TRUSTED_PROXIES` comment of `.env.example`. Run them (PASS).
- [ ] **Step 11: `seed-demo` applies a changed `KATWORK_DEMO_PASSWORD`.** Write `seed_demo_applies_changed_demo_password` in `tests/cli.rs`: after a seed with one value, a run with another makes the new password work and the old one fail, and ends the demo user's sessions; a run with the same value changes nothing and keeps the sessions. Run it (FAIL). When `KATWORK_DEMO_PASSWORD` is set and does not verify against the stored hash, `seed-demo` stores its hash and calls `reset::revoke_credentials` (Task 3's helper that ends the account's sessions and pending reset tokens), printing nothing, because the operator chose the value. Run it (PASS).
- [ ] **Step 12: Bilingual reset mail.** Write `reset_mail_is_bilingual_in_the_users_locale_order` in `tests/mailer.rs`: for a `th` user the Thai subject half and paragraph come first, for an `en` user the English ones; each body holds the link exactly once, on its last line. Run it (FAIL). Implement `mailer::reset_mail` and use it in `reset::request` with the account's `locale`. Run it (PASS).
- [ ] **Step 13: SMTP needs a public URL.** Write `smtp_without_public_url_is_config_invalid` in `crates/katwork/tests/config.rs`: `SMTP_HOST` set with `KATWORK_PUBLIC_URL` unset is `ConfigInvalid { var: "KATWORK_PUBLIC_URL" }`, so a mailed link can never point at `localhost`. Run it (FAIL). Implement it in `Config::from_lookup` and set `KATWORK_PUBLIC_URL` in the existing tests that configure SMTP. Run it (PASS).
- [ ] **Step 14: Wait on observed state, not time, in the reset-limit test.** `reset_limits_apply_before_lookup_and_always_return_202` in `crates/katwork-server/tests/security.rs` sleeps a fixed 50 ms for the background reset lookups, twice, and failed under load. Make the reset work observable: `ResetWork` in `routes/auth.rs` becomes `pub`, with `RESET_WORK_SLOTS: u32 = 32` and `ResetWork::drained(&self)`, an async fn that acquires all `RESET_WORK_SLOTS` permits of its semaphore and releases them, so it returns only once no spawned lookup holds a permit; `app::build_router_with(state: AppState, reset_work: ResetWork) -> Router` takes the instance, and `build_router(state)` calls it with `ResetWork::default()`. Rewrite the test to build its router with its own `ResetWork` and replace both sleeps with `work.drained().await`. Run `for i in $(seq 50); do cargo test -p katwork-server --test security reset_limits_apply_before_lookup_and_always_return_202 -- --test-threads=16 || exit 1; done` while a second copy of the same loop runs. Expected: PASS on every run.
- [ ] **Step 15: Run** `cargo test --workspace --locked && (cd web && npm test && npm run build)`. Expected: PASS.
- [ ] **Step 16: Commit** with `feat: operator accounts and first-run convenience`.

### Task 5: Dots, Teams and the default roster

**Status:** in review as PR #24. The interfaces below include its review rulings. A follow-up PR (Steps 6–10) comes after it and Task 3b merge.

**Files:**
- Create: `crates/katwork/src/team/{mod,dot,team,roles}.rs`, `crates/katwork/migrations/0004_team_membership.sql`, `crates/katwork-server/src/routes/{dots,teams}.rs`, `crates/katwork-server/tests/team.rs`
- Modify: `crates/katwork-server/src/main.rs` (`seed-demo`)

**Interfaces:**
- Consumes: `Tenant`, `SpaceId`, `types::{ModelTier, Locale, DotStatus}`.
- Produces:
  - `Dot { id: DotId, default_space_id: SpaceId, spaces: Vec<SpaceId>, name: String, role: DotRole, persona: String, instructions: String, tool_allowlist: Vec<String>, model_tier: ModelTier, autonomy: Autonomy, status: DotStatus, appearance: Appearance, max_concurrency: u16, daily_token_budget: Option<i64>, policy_version: i64 }`
  - `Dot::config_hash(&self) -> String`: hex SHA-256 of the canonical JSON of `persona`, `instructions`, `tool_allowlist`, `model_tier`, `autonomy` and `policy_version`
  - `DotRole::{Lead, Researcher, Writer, Analyst, Scheduler, Reviewer, Custom(String)}` and `Autonomy::{AskFirst, Standard}`
  - `Appearance { avatar: String, accent: String }`, stored in `dots.appearance`
  - `NewDot { name, role, persona, instructions, tool_allowlist, model_tier, autonomy, appearance }`
  - `DotPatch { name, persona, instructions, tool_allowlist, model_tier, autonomy, status, appearance, max_concurrency, daily_token_budget }`, every field an `Option`. `daily_token_budget: Option<Option<i64>>` carries `#[serde(default, deserialize_with = "double_option")]`, where `double_option` is a small local deserializer: an omitted field stays `None` (no change), JSON `null` becomes `Some(None)` (remove the cap), and a number becomes `Some(Some(n))`. So `{"daily_token_budget": null}` clears the cap. `DotPatch` has no `default_space_id` and rejects unknown fields, so a `PATCH` that sets `default_space_id` is 400 `request_invalid`: a Dot's default Space never changes after hire.
  - Free-text limits (initial defaults): `persona` at most 4,000 Unicode scalar values, `instructions` at most 8,000, and each `appearance` field (`avatar`, `accent`) at most 200. All of them reject NUL and every other Cc control character, except that `persona` and `instructions` keep tab, line feed and carriage return. A violation is 400 `request_invalid`.
  - `tool_allowlist` entries must match `^[a-z][a-z0-9_.]{0,63}$`, otherwise 400 `request_invalid`; the list is stored sorted and deduplicated, so `config_hash` and the `policy_version` comparison always see one canonical form. `config_hash` follows the canonical-JSON rule (Global Constraints).
  - `HireDot { name: String, role: DotRole, persona: String }`, the hire request
  - `roles::template(role: &DotRole, locale: Locale) -> NewDot`: the defaults a hire starts from: the role's instructions, `tool_allowlist = ["read_thread", "ask_user"]`, `model_tier = Standard`, `autonomy = AskFirst`, and the role's avatar (a neutral avatar for `Custom`); the request's name and persona replace the template's
  - Who may change what: any member may hire a Dot, edit its `name`, `persona` and `appearance`, create teams and change team membership. Only an owner or admin may change execution-affecting settings (`instructions`, `tool_allowlist`, `model_tier`, `autonomy`, `status`, `max_concurrency`, `daily_token_budget`) and grants; a member gets 403 `forbidden`.
  - Dot names are unique among a team's active members, compared by their Unicode `NFKC_Casefold` mapping, so `Writer`, `writer` and the fullwidth `Ｗｒｉｔｅｒ` collide; `team::create`, `team::add_member` and a rename through `dot::update` return 422 `dot_name_conflict` otherwise. Dot and team names pass `tenancy::validate_name` (Task 4), must contain at least one letter or digit (Unicode category L or N), and must not contain a `Default_Ignorable_Code_Point` (an invisible code point such as U+200B or U+3164); otherwise 400 `request_invalid`.
  - `Team { id: TeamId, space_id: SpaceId, name: String }` and `TeamView { team: Team, members: Vec<Dot> }`, with the lead first
  - `roles::default_roster(locale: Locale) -> Vec<NewDot>`: 6 Dots with English and Thai display names, for example Lead/หัวหน้าทีม, Researcher/นักวิจัย, Writer/นักเขียน, Analyst/นักวิเคราะห์, Scheduler/เลขา, Reviewer/ผู้ตรวจ. Each role gets a distinct avatar, and every Dot gets `tool_allowlist = ["read_thread", "ask_user"]`.
  - `dot::hire(pool, &Tenant, SpaceId, NewDot) -> Result<DotId, KatError>`: the Space becomes the Dot's default and its first grant
  - `dot::list(pool, &Tenant, SpaceId) -> Result<Vec<Dot>, KatError>`: the Dots granted to that Space
  - `dot::get(pool, &Tenant, DotId) -> Result<Dot, KatError>`
  - `dot::update(pool, &Tenant, DotId, DotPatch) -> Result<Dot, KatError>`
  - `dot::grant_space(pool, &Tenant, DotId, SpaceId) -> Result<(), KatError>` and `dot::revoke_space(pool, &Tenant, DotId, SpaceId) -> Result<(), KatError>`; revoking the default Space is 422 `default_space_required`, and revoking a Space while the Dot is an active member of a team in that Space is 422 `grant_in_use`
  - `policy_version` changes only for authority or safety. An effective change (a new value that differs from the stored one) to `tool_allowlist`, `status`, `autonomy` or `model_tier` (which can change residency), or a Space grant added or revoked, increments it once, in the same transaction. `max_concurrency`, `daily_token_budget`, `persona`, `instructions`, `name` and `appearance` never bump it: an edit to them applies from the Dot's next run, so it never aborts queued or running work.
  - Public helpers that take the caller's connection, for Tasks 8, 11 and 12. None of them takes a lock.
    - `dot::get_on<'e, E: Executor<'e, Database = Postgres>>(executor: E, &Tenant, DotId) -> Result<Dot, KatError>`: `dot::get` on any executor, a transaction included
    - `dot::is_granted(conn: &mut PgConnection, &Tenant, DotId, SpaceId) -> Result<bool, KatError>`: the grant only, whatever the Dot's status, so a caller can tell a paused Dot from a revoked grant
    - `dot::is_eligible(conn: &mut PgConnection, &Tenant, DotId, SpaceId) -> Result<bool, KatError>`: `active` status and the grant, read in one snapshot
    - `team::eligible_members(conn: &mut PgConnection, &Tenant, TeamId) -> Result<Vec<Dot>, KatError>`: the team's active members whose Dot is `active` and granted to the team's Space, lead first, then by `position`. Task 6 makes a thread's team always sit in the thread's Space, so this is the thread's eligible set.
  - Team membership is never deleted: `team::remove_member` sets `team_members.active = false`, every reader (`team::get`, the name checks, `eligible_members`, routing) filters on `active`, and `team::add_member` for an inactive row reactivates it.
  - Migration `0004_team_membership.sql`: `team_members.active BOOLEAN NOT NULL DEFAULT true`; the one-lead index becomes `UNIQUE (team_id) WHERE is_lead AND active`; `teams.is_default BOOLEAN NOT NULL DEFAULT false` with `UNIQUE (workspace_id, space_id) WHERE is_default`, which makes `team::create_default` idempotent per Space; `CHECK (daily_token_budget >= 0)` and `CHECK (max_concurrency <= 65535)`.
  - `team::create_default(pool, &Tenant, SpaceId, Locale) -> Result<TeamId, KatError>` and `team::get(pool, &Tenant, TeamId) -> Result<TeamView, KatError>`
  - `NewTeam { name: String, lead: DotId, members: Vec<DotId> }`, `team::create(pool, &Tenant, SpaceId, NewTeam) -> Result<TeamId, KatError>` and `team::list(pool, &Tenant, SpaceId) -> Result<Vec<Team>, KatError>`
  - `team::add_member(pool, &Tenant, TeamId, DotId, is_lead: bool) -> Result<(), KatError>`: a second lead is 422 `team_lead_conflict`; a Dot not granted to the team's Space is 422 `dot_not_in_space`
  - `team::remove_member(pool, &Tenant, TeamId, DotId) -> Result<(), KatError>`: removing the lead is 422 `team_lead_required`
- Routes:
  - Bodies are the serde JSON of the named structs, with snake_case fields.
  - `GET /api/workspaces/{workspace_id}/spaces/{space_id}/dots` → 200 `[Dot]`; `POST` ("hire") with `HireDot` → 201 `Dot`
  - `GET /api/workspaces/{workspace_id}/dots/{dot_id}` → 200 `Dot`; `PATCH` with `DotPatch` → 200 `Dot`
  - `PUT/DELETE /api/workspaces/{workspace_id}/dots/{dot_id}/spaces/{space_id}` (admin): grant and revoke → 204
  - `GET /api/workspaces/{workspace_id}/spaces/{space_id}/teams` → 200 `[Team]`; `POST` with `NewTeam` → 201 `TeamView`
  - `GET /api/workspaces/{workspace_id}/teams/{team_id}` → 200 `TeamView`
  - `PUT /api/workspaces/{workspace_id}/teams/{team_id}/members/{dot_id}` with `{"is_lead": bool}` → 204; `DELETE` → 204

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/team.rs`:
  - `default_team_has_one_lead_and_five_specialists`
  - `team_rejects_second_lead`: `PUT …/members/{dot_id}` with `is_lead: true` on a team that has a lead gives 422 `team_lead_conflict`
  - `hire_uses_role_template_defaults`
  - `member_cannot_change_execution_settings_admin_can`
  - `patch_daily_budget_omitted_null_and_number`: through the route, an omitted field keeps the cap, `null` clears it, and a number sets it
  - `duplicate_dot_name_in_team_is_rejected`: `"Writer"` and `"writer"` in one team give 422 `dot_name_conflict`
  - `hire_custom_dot_with_thai_name`
  - `team_persisted_server_side_and_returned_in_order`: the lead comes first
  - `default_roster_avatars_are_distinct`
  - `granting_and_revoking_space_bumps_policy_version`
  - `authority_edits_bump_policy_version_once`: changing `tool_allowlist`, `status`, `autonomy` or `model_tier` bumps it by exactly 1; re-sending the stored value bumps nothing
  - `capacity_budget_and_text_edits_do_not_bump_policy_version`: `max_concurrency`, `daily_token_budget`, `persona`, `instructions`, `name` and `appearance` edits leave it unchanged
  - `patch_cannot_change_default_space`: 400 `request_invalid`
  - `free_text_fields_reject_controls_and_overlong_values`: a NUL in `persona`, a 4,001-character `persona`, an 8,001-character `instructions` and a 201-character `appearance.accent` are each 400; `instructions` with line feeds and tabs is accepted
  - `tool_allowlist_entries_are_validated_sorted_and_deduplicated`: `["read_thread", "ask_user", "read_thread"]` is stored as `["ask_user", "read_thread"]`; `"Read_Thread"` and a 65-character entry are 400
  - `revoking_a_grant_in_use_is_422`: `grant_in_use` while the Dot is an active member of a team in that Space, allowed after `remove_member`
  - `name_uniqueness_uses_nfkc_casefold`: `Writer` and fullwidth `Ｗｒｉｔｅｒ` in one team give 422 `dot_name_conflict`
  - `names_need_a_letter_or_digit_and_no_invisible_code_points`: `"---"` and `"Writer\u{200B}"` are 400
  - `removed_member_is_soft_deleted_and_hidden`: the row stays with `active = false`, `team::get` omits it, and re-adding reactivates it
  - `malformed_path_ids_use_json_errors_and_execution_keys_check_role_first`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test team`. Expected: FAIL.
- [ ] **Step 3: Implement.** `seed-demo` also creates the default team in the demo Space.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: dots, teams, default roster`.

**Follow-up PR, after PR #24 and Task 3b merge** (no migration):

- [ ] **Step 6: Lead concurrency default.** Write `lead_defaults_to_three_concurrent_tasks_specialists_to_one` in `tests/team.rs`: the default roster's Lead has `max_concurrency = 3` and each specialist 1, and a hired `Custom` Dot has 1. Run it (FAIL). `NewDot` gains `max_concurrency: u16`; `roles::template` sets 3 for `DotRole::Lead` and 1 for every other role (initial defaults); `dot::hire` and `team::create_default` store it. `HireDot` cannot set it; an admin changes it with `PATCH`. Run it (PASS).
- [ ] **Step 7: Reply in the person's language.** Write `role_templates_tell_dots_to_reply_in_the_persons_language`: every role template's `instructions`, `Custom` included, end with `Reply in the language of the person's latest message.` for `Locale::En` and `ตอบเป็นภาษาเดียวกับข้อความล่าสุดของคนที่คุยด้วย` for `Locale::Th`. Run it (FAIL), add the line to `roles.rs`, run it (PASS). Existing Dots keep their instructions.
- [ ] **Step 8: Thai demo seed.** Write `seed_demo_creates_a_thai_workspace_with_the_thai_roster` in `tests/cli.rs`: `seed-demo` also gives the demo user a second workspace `เดโม`, with one Space `พื้นที่ทดลอง` (`allow_external = true`) and the default team from `roles::default_roster(Locale::Th)`, where หัวหน้าทีม comes first and the five specialists have Thai names; a second run creates nothing. Run it (FAIL), implement, run it (PASS).
- [ ] **Step 9: A default team for `create-user`'s new workspace.** Write `create_user_new_workspace_has_default_team` in `tests/cli.rs`: a workspace that `create-user` creates has its `General` Space with a default team, the Lead first and the five specialists from `roles::default_roster(Locale::En)` (the new account's locale), so the owner can start a thread at once; joining an existing workspace creates no team. Run it (FAIL). Add `team::create_default_on(conn: &mut PgConnection, &Tenant, SpaceId, Locale) -> Result<TeamId, KatError>`, the body of `team::create_default` on the caller's transaction, which `team::create_default` now wraps. Let `tenancy::resolve` take any `sqlx::PgExecutor` instead of `&PgPool`, so it can read a membership its own transaction inserted; it stays the only constructor of `Tenant`. `accounts::create_user` then resolves the new owner's `Tenant` on its transaction and calls `team::create_default_on` for `General` before it issues the invite, so the account, workspace, Space and team commit together. Run it (PASS).
- [ ] **Step 10: Commit** with `feat: lead concurrency default, reply-language line, thai demo seed, default team for new workspaces`.

### Task 6: Threads and messages (web channel)

**Files:**
- Create: `crates/katwork/src/chat/{mod,thread,message,mentions}.rs`, `crates/katwork/migrations/0005_chat_integrity.sql`, `crates/katwork/tests/mentions.rs`, `crates/katwork-server/src/routes/threads.rs`, `crates/katwork-server/tests/chat.rs`
- Modify: `Cargo.toml` and `crates/katwork/Cargo.toml` (`unicode-segmentation`; `pulldown-cmark` with `default-features = false`, MIT), `crates/katwork/src/{error.rs,team/mod.rs}`, `crates/katwork-server/src/{app.rs,error.rs,extract.rs}`, `crates/katwork/tests/schema.rs` (EXPLAIN tests), `crates/katwork-server/tests/team.rs`

**Interfaces:**
- Consumes: `Tenant`, `SpaceId`, `TeamId`, `DotId`, `JobId`, `EventSeq`, `types::{Question, JobStatus, ReasonCode}` (Tasks 2 and 4); Task 5's name key (`name_key` in `team/mod.rs`: NFKC, default case fold, NFKC, Thai above-marks in a fixed order, then NFC), its Dot and team name checks, and `roles::default_roster` (tests).
- Produces:
  - `Thread { id: ThreadId, space_id: SpaceId, team_id: TeamId, title: String }`
  - `thread::create(pool, &Tenant, SpaceId, TeamId, title: &str) -> Result<Thread, KatError>`: a Space or team outside the tenant's workspace, or a team outside that Space, is `NotFound`. `title` is trimmed; an empty title is stored as `""` (the web shows a localized placeholder, never a server-side default); a title over 120 Unicode scalar values or containing a Cc control character (NUL included) is 422 `thread_title_invalid`. Emoji sequences with U+200D stay allowed, so the Task 4 name check is not reused here.
  - `thread::get(pool, &Tenant, ThreadId) -> Result<Thread, KatError>`; another workspace's thread is `NotFound`
  - `thread::list(pool, &Tenant, SpaceId, limit: u16) -> Result<Vec<Thread>, KatError>`: newest first by `id`, `limit` clamped to 1..=100
  - `Author::{User(UserId), Dot(DotId), System}` (`Eq`, `Hash`), serialized as `{"kind": "user" | "dot" | "system", "id"}` with no `id` for `system`
  - `NoticeDot { id: DotId, name: String }` and `NoticeMeta { code: ReasonCode, dots: Vec<NoticeDot> }` (`Serialize`, `Deserialize`): the parameters of a `System` notice, stored in `messages.meta`. The notice's `body` stays its code, so a client that ignores `meta` still shows `reason.<code>`, and one that reads it can name the Dot.
  - `Message { id: MessageId, thread_id: ThreadId, author: Author, author_name: String, body: String, synthetic: bool, question: Option<Question>, meta: Option<NoticeMeta>, job_id: Option<JobId>, job_status: Option<JobStatus>, job_reason: Option<ReasonCode>, client_msg_id: Option<String>, created_at: DateTime<Utc> }`. `author_name` is resolved on the server: the user's `display_name`, the Dot's `name`, or `""` for `System`. `job_status` and `job_reason` come from a `LEFT JOIN jobs`; `job_status` is null while the job runs, so a reloaded page knows which jobs still offer Stop.
  - `NewMessage { id: Option<MessageId>, author: Author, body: String, client_msg_id: Option<String>, idempotency_key: Option<String>, synthetic: bool, question: Option<Question>, meta: Option<NoticeMeta>, job_id: Option<JobId> }`; `id: None` generates a new id, and `job_id` links the message to its job when it is inserted
  - `UserPost { body: String, client_msg_id: Option<String> }` (`Deserialize`, `#[serde(deny_unknown_fields)]`): the body of the post route, which Task 12's pipeline also takes
  - `message::validate_user_post(body: &str, client_msg_id: Option<&str>) -> Result<(), KatError>`, the post rules, checked in this order:
    - `body` blank after trimming is 422 `message_empty`; containing U+0000 is 422 `message_invalid`, because the insert would otherwise fail and answer 500
    - `body` over 32,768 UTF-8 bytes is 413 `message_too_large`: 10,922 Thai characters fit and 10,923 do not, against 32,768 ASCII characters. The JSON `\uXXXX` form of a body at the cap is about 65 KB, under Task 3's 1 MiB request limit, so it reaches this check. The body is stored as sent, untrimmed.
    - `client_msg_id`, when present: over 64 characters (counted in characters, as the database's `char_length` check counts, never in bytes) is 422 `client_msg_id_too_long`; empty, or holding anything but `[A-Za-z0-9._:-]`, is 422 `client_msg_id_invalid`, because an empty key would fold every later empty-key post of that author in that thread into the first
  - `message::append(conn: &mut PgConnection, &Tenant, ThreadId, NewMessage) -> Result<Appended, KatError>` and `Appended { message: Message, created: bool }`, where `message` is the stored row as `message::list` returns it. It is specified for Read Committed, the SQLx default. It runs `INSERT … ON CONFLICT DO NOTHING RETURNING id`, never `DO UPDATE`; when no row comes back, a separate statement selects the existing row by `(thread, author, client_msg_id)` or by `idempotency_key`:
    - the same key with the same `body`: that row, `created: false`
    - the same `client_msg_id` with a different `body`: 409 `client_msg_id_conflict`, and nothing is written
    - no row found (a primary-key collision on `NewMessage.id`): an error, never `created: false`

    A concurrent duplicate waits on the unique index, then gets the first row. It takes a connection so that intake (Task 12) and `job::conclude` (Task 10) can write inside their transactions.
  - `message::set_job(conn: &mut PgConnection, &Tenant, MessageId, JobId) -> Result<(), KatError>`
  - `MessagePage { messages: Vec<Message>, event_seq: EventSeq }` and `message::list(pool, &Tenant, ThreadId, before: Option<MessageId>, limit: u16) -> Result<MessagePage, KatError>`:
    - `event_seq` is read first, in its own statement: the thread's `last_event_seq`, or, while a run of the thread is still `running`, one less than the seq of the oldest such run's `RUN_STARTED` event (`events.kind = 'RUN_STARTED'`; Task 9's `log::append` stores the AG-UI `type` in `kind`). The client opens the event stream with `?after=<event_seq>` (Task 9), so a reload replays the runs still streaming. A message stored between the two statements arrives twice, which is harmless because `katwork.message_stored` replaces by id; the other order could lose it.
    - then the page: the newest `limit` messages by `id DESC`, with `id < before` when set, returned oldest to newest. `limit` above 200 is clamped to 200, and 0 is 400 `request_invalid`. Messages are ordered by `id` (UUIDv7, minted in one process), never by `created_at`, which is the transaction start and ties for every row one transaction writes.
  - `mentions::resolve(body: &str, names: &[(DotId, &str)]) -> Vec<DotId>`: the Dots `body` mentions, in text order, each once. This is the only mention parser: routing (Task 12) calls it, and no client parses mentions again.
    - The body is parsed as Markdown with `pulldown-cmark` and adjacent text merged with `TextMergeStream`; only `Event::Text` outside a code block is scanned, so fenced (backtick or tilde), indented and inline code, HTML and link destinations never mention anyone; an unclosed fence runs to the end of the message, as CommonMark defines.
    - An `@` starts a mention unless the scalar before it, in the same text node, is an ASCII letter or digit or one of `._%+-/@`, so `a@writer.com` and `https://x.test/@Writer` are not mentions. A Thai letter before it is allowed (`ถาม@Writer`), because Thai writers omit spaces.
    - After the `@`, the rest of the text node is split into grapheme clusters (`unicode-segmentation`), so Thai marks stay on their consonant (a per-character letter test cuts `นักวิเคราะห์` to `นักวิเคราะห`). For k from the largest grapheme-cluster count among `names` down to 1, the first k clusters match a name when their name key (Task 5's) equals that name's key and the mention ends after them. The longest match wins, so `@นักวิเคราะห์ ภาษี` names a Dot of that name, not `นักวิเคราะห์`.
    - A mention ends at the end of its text node, at whitespace (`char::is_whitespace`, so U+00A0 counts), at a Cf character such as U+200B, at punctuation (Unicode category P), or where the script changes between the Thai block U+0E00–U+0E7F and a letter or digit outside it (`@Writerช่วยสรุป` names `Writer`). A Thai name followed directly by another Thai letter is not a match: the roster's `เลขา` is a prefix of the ordinary word `เลขานุการ`. A client that inserts a mention puts a space after the name; dictionary segmentation of unspaced Thai after a Thai name is outside SP1.
  - Task 5's `name_key` becomes `pub(crate)` as `team::name_key(name: &str) -> String`, and Dot and team names reject `@` (400 `request_invalid`), because `@` starts a mention
  - `ApiJson<T>(pub T)` in `extract.rs`: `FromRequest<AppState>` with `Rejection = ApiError`. It maps every `JsonRejection` the way `extract::payload` does, 413 `body_too_large` over Task 3's 1 MiB limit and 400 `request_invalid` otherwise, so a malformed body, an unknown field or a wrong field type is always JSON `{code}`, never axum's plain-text rejection. Routes that check no role take bodies through it (Tasks 6 and 12). A route that must call `require_admin()` before parsing (Global Constraints, "Roles before bodies"), such as Task 13b's members routes, keeps reading the body as bytes and parses it with `extract::payload` after the check.
  - New `KatError` variants: `MessageTooLarge` (413 `message_too_large`), `MessageEmpty` (422 `message_empty`), `MessageInvalid` (422 `message_invalid`), `ClientMsgIdInvalid` (422 `client_msg_id_invalid`), `ClientMsgIdTooLong` (422 `client_msg_id_too_long`), `ClientMsgIdConflict` (409 `client_msg_id_conflict`) and `ThreadTitleInvalid` (422 `thread_title_invalid`)
  - Migration `0005_chat_integrity.sql`:
    - `ALTER TABLE teams ADD UNIQUE (id, space_id, workspace_id)` and `ALTER TABLE threads ADD FOREIGN KEY (team_id, space_id, workspace_id) REFERENCES teams (id, space_id, workspace_id)`. `0001`'s two independent foreign keys let a thread name a team from another Space, which would leave routing's eligible set (Dots granted to the thread's Space) silently empty.
    - `ALTER TABLE messages ADD COLUMN meta JSONB NULL`
    - indexes `messages_thread_id ON messages (thread_id, id)` (the message list and Task 12's history query), `threads_space_id ON threads (space_id, id DESC)` (the thread list) and `events_run_started ON events (thread_id, seq) WHERE kind = 'RUN_STARTED'` (the `event_seq` cursor)
- Routes. Bodies go through `ApiJson` into structs with `#[serde(deny_unknown_fields)]`, so a client can never set `author`, `synthetic`, `job_id` or any other server fact:
  - `POST /api/workspaces/{workspace_id}/threads` with `{space_id, team_id, title?}` → 201 `Thread`; a team outside that Space is 404
  - `GET /api/workspaces/{workspace_id}/spaces/{space_id}/threads?limit=` → 200 `[Thread]`, newest first, `limit` default 50
  - `GET /api/workspaces/{workspace_id}/threads/{thread_id}` → 200 `Thread`
  - `GET /api/workspaces/{workspace_id}/threads/{thread_id}/messages?before=&limit=` → 200 `{messages, event_seq}` (`MessagePage`), `limit` default 50
  - `POST /api/workspaces/{workspace_id}/threads/{thread_id}/messages` with `{body, client_msg_id?}`: `message::validate_user_post`, then `message::append`. The first post returns 201 `{message_id, job_id}`; a retry with the same `client_msg_id` and body returns 200 with the original `{message_id, job_id}`; the same `client_msg_id` with a different body is 409 `client_msg_id_conflict` and stores nothing. `job_id` is filled in by Task 12.

- [ ] **Step 1: Write the failing tests:**
  - in `crates/katwork/tests/mentions.rs`, with the names of `roles::default_roster` in both locales:
    - `mentions_resolve_english_and_thai`: `"@Writer ช่วยสรุป"` gives Writer; `"@นักวิเคราะห์ ดูไฟล์"` gives the Analyst
    - `mentions_roster_thai_names_survive`: for every Thai roster name N, `"@{N} ดูไฟล์"` and `"@{N},"` both give N's Dot
    - `mentions_ignore_code_and_trailing_punctuation`: `` "`@Writer` and @Analyst," `` gives only the Analyst
    - `mentions_ignore_every_code_form`: a `~~~` fence, a 4-space indented block, a double-backtick span, a fence holding three backticks and an unclosed fence each give none; a mention after a closed fence is found
    - `mentions_resolve_latin_name_then_thai_without_space`: `"@Writerช่วยสรุป"` gives Writer
    - `mentions_resolve_thai_prefix_is_not_a_match`: with the Dot `เลขา`, `"@เลขานุการ ดู"` gives none and `"@เลขา ดู"` gives `เลขา`
    - `mentions_email_and_url_are_not_mentions`: `"a@writer.com"` and `"https://x.test/@Writer"` give none; `"ถาม@Writer"` gives Writer
    - `mentions_zwsp_and_nbsp_end_the_token`: `"@Writer\u{200B}ช่วย"` and `"@Writer\u{00A0}ช่วย"` give Writer
    - `mentions_resolve_normalizes_case_and_mark_order`: a Dot stored as `Writer` matches `"@WRITER"`; a Dot stored as `กุ่` with U+0E38 U+0E48 matches text typed with U+0E48 U+0E38
    - `mentions_resolve_multi_word_name_longest_wins`: with Dots `นักวิเคราะห์` and `นักวิเคราะห์ ภาษี`, `"@นักวิเคราะห์ ภาษี ช่วยดู"` gives the second
    - `mentions_keep_text_order_and_drop_repeats`: `"@Analyst @Writer @Analyst"` gives `[Analyst, Writer]`
  - in `crates/katwork-server/tests/chat.rs`:
    - `post_and_list_messages_in_order`
    - `messages_list_pages_without_gaps_or_duplicates`: 120 messages written in 3 transactions, so many share `created_at`; pages of 50 by `before` reassemble the exact `id` order
    - `list_event_seq_is_read_before_messages`: while a loop lists the thread, another task commits a message and its event; every new message is either in the page or has its event's seq above `event_seq`, never neither
    - `list_event_seq_rewinds_to_the_oldest_running_run`: with `last_event_seq` 9 and a `running` run whose `RUN_STARTED` is seq 4 (rows inserted in SQL), `event_seq` is 3; once that run is `finished`, it is 9
    - `message_json_carries_author_name_and_job_status`: a user message shows its author's `display_name`; a message whose job is running has `job_status: null`, and one whose job ended shows its status and reason (job rows inserted in SQL)
    - `blank_body_is_422` and `nul_in_body_is_422_not_500`
    - `body_limit_counts_utf8_bytes`: 10,922 Thai characters give 201 and 10,923 give 413 `message_too_large`
    - `escaped_thai_body_at_the_cap_is_accepted`: the same 10,922 characters sent in JSON `\uXXXX` form (about 65 KB) give 201
    - `retry_with_same_client_msg_id_returns_original_ids`: one row is stored, and the retry returns 200 with the same `message_id`
    - `client_msg_id_blank_or_non_ascii_is_422`: `""` and `"ข้อความ"` are 422 `client_msg_id_invalid`; 65 characters are 422 `client_msg_id_too_long`
    - `client_msg_id_reuse_with_different_body_is_409_and_stores_nothing`
    - `malformed_json_and_unknown_fields_return_code_json`: `{"body":"x","author":"…"}` and a truncated JSON body are each 400 with the body `{"code":"request_invalid"}`, never plain text
    - `client_cannot_set_author_synthetic_or_job_id`: a post naming `synthetic`, `author` or `job_id` is 400 `request_invalid` and stores nothing
    - `thread_title_is_trimmed_bounded_and_nul_free`: `"  แผนงาน  "` is stored as `แผนงาน`, an omitted title as `""`, and 121 characters or a NUL are each 422 `thread_title_invalid`
    - `thread_list_is_newest_first_and_get_returns_one`
    - `thread_create_rejects_team_from_another_space`: 404, and a direct SQL insert of such a thread violates the new foreign key
    - `create_thread_with_other_workspaces_space_is_404`, `get_messages_of_other_workspace_thread_is_404` and `post_to_other_workspace_thread_is_404` (Review Focus 1)
    - `append_concurrent_duplicates_return_one_row`: 20 tasks append the same `(thread, author, client_msg_id)` and body; one row, one `created: true`, and the same id for all
    - `append_primary_key_collision_is_an_error`: a `NewMessage` whose `id` already exists in another thread is an error, never `created: false`
  - in `crates/katwork-server/tests/team.rs`: `dot_and_team_names_reject_at_sign`: hiring, renaming or creating a team with `@` in the name is 400 `request_invalid`
  - in `crates/katwork/tests/schema.rs`, each running `EXPLAIN` with `enable_seqscan = off` on the exact query the code runs and asserting the index name: `message_list_uses_thread_id_index`, `thread_list_uses_space_index` and `event_seq_cursor_uses_run_started_index`
- [ ] **Step 2: Run** `cargo test -p katwork --test mentions --test schema && cargo test -p katwork-server --test chat --test team`. Expected: FAIL.
- [ ] **Step 3: Implement** the migration, `chat/`, `ApiJson`, the routes, the new `KatError` variants and their `ApiError` statuses, and the two `team/mod.rs` changes.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: threads and messages`.

### Task 7: Model gateway, FakeProvider and Rig adapter

**Status:** merged in PR #21. The interfaces below match its code; Task 7b adds the local provider.

**Files:**
- Create: `crates/katwork/src/models/{mod,provider,registry,guard,fake,rig_adapter}.rs`, `crates/katwork/tests/{models.rs,rig_fixtures.rs}`, `crates/katwork/tests/fixtures/models/` (recorded provider bytes)
- Modify: `Cargo.toml`, `Cargo.lock`, `crates/katwork/src/{runtime.rs,config.rs}`, `crates/katwork-server/src/main.rs`

**Interfaces:**
- Consumes: `Config`, `ProviderEnv` (Task 1); `types::{ModelTier, SpacePolicy}` and `Core` (Task 2). Nothing from Tasks 3–6.
- Produces:
  - `#[async_trait] trait ModelProvider: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> Capabilities; async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError>; }`
  - `ChatRequest { system: String, messages: Vec<ChatMsg>, tools: Vec<ToolSpec>, max_output_tokens: u32, temperature: Option<f32> }` (`Serialize`).
  - `ChatRequest::input_charge_bound(&self) -> u64`: the byte length of the whole request serialized as JSON, plus `TOOL_PROMPT_OVERHEAD_TOKENS = 1024` whenever `tools` is non-empty (initial default; a fixed allowance that covers the implicit tool system prompt Anthropic adds, and OpenAI's as well). The JSON covers system text, every message and every tool schema, with their JSON framing. Tokenizers that work on UTF-8 bytes (byte-level BPE, and byte-fallback tokenizers) emit at most one token per byte, so this is an upper bound on input tokens even for emoji and Thai, where one character can be several tokens; the JSON framing of each message is longer in bytes than the few framing tokens a provider adds. The model table in `registry.rs` lists only models whose tokenizer has this property.
  - System text lives only in `ChatRequest.system`; there is no system role.
  - `enum ChatMsg { User { content: String, name: Option<String> }, Assistant { content: String, tool_calls: Vec<ToolCall>, replay: Option<ProviderReplay> }, Tool { tool_call_id: String, name: String, content: String, is_error: bool } }` (`Serialize`, `Deserialize`)
  - `ToolCall { id: String, name: String, args_json: String }`
  - `ProviderReplay { provider_id: String, version: u32, data: serde_json::Value }`: opaque, versioned provider state, passed back only to the same provider in the same workspace and thread
  - `ToolSpec { name: String, description: String, parameters: serde_json::Value }`, where `parameters` is JSON Schema
  - `ChatChunk::{TextDelta(String), Progress, ToolCall(ToolCall), Usage(Usage), Done { stop: StopKind, turn: AssistantTurn }}`. `Progress` is content the loop does not use: a part starting or ending, a reasoning delta, or a non-empty tool-argument delta. The adapter emits it so the stall timers see that content, and `guard_stream` forwards it, so the caller knows content has started. Once any content chunk (a non-empty `TextDelta`, `Progress`, `ToolCall` or `Done`) has reached the caller, the attempt is not retryable: Task 7 never retries (Rig's retries and the HTTP client's are off), and Task 8 owns every retry. Text deltas are provisional. `ToolCall` chunks are emitted only after the whole turn is accepted. `turn` is the canonical final turn, never joined deltas.
  - `AssistantTurn { content: String, tool_calls: Vec<ToolCall>, replay: Option<ProviderReplay> }`
  - `StopKind::{EndTurn, ToolUse, MaxTokens, Refusal, Other(String)}`
  - `Usage { input: Option<u64>, cache_read: Option<u64>, cache_write: Option<u64>, output: Option<u64>, complete: bool }`. Cache counters are subsets of `input`, not extra. A missing count is `None`, never 0.
  - `ModelError::{ProviderUnavailable, ResidencyDenied, Timeout { phase: TimeoutPhase }, RateLimited { retry_after_secs: Option<u64> }, QuotaExceeded { resets_at: Option<DateTime<Utc>> }, AuthFailed, Protocol(String), Other(String)}` and `TimeoutPhase::{FirstChunk, Idle}`. `Protocol` and `Other` hold private diagnostics only; the provider's raw error body never reaches a tenant, and never reaches a log at any level. `ModelError`'s `Display` is its stable code (`model_protocol_error`, never the diagnostic string), and `ModelError::reason_code(&self) -> ReasonCode` gives each variant one stable `ReasonCode`. The adapter's only log line for a provider error is `model_provider_error`, with `error_kind` (that code), `status`, `provider_request_id` and `error_body_bytes`.
  - `StreamTimeouts { first_chunk: Duration, idle: Duration }`, default 30 s each: `first_chunk` is the time to the first content chunk, and `idle` the longest gap between content chunks
  - `guard::guard_stream(inner: BoxStream<'static, Result<ChatChunk, ModelError>>, t: StreamTimeouts) -> BoxStream<'static, Result<ChatChunk, ModelError>>`. It times the actual stream polling, not only the opener: a stall yields `Timeout { phase }`. **Only content counts as progress:** a non-empty `TextDelta`, `Progress`, `ToolCall` or `Done`. Raw HTTP bytes, SSE comments, blank lines, empty deltas, unknown events, Anthropic `ping` and `message_start`, and usage-only frames (`Usage`) never reset either timer, so a stream that keeps the connection alive without content still times out. A stream that ends without `Done` yields `Protocol`, and so does `Done { stop: ToolUse }` with no tool call.
  - `Capabilities { tools: bool, residency: Residency, synthetic: bool }` and `Residency::{Local, External}`
  - `TokenPrice { input_micros_per_mtok: u64, output_micros_per_mtok: u64 }`
  - `ModelEntry { provider_id: String, model: String, tier: ModelTier, capabilities: Capabilities, max_output_tokens: u32, temperature: Option<f32>, price: Option<TokenPrice> }`. Capabilities and residency come from the entry, never from the provider name or base URL. SP1 configures no prices, so `model_calls.cost_micros` stays NULL; cost is only ever computed from usage the provider reported.
  - `Selected { provider: Arc<dyn ModelProvider>, entry: ModelEntry }`
  - `Registry::from_config(&Config) -> Registry` and `Registry::from_entries(Vec<(ModelEntry, Arc<dyn ModelProvider>)>) -> Registry`
  - `Registry::for_tier(&self, tier: ModelTier, space_policy: &SpacePolicy) -> Result<Selected, ModelError>`
  - `FakeTurn::{Chunks(Vec<ChatChunk>), Error(ModelError), Hang, ChunksThenHang(Vec<ChatChunk>)}`
  - `FakeProvider::scripted(Vec<FakeTurn>) -> FakeProvider` (`Clone`; each `stream` call takes the next turn) and `FakeProvider::received(&self) -> Vec<ChatRequest>`, the requests it got. Only scripted mode records requests.
  - `FakeProvider::demo() -> FakeProvider`: canned English and Thai replies, `Local`, with `capabilities().synthetic == true`. It records no requests, because it serves real keyless installs.
  - `HttpLimits { connect: Duration, read: Duration, max_response_bytes: usize, max_error_body_bytes: usize, max_sse_frame_bytes: usize }`, with `HttpLimits::default()` = 10 s, 30 s, 16 MiB, 64 KiB and 1 MiB (initial defaults), and `BoundedHttp::new(limits: HttpLimits) -> Result<BoundedHttp, ModelError>`
  - `OpenAiApi::{Chat, Responses}`, `RigProvider::openai(env: &ProviderEnv, model: &str, api: OpenAiApi) -> Result<RigProvider, ModelError>` and `RigProvider::anthropic(env: &ProviderEnv, model: &str) -> Result<RigProvider, ModelError>`
  - `Core` gains `registry: Arc<Registry>`, built in `Core::new` with `Registry::from_config`, and `Core::with_registry(self, registry: Registry) -> Core` for tests
- Registry rules:
  - `from_config` registers each configured provider as `External`: OpenAI through `OpenAiApi::Chat` (chosen explicitly, never detected) and Anthropic, with one model id per tier from a constant table in `registry.rs` (initial defaults, checked at Task 7 start). The same table sets `max_output_tokens` to 2,048 (Fast), 4,096 (Standard) and 8,192 (Strong), and `temperature: None`, meaning the provider's default (initial defaults).
  - `from_config` never fails. A provider whose constructor returns an error is left out and logged once as `provider_init_failed` with the provider id only (no key, no URL); its tiers then resolve to `ProviderUnavailable`, never to the demo provider.
  - Task 7 also tightens `Config::from_lookup`: `OPENAI_BASE_URL` and `ANTHROPIC_BASE_URL`, when set, must parse as `http` or `https` URLs, otherwise `config_invalid` at startup.
  - With no provider keys and `demo`, every tier maps to `FakeProvider::demo()`. With no keys and no demo, `for_tier` returns `ProviderUnavailable`.
  - An `External` entry for a Space with `allow_external = false` is `ResidencyDenied`.
- Rig adapter rules (ADR-0001), all inside `rig_adapter.rs`; no Rig type leaves that file:
  - only Rig's completion and streaming clients; no `AgentRunner`, `ToolServer`, hooks or typed-output retries
  - tools advertised as `ToolDefinition` only
  - hosted tools, server-side fallbacks, raw `additional_params` and hidden continuation stay off
  - all HTTP goes through KatWork's `BoundedHttp`, injected through Rig's HTTP client interface (the trait Rig's own reqwest transport implements). reqwest supplies the connect and read timeouts and the no-redirect policy. `BoundedHttp` reads every body chunk by chunk and applies the byte caps before buffering or decoding: an over-cap response is `Protocol("response_too_large")`; an error body is cut at its cap before Rig reads it; an SSE frame that passes its cap without a terminating blank line is `Protocol("sse_frame_too_large")`. If the pinned Rig release has no injectable HTTP client interface, stop and revise this task; never fall back to an unbounded client.
  - provider bytes are decoded with an incremental UTF-8 decoder, never `String::from_utf8` per network chunk
  - Anthropic's opening and closing usage snapshots are merged, never summed
  - Anthropic spend-cap and credit errors map to `QuotaExceeded { resets_at: None }`, not `RateLimited`: a 429 `rate_limit_error` whose `details.error_code` is `enforced_spend_limit_reached`, a 400 `invalid_request_error` whose message starts with the organization usage-limit, workspace usage-limit or low-credit-balance text, and the codes `insufficient_quota`, `quota_exceeded`, `billing_error` and `credit_balance_too_low`. Every other error keeps its own mapping; an authentication error (401, 403, `authentication_error`, `permission_error`, `invalid_api_key`) is checked first and stays `AuthFailed`.
  - a provider tool call with no id gets `kw_call_{index}`; a duplicate id within one turn is `Protocol`
  - the OpenAI Responses route sends `store: false`, so the provider keeps no stored copy of the conversation
  - on both OpenAI routes, a `ChatMsg::Tool` with `is_error: true` is sent with the text prefix `Error: `, because neither the Chat nor the Responses encoder has an error field

- [ ] **Step 1: Pin dependencies.** Pin `rig-core = "=0.45.0"` with `default-features = false` and explicit reqwest and rustls features. Pin it explicitly: under resolver 3, which honours `rust-version`, an unpinned add silently resolves to 0.42.0, the newest release that fits the 1.94.0 floor. rig-core 0.45.0 and its sub-crates need Rust 1.97.1, so the same commit raises `rust-version` in `Cargo.toml` and the CI `msrv` job's toolchain to 1.97.1. If the release moves its reqwest transport into a separate crate, pin that crate to the same exact version. Add `reqwest` with the same version requirement that transport declares (for example `0.13`), never a different line, so `Cargo.lock` resolves one reqwest. Then re-run the `msrv` check on the new locked graph at 1.97.1. Add `async-trait`, `futures` and `tracing-subscriber`. In `katwork-server`, initialize tracing from `RUST_LOG`, defaulting to `info,rig=info`, and add the `filter_fn` layer `dependency_log_allowed`, which drops every event whose target starts with `rig` (and with `sqlx`) whatever `RUST_LOG` says: Rig logs full prompts at TRACE and provider messages even at WARN (Global Constraints). Test `rig_trace_prompts_are_blocked_even_with_explicit_trace_directives` (a unit test in `main.rs`): with `RUST_LOG=trace,rig::completions=trace`, events on `rig::completions`, `rig_core::completions`, `rig_core::providers` and `rig::providers` produce no output.
- [ ] **Step 2: Write the failing tests** in `crates/katwork/tests/models.rs`:
  - `registry_refuses_external_provider_for_local_only_space`: returns `ModelError::ResidencyDenied`
  - `invalid_provider_base_url_is_config_invalid`
  - `provider_init_failure_resolves_to_provider_unavailable`
  - `demo_mode_uses_synthetic_provider_without_keys`: no API keys plus `KATWORK_DEMO=1` gives `FakeProvider::demo`
  - `no_keys_and_no_demo_is_provider_unavailable`
  - `stream_stall_returns_model_timeout`: `FakeTurn::Hang` gives `Timeout { phase: FirstChunk }` and `ChunksThenHang` gives `Timeout { phase: Idle }`, with the timeouts injected small
  - `stream_without_done_is_protocol_error`
  - `input_charge_bound_covers_multibyte_text`: for `"👋"` and `"ฯ"` repeated, the bound is at least the UTF-8 byte count, which exceeds their published token counts
  - `input_charge_bound_includes_tool_schemas`: adding a 20 KB tool schema raises the bound by at least 20 KB
  - `input_charge_bound_adds_tool_prompt_overhead_only_with_tools`: `TOOL_PROMPT_OVERHEAD_TOKENS` is added when `tools` is non-empty and not otherwise
  - `demo_provider_records_no_requests`
  - `tool_use_stop_without_call_is_protocol_error`
- [ ] **Step 3: Write the failing adapter fixture tests** in `crates/katwork/tests/rig_fixtures.rs`. Each serves recorded provider bytes from `tests/fixtures/models/` through a local HTTP server, offline, with the adapter's base URL pointed at it:
  - `thai_split_mid_codepoint_across_http_chunks_decodes`
  - `openai_chat_usage_only_tail_is_recorded`
  - `openai_responses_final_text_replaces_deltas`
  - `completed_tool_call_then_response_failed_is_not_executable`
  - `absent_finish_reason_is_protocol_error`
  - `interleaved_argument_fragments_assemble_per_call`
  - `malformed_and_unfinished_calls_are_protocol_errors`
  - `several_calls_in_one_turn_keep_order`
  - `missing_call_id_is_assigned_and_duplicate_in_turn_is_protocol_error`
  - `anthropic_usage_snapshots_merge_not_sum`
  - `anthropic_error_results_map_to_model_errors`
  - `cancel_during_network_wait_returns_promptly`
  - `unknown_events_are_ignored`
  - `ping_only_stream_times_out_first_chunk_at_30s`: Anthropic `ping` events and nothing else; paused tokio clock
  - `comment_only_stream_times_out_first_chunk_at_30s`: SSE `:` comments and nothing else; paused tokio clock
  - `pings_after_text_time_out_idle`: text, then only pings; `Timeout { phase: Idle }` 30 s after the last text
  - `responses_route_sends_store_false`: the fixture server checks the request body
  - `openai_tool_error_results_carry_error_prefix`: on both the Chat and the Responses route
  - `replay_round_trips_to_the_same_provider`
  - `oversized_response_is_rejected_before_decoding`
  - `oversized_error_body_is_capped_before_rig_reads_it`
  - `unterminated_sse_frame_over_cap_is_rejected`
  - `content_then_connection_failure_is_not_retried_or_double_charged` and `tool_progress_then_provider_failure_is_not_retried_or_double_charged`: the adapter makes one HTTP request and yields the content, then the error
  - `anthropic_spend_cap_maps_to_quota`, `anthropic_organization_spend_limit_maps_to_quota`, `anthropic_workspace_spend_limit_maps_to_quota`, `anthropic_credit_balance_maps_to_quota` and `anthropic_quota_matching_preserves_other_errors`
  - `live_openai_chat_smoke` (`#[ignore]`): runs only when `OPENAI_API_KEY` is set, never in CI
- [ ] **Step 4: Run** `cargo test -p katwork --test models --test rig_fixtures`. Expected: FAIL.
- [ ] **Step 5: Implement.**
- [ ] **Step 6: Run.** Expected: PASS.
- [ ] **Step 7: Commit** with `feat: model gateway with residency policy, fake and rig providers`.

### Task 7b: Local model provider

A small follow-up PR to PR #21 (merged); it must merge before Task 8, which reads the per-entry timeouts. It lets an operator run every tier on one local OpenAI-compatible server, so a local-only Space works outside demo mode (ADR-0005). It adds no migration.

**Files:**
- Modify: `crates/katwork/src/config.rs`, `crates/katwork/src/models/{registry.rs,rig_adapter.rs,guard.rs}` (`HttpLimits.first_byte`, the per-entry `BoundedHttp`), `crates/katwork-server/src/main.rs`, `crates/katwork/tests/{config.rs,models.rs,rig_fixtures.rs}`, `crates/katwork/tests/fixtures/models/` (a local chat fixture), `.env.example`, `docker-compose.yml`

**Interfaces:**
- Consumes: `Registry`, `ModelEntry`, `Capabilities`, `Residency`, `RigProvider`, `OpenAiApi`, `BoundedHttp`, `StreamTimeouts` (Task 7); `Config` (Task 1).
- Produces:
  - `LocalEnv { base_url: String, model: String, api_key: Option<String> }` and `Config.local: Option<LocalEnv>`, read from `KATWORK_LOCAL_BASE_URL`, `KATWORK_LOCAL_MODEL` and the optional `KATWORK_LOCAL_API_KEY`. The base URL and model come together: either one without the other is `config_invalid` naming the missing variable (Task 3b's `ConfigInvalid { var }`; whichever PR merges second adapts). The base URL must parse as an `http` or `https` URL with a host, for example `http://local-llm:8000/v1`. `Debug` shows only the URL's origin and prints `[redacted]` for the key.
  - `ModelEntry` gains `timeouts: ModelTimeouts`, with `ModelTimeouts { first_chunk: Duration, idle: Duration, run_wall_clock: Duration }` and `ModelTimeouts::stream(&self) -> StreamTimeouts` (the first two, for `guard_stream`). Hosted and demo entries use `ModelTimeouts::HOSTED` (30 s, 30 s, 90 s); local entries use `ModelTimeouts::LOCAL` (120 s, 60 s, 300 s). All are initial defaults.
  - Each entry's timeouts reach the transport. `HttpLimits` gains `first_byte: Duration` (default 30 s), and `HttpLimits::for_entry(t: &ModelTimeouts) -> HttpLimits` gives `first_byte = t.first_chunk` and `read = max(t.first_chunk, t.idle)`, with the default connect timeout and byte caps: 120 s and 120 s for a local entry, 30 s and 30 s for a hosted one. The transport is only a safety net. `BoundedHttp` never enforces a content rule: it bounds only raw socket silence, waiting at most `first_byte` for the response headers and at most `read` between any two body chunks (comments and pings included), and reqwest's own `read_timeout` is set to `read`. Each of these bounds is at least every content deadline of the entry, so the HTTP layer never cuts a stream that `guard_stream`'s content-based first-chunk and idle deadlines would still allow. The provider constructors gain a `limits: HttpLimits` argument, `RigProvider::openai(env, model, api, limits)` and `RigProvider::anthropic(env, model, limits)`, and `from_config` builds a `BoundedHttp` for each entry from `HttpLimits::for_entry(&entry.timeouts)`. A local entry therefore never inherits the hosted 30 s read timeout.
  - `RigProvider::local(env: &LocalEnv, limits: HttpLimits) -> Result<RigProvider, ModelError>`: Rig's OpenAI Chat Completions client (`OpenAiApi::Chat`) pointed at `env.base_url`, through that `BoundedHttp` like the hosted adapters, with every Task 7 adapter rule. Its provider id is `local`. Without `api_key`, requests carry no `Authorization` header.
  - Registry rules:
    - `from_config` registers the local provider after the hosted ones: one entry per tier, each with `model = KATWORK_LOCAL_MODEL`, `Capabilities { tools: true, residency: Residency::Local, synthetic: false }`, the tier's `max_output_tokens`, `temperature: None`, `price: None` and `ModelTimeouts::LOCAL`.
    - `for_tier` already returns the first registered entry the Space's policy allows. So a Space with `allow_external = true` gets the hosted entry when one is configured, and the local entry otherwise; a local-only Space always gets the local entry.
    - A configured local provider counts as a provider: the demo fallback applies only when OpenAI, Anthropic and local are all unset.
    - A local constructor error is logged once as `provider_init_failed` with `provider_id = "local"` only, and the local entries are left out.
    - Residency comes from the variable that configured the provider, never from the URL. KatWork does not check that `KATWORK_LOCAL_BASE_URL` points inside the install's network; the operator declares it (ADR-0005).
  - `Registry::log_tiers(&self)`: one `info` event `model_tier` per tier, with `tier`, `hosted_allowed` (the `provider_id/model` a Space with `allow_external = true` gets, or `none`) and `local_only` (the same for a local-only Space), and never a key or URL. `serve` calls it once, after building `Core`.
  - Task 8 takes each attempt's stream timeouts from `selected.entry.timeouts.stream()`, capped by the run's remaining time like every call, and the run's wall clock from the `timeouts.run_wall_clock` of the entry its first attempt selects.
- `.env.example` documents the three variables; `docker-compose.yml` passes them to the `katwork` service.

- [ ] **Step 1: Write the failing tests:**
  - in `crates/katwork/tests/config.rs`: `local_base_url_and_model_must_be_set_together`, `local_base_url_must_be_http_or_https`, `local_api_key_is_redacted_in_debug`
  - in `crates/katwork/tests/models.rs`:
    - `local_only_space_uses_local_provider_without_demo`: only `KATWORK_LOCAL_*` set, demo off; `for_tier` on a local-only Space gives the `local` entry for every tier
    - `hosted_allowed_space_prefers_hosted_entry_over_local`: OpenAI and local both set; a Space with `allow_external = true` gets `openai`, a local-only Space gets `local`
    - `local_provider_disables_demo_fallback`: local set and `KATWORK_DEMO=1`; no tier maps to the demo provider
    - `local_entries_carry_local_timeouts`: 120 s, 60 s and a 300 s run wall clock on local entries; 30 s, 30 s and 90 s on hosted and demo ones
    - `local_init_failure_resolves_to_provider_unavailable`
    - `startup_logs_one_line_per_tier`: three `model_tier` events naming the serving entries, with no key or URL in the captured output
  - in `crates/katwork/tests/rig_fixtures.rs`, each through a real local HTTP fixture server:
    - `local_provider_streams_from_an_openai_compatible_server` and `local_provider_without_key_sends_no_authorization_header` (the fixture server checks the request headers)
    - `local_entry_http_read_timeout_is_not_30s`: paused tokio clock; the server sends its response headers after 45 s, and in a second case sends text, stays silent for 45 s, then sends the rest. Through a local entry's `BoundedHttp`, both streams complete; through a hosted entry's limits, the same server gives `Timeout`
    - `transport_gap_never_preempts_first_content_deadline`: paused tokio clock; on a local entry, the server sends its response headers and an SSE comment at 0 s and the first text at 90 s; the stream succeeds, because comments are not content and 90 s is inside both the 120 s first-content deadline and the 120 s transport gap
    - `http_limits_follow_the_entry_timeouts`: `HttpLimits::for_entry(&ModelTimeouts::LOCAL)` gives a 120 s first byte, a 120 s read, the 10 s connect and the default byte caps; `HttpLimits::for_entry(&ModelTimeouts::HOSTED)` gives 30 s and 30 s, and for every entry `read >= max(first_chunk, idle)`
- [ ] **Step 2: Run** `cargo test -p katwork --test config --test models --test rig_fixtures`. Expected: FAIL.
- [ ] **Step 3: Implement**, including the `.env.example` and `docker-compose.yml` changes.
- [ ] **Step 4: Run** `cargo test --workspace --locked`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat: local openai-compatible model provider`.

### Task 9: Event log, hub and AG-UI SSE

This task comes before Task 8, because the agent loop emits through `EventSink`.

**Files:**
- Create: `crates/katwork/src/events/{mod,agui,log,hub,sink,coalesce}.rs`, `crates/katwork/tests/events.rs`, `crates/katwork/tests/fixtures/agui/schema.json` (the AG-UI protocol 1.0 JSON Schema, MIT, vendored unchanged with its licence text as `tests/fixtures/agui/LICENSE`), `crates/katwork/tests/fixtures/agui/spec/<Event>/valid/*.json` (the protocol's own valid fixtures for the 11 emitted event types), `crates/katwork/tests/fixtures/agui/katwork/` (one fixture per `katwork.*` payload), `crates/katwork-server/src/routes/events.rs`, `crates/katwork-server/tests/events.rs`, `crates/katwork-server/tests/fixtures/sse/thai_emoji.sse` (a byte-for-byte copy of Task 13a's `web/src/testing/fixtures/thai_emoji.sse`)
- Modify: `Cargo.toml` and `crates/katwork/Cargo.toml` (`tokio-util`; `jsonschema`, MIT, as a dev-dependency), `crates/katwork/src/{runtime.rs,config.rs,db.rs,error.rs}`, `crates/katwork-server/src/{main.rs,app.rs,guards.rs,transport.rs,error.rs}`, `crates/katwork/tests/config.rs`, `crates/katwork-server/tests/transport.rs`, `.env.example` (`KATWORK_LOCK_TIMEOUT_MS`), `NOTICE` (the vendored schema, a test fixture only)

**Interfaces:**
- Consumes: `Tenant`, `tenancy::resolve` (Task 4); `session::authenticate` (Task 3); `thread::get`, `Author`, `Message`, `NoticeMeta`, `MessagePage.event_seq` (Task 6); `unicode-segmentation` (added by Task 6); ids, `types::{SideEffect, ReasonCode, JobStatus, HoldReason, Question}`, `fence::{Fence, check, lock}`, `Core`.
- Produces:
  - `enum AgUiEvent`: KatWork's own serde types for AG-UI protocol 1.0, covering only the events KatWork emits (ADR-0003). Serialized with `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]` and camelCase fields. Variants: `RunStarted{threadId,runId,protocolVersion}`, `TextMessageStart{messageId,role}`, `TextMessageContent{messageId,delta}`, `TextMessageEnd{messageId}`, `ToolCallStart{toolCallId,toolCallName,parentMessageId}`, `ToolCallArgs{toolCallId,delta}`, `ToolCallEnd{toolCallId}`, `ToolCallResult{messageId,toolCallId,content}`, `RunFinished{threadId,runId}`, `RunError{message,code}`, `Custom{name,value}`. `protocolVersion` is always `"1.0"`. `RunError.message`, which the schema requires, always equals `code`: provider text never goes there.
  - Every serialized event validates against the vendored AG-UI 1.0 schema, whose objects are closed, so a stray field fails. The thread stream as a whole is KatWork's own stream built from those shapes, not a conformant AG-UI run stream: runs overlap in one thread, `TEXT_MESSAGE_START` restarts a message's text, `katwork.*` events also come from intake outside any run, and cancel is `RUN_ERROR{code: "cancelled"}`. Stock AG-UI clients are not supported on it (ADR-0003, amended).
  - Ids: `threadId` and `runId` are KatWork's. `toolCallId` is the `ToolCallRecordId`, so a provider id reused in a later step stays distinct. `TextMessageStart.messageId` equals the stored reply's `MessageId`. `TOOL_CALL_END` means the arguments are complete; `TOOL_CALL_RESULT` means execution is complete.
  - `KatworkEvent`, serialized as `Custom{name,value}` through `From<KatworkEvent> for AgUiEvent`. Each payload is a serde struct with camelCase fields, typed only with Task 2 and Task 6 types:
    - `katwork.dot_state`: `DotStateEvent { dot_id: DotId, state: DotState }`, with `DotState::{Idle, Working, Researching, Waiting, NeedsApproval, Error, Paused}`
    - `katwork.job_status`: `JobStatusEvent { job_id: JobId, status: Option<JobStatus>, reason: Option<ReasonCode>, hold_reason: Option<HoldReason> }`; `status` is null while the job runs
    - `katwork.decision`: `DecisionEvent { job_id: JobId, inputs: serde_json::Value, verdict: serde_json::Value, policy_version: u32, thresholds: serde_json::Value }`; Task 10's `policy::decision_event` owns the conversion and defines the shapes
    - `katwork.tool_status`: `ToolStatusEvent { tool_call_id: ToolCallRecordId, ok: bool, code: Option<String> }`
    - `katwork.usage`: `UsageEvent { run_id: RunId, dot_id: DotId, model_call_id: ModelCallId, input: Option<u64>, output: Option<u64>, complete: bool }`
    - `katwork.message_stored`: `MessageStoredEvent { message_id: MessageId, author: Author, author_name: String, body: String, synthetic: bool, question: Option<Question>, meta: Option<NoticeMeta>, job_id: Option<JobId>, client_msg_id: Option<String>, created_at: DateTime<Utc> }`, built with `MessageStoredEvent::from(&Message)` and emitted in the transaction that commits the message: user messages (Task 12), Dot replies and notices (Tasks 10 and 12). `client_msg_id` lets the posting tab match the event to its optimistic bubble even when the event arrives before its own POST response.
    - `katwork.run_started`: `RunStartedEvent { run_id: RunId, job_id: JobId, dot_id: DotId, reply_message_id: MessageId }`, emitted right after `RUN_STARTED`, so the client knows which provisional message belongs to which run, and which job's run has started (the chat clears that job's queue position, Task 13b). Task 13a's reducer ignores fields it does not use, so adding `jobId` needs no 13a change.
    - `katwork.run_closed`: `RunClosedEvent { run_id: RunId, end: RunEnd, code: Option<String> }`, appended in the same transaction **immediately before** every `RUN_FINISHED` or `RUN_ERROR`, so each run's terminal event is its last event. `code` is the `RUN_ERROR` code (`cancelled`, `lease_lost`, …), `None` before `RUN_FINISHED`; AG-UI's `RUN_ERROR` carries no run id, so the client reads the code from here.
  - Client folding rules, which Task 13a implements: a `TEXT_MESSAGE_START` for a `messageId` starts that message's provisional text over; `TEXT_MESSAGE_CONTENT` appends to it; `katwork.message_stored` is authoritative and replaces whatever the client holds for that `messageId`, with the stored body, `synthetic` flag and question. On `katwork.run_closed {runId, code}`, the client drops that run's provisional message (the `replyMessageId` from `katwork.run_started`) unless `katwork.message_stored` confirmed it. It never infers the run from `RUN_ERROR`, which carries no run id, so concurrent runs in one thread stay apart, so text from a run that lost its lease or failed never stays beside the stored transcript. The rules fold the event log deterministically, so a reconnect that replays from any seq ends at the same transcript.
  - `DotState::for_tool(SideEffect) -> DotState`: `Read`, `WorkspaceRead` and `Navigate` give `Researching`; every other effect gives `Working`. Only the harness emits `dot_state`: `Working` while the model streams, `NeedsApproval` on an approval stop, `Waiting` on a question, `Error` on a run error, `Paused` for a paused Dot, `Idle` otherwise.
  - `log::append(tx: &mut Transaction<'_, Postgres>, workspace_id: WorkspaceId, thread_id: ThreadId, run_id: Option<RunId>, e: &AgUiEvent) -> Result<EventSeq, KatError>`: runs `UPDATE threads SET last_event_seq = last_event_seq + 1 WHERE id = $1 RETURNING last_event_seq` and then the insert, inside the caller's transaction, so appenders serialize per thread. The `UPDATE` takes the thread lock; a caller that also needs job or task locks has taken them already, and one that needs the Dot row takes it afterwards (global lock order). `events.kind` is the event's AG-UI `type` (`RUN_STARTED`, `TEXT_MESSAGE_CONTENT`, …), or its `name` for a `CUSTOM` event (`katwork.run_started`); Task 6's `event_seq` cursor reads `kind = 'RUN_STARTED'`. Before the insert it serializes the event to a `serde_json::Value` and replaces every U+0000 with U+FFFD in its strings, because `jsonb` rejects `\u0000` and a model delta holding one would otherwise abort the run. `jsonb` also does not keep key order, so tests compare parsed JSON, never bytes.
  - `log::since(pool, &Tenant, ThreadId, after: EventSeq, limit: u32) -> Result<Vec<(EventSeq, AgUiEvent)>, KatError>`: at most `limit` events with `seq > after`, in seq order
  - `log::tail(pool, &Tenant, ThreadId) -> Result<EventSeq, KatError>`: the thread's `last_event_seq`
  - `EventHub` (`Clone`): `EventHub::new() -> EventHub`, `subscribe(&self, ThreadId) -> tokio::sync::broadcast::Receiver<EventSeq>` and `notify(&self, ThreadId, EventSeq)`. The broadcast is only a wake-up; the log is the truth. `notify` removes a thread's sender once its `receiver_count()` is 0, and `subscribe` creates it again, so the map never grows with every thread ever opened.
  - `EventHub::open_stream(&self, user_id: UserId) -> Result<StreamPermit, KatError>` and `MAX_STREAMS_PER_USER: usize = 6` (initial default, the browser's per-host HTTP/1 connection budget): a seventh open stream for one user in this process is `KatError::TooManyStreams` (429 `too_many_streams`); dropping the `StreamPermit` frees its slot
  - `RunEnd::{Finished, Cancelled, Interrupted}`, whose wire values are the `runs.state` values `finished`, `cancelled` and `interrupted`
  - `#[async_trait] trait EventSink: Send + Sync { async fn emit(&self, e: AgUiEvent) -> Result<EventSeq, KatError>; async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError>; }`
  - `Config.lock_timeout: Duration`, read from `KATWORK_LOCK_TIMEOUT_MS` (default 5,000; a positive integer, otherwise `config_invalid`); `db::set_lock_timeout(conn: &mut PgConnection, t: Duration) -> Result<(), KatError>`, which runs `SELECT set_config('lock_timeout', $1, true)`, the transaction-local form, because `SET LOCAL` takes no bind parameters; and `KatError::Busy` (503 `busy`, sent with `Retry-After: 1`), into which every database error with SQLSTATE `55P03` converts
  - `PgEventSink::new(pool: PgPool, hub: EventHub, lock_timeout: Duration, workspace_id: WorkspaceId, thread_id: ThreadId, run_id: Option<RunId>, fence: Option<Fence>) -> PgEventSink`: each `emit` runs, in its own transaction, `db::set_lock_timeout`, then `fence::check` (when `fence` is set; a shared task-row lock) and then `log::append` (the thread lock), in the global order, commits, and only then calls `hub.notify`. A thread lock held past `lock_timeout` returns `busy` and writes nothing, so a stuck transaction cannot freeze every stream of the thread silently. A failed check returns `lease_lost` and writes nothing, so a worker whose lease was reclaimed or cancelled cannot add events after the harness closed its run. `close_run` needs a `run_id` and a fence. In one transaction it runs `fence::check`; for each of the run's open calls (no `ended_at` in `tool_calls`) it appends `TOOL_CALL_RESULT` and `katwork.tool_status{ok: false, code}` and marks the call `outcome_unknown`; it appends `katwork.run_closed {end, code}` and then `RUN_ERROR{message: code, code}`, and sets the run's `state` to `end` with `ended_at = now()`. The terminal event and the run's closure therefore commit together or not at all.
  - `DeltaCoalescer::new() -> DeltaCoalescer`, `push(&mut self, delta: &str, now: Instant) -> Option<String>` and `finish(&mut self) -> Option<String>`: it buffers text deltas and releases them at ≥100 ms or ≥512 bytes, cut only at a grapheme-cluster boundary (`unicode-segmentation`). It always holds back the buffer's last grapheme cluster until more text arrives or `finish()`, because that cluster may still grow (`👋` then `🏽`, `🇹` then `🇭`, a Thai consonant then its mark). It also never releases a chunk that ends with a Thai leading vowel U+0E40–U+0E44 (เ แ โ ใ ไ): such a vowel is its own cluster but is read with the consonant after it, so it is held back with the cluster that follows. The client therefore renders deltas as received and never cuts text itself.
  - `Core` gains `events: EventHub` and `shutdown: CancellationToken` (`tokio-util`, added here; Task 8 uses the same crate), built in `Core::new`. `serve` cancels `core.shutdown` when the shutdown signal arrives.
  - `transport::serve_with_shutdown`, once its shutdown future resolves, waits at most `SHUTDOWN_GRACE: Duration = 10 s` for open connections and then aborts the rest. A connection's graceful shutdown waits for its response body, and an SSE body ends only through `core.shutdown`, so without the bound a single stream that missed the token would hold the process open.
  - Route `GET /api/workspaces/{workspace_id}/threads/{thread_id}/events?after=<seq>`: SSE with `id:` set to the per-thread `EventSeq`.
    - Cursor: the `Last-Event-ID` header when present, else `after`, else 0. The header wins, because a native reconnect resends the original URL and adds the header. A value that is not ASCII digits, or is above 2^53 − 1, is 400 `invalid_cursor`. `EventSource` cannot send headers on its first connect, so a page passes `after` from the messages snapshot's `event_seq` (Task 6) instead of replaying from 0.
    - Before the response starts, the route authenticates, resolves the tenant and the thread (404 across tenants), parses the cursor and takes a `StreamPermit` (429 `too_many_streams`). Every refusal is a JSON `{code}` response; any non-200 ends a browser `EventSource` for good, which Task 13b handles.
    - The body opens with `retry: 3000`. It subscribes to the hub, then replays `since(cursor)` in pages of 500 until a page comes back short, then goes live: on each wake-up, on a lagged receiver, and on a 2 s tick even without a wake-up (a notify can be lost when the appender's future is dropped between its commit and `hub.notify`), it reads `since(last sent)` in the same pages. It drops any event with `seq ≤` the last one sent. A cursor above `log::tail` (a restored database) sends `event: resync_required` with `data: {"reason":"cursor_ahead"}` and ends.
    - Every 15 s it writes a `:` comment, for proxies, and `event: ping` with `data: {"tail": <last seq sent>}` and **no `id:` line**: `EventSource` hides comments from the page, so the named ping is what the client watches, and a block without `id:` never moves the browser's last event id. Only log events carry `id:`.
    - Every 30 s it re-checks the session (`session::authenticate` with the cookie token it started with) and the membership (`tenancy::resolve`); on failure it sends `event: stream_error` with `data: {"code":"unauthenticated"}` or `{"code":"not_found"}` and ends. The event is named `stream_error` because `EventSource` already fires `error` for connection failures.
    - It ends with `take_until(core.shutdown.clone().cancelled_owned())`, so graceful shutdown finishes.
    - Replay runs inside the stream body, never in the handler before the response starts. The route is mounted outside Task 3's 30 s request-timeout and 256-request concurrency layers, which would cut every stream and let open streams starve other requests, and inside every other guard (Host, Origin, security headers, authentication). The transport's idle timeout does not close it, because the keep-alive writes every 15 s.
    - Behind the HTTPS profile every browser shares Caddy's address. Task 3b exempts a trusted proxy from the per-peer connection cap, and the per-user stream cap keeps one person's open tabs from using up the connections everyone else needs to log in.

  - New `KatError` variants: `TooManyStreams` (429 `too_many_streams`), `InvalidCursor` (400 `invalid_cursor`) and `Busy` (503 `busy`)

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/events.rs`:
  - `events_serialize_as_agui_json`: `TextMessageContent` → `{"type":"TEXT_MESSAGE_CONTENT","messageId":"…","delta":"…"}`
  - `agui_events_validate_against_vendored_1_0_schema`: every emitted variant, as serialized, validates against `tests/fixtures/agui/schema.json` with the `jsonschema` crate (Draft 2020-12); a variant with one extra field fails
  - `agui_spec_valid_fixtures_roundtrip`: each vendored `spec/<Event>/valid/*.json` of the 11 emitted types deserializes into `AgUiEvent` and serializes back to equal JSON
  - `katwork_payloads_match_their_fixtures`: every `katwork.*` payload round-trips against its fixture in `tests/fixtures/agui/katwork/`
  - `run_error_message_is_the_code`: every `RUN_ERROR` the sink writes has `message == code`
  - `concurrent_appenders_never_skip_on_resume`: two tasks append to one thread at once, and a reader resuming at any seq sees every event
  - `since_pages_without_gap_or_duplicate`: 5,000 events read with `limit` 500 while another task appends; the seqs read are strictly consecutive
  - `close_run_is_atomic_with_its_terminal_event`: when the `RUN_ERROR` append fails inside `close_run`, nothing commits: no closing events, and the run is still `running`
  - `run_closed_immediately_precedes_its_terminal_event`: two runs of one thread close at once; each run's `katwork.run_closed` is followed, at the next seq, by its own `RUN_ERROR`, with no other event between
  - `fenced_sink_rejects_writes_after_epoch_change`: after the task's epoch is incremented, `emit` returns `lease_lost` and the log is unchanged
  - `nul_in_delta_does_not_fail_append`: a delta `"a\u{0}b"` is stored and replayed as `"a\u{FFFD}b"`
  - `sink_lock_wait_is_bounded`: another transaction holds the thread row past a `lock_timeout` of 200 ms; `emit` returns `busy` and the log is unchanged
  - `thai_delta_never_splits_a_grapheme_cluster`: `"ที่นี่สวัสดีครับ 👋🏽"` pushed in every split; every released chunk ends on a grapheme boundary, and the concatenation is byte-equal
  - `coalescer_holds_back_trailing_cluster`: `"👋"` then `"🏽"`, `"🇹"` then `"🇭"`, and `"เ"` then `"ก"`, pushed in every split; no released chunk ends inside a cluster or on a leading vowel, and the concatenation is byte-equal
  - `hub_drops_sender_when_last_receiver_leaves`
  - `seventh_stream_permit_is_refused_until_one_drops`
  - in `crates/katwork/tests/config.rs`: `lock_timeout_must_be_a_positive_integer`: unset gives 5 s, `"250"` gives 250 ms, and `"0"` or `"x"` is `config_invalid (KATWORK_LOCK_TIMEOUT_MS)`
- [ ] **Step 2: Write the failing route tests** in `crates/katwork-server/tests/events.rs`, reading the response as raw bytes, never `String`s:
  - `reconnect_with_last_event_id_has_no_gap_or_duplicate`: Review Focus 3
  - `replay_to_live_gap_loses_nothing`: an event appended between replay and the live phase arrives once
  - `lagged_subscriber_recovers_from_log`
  - `missed_notify_is_recovered_by_poll`: paused tokio clock; an event appended and committed without `hub.notify` arrives within 2 s
  - `last_event_id_header_wins_over_after_query`: with the header `5` and `after=2`, the first event sent is seq 6; with `after=2` alone it is seq 3
  - `non_numeric_last_event_id_is_400`: `invalid_cursor` as JSON, before any stream bytes
  - `cursor_ahead_of_tail_sends_resync_required_then_ends`
  - `stream_starts_with_retry_3000`
  - `ping_event_has_no_id_line`: paused tokio clock; at 15 s the raw bytes hold a `:` comment and `event: ping` with no `id:` line, and the next log event still carries its own seq
  - `stream_closes_after_session_revoked_or_member_removed`: paused tokio clock; after `session::revoke`, or after the membership row is removed, the next 30 s check sends `stream_error` with the matching code and the body ends
  - `seventh_stream_for_one_user_is_429`: `too_many_streams`; closing one of the six lets a new one open
  - `sse_thai_and_emoji_event_parses_at_every_byte_split`: uses `tests/fixtures/sse/thai_emoji.sse`
  - `sse_fixture_matches_the_web_fixture`: `tests/fixtures/sse/thai_emoji.sse` is byte-for-byte equal to Task 13a's `web/src/testing/fixtures/thai_emoji.sse`, so the server and the browser parse the same bytes
  - `sse_crlf_framing_and_comment_keepalives_parse`
  - `duplicate_replay_is_dropped`
  - `other_workspace_cannot_subscribe`: 404
  - `sse_stream_outlives_the_request_timeout`: paused tokio clock; a stream stays open and keeps delivering past 30 s
  - `open_streams_do_not_count_against_the_request_limit`: with 257 streams open through the router (as every HTTP-level test runs, without the transport), from 43 users, an ordinary request still succeeds
  - `sse_route_keeps_host_and_origin_guards`: `Host: evil.example` is still 403 `host_not_allowed`
  - in `crates/katwork-server/tests/transport.rs`, through `serve_with_shutdown`:
    - `stream_ends_on_shutdown_token`: with a stream open, cancelling `core.shutdown` and resolving the shutdown future ends the body within 1 s, and `serve_with_shutdown` returns
    - `shutdown_aborts_connections_after_grace`: paused tokio clock; a connection whose body never ends is aborted at `SHUTDOWN_GRACE`, and `serve_with_shutdown` returns
    - `streams_through_trusted_proxy_peer_do_not_block_login`: with the test peer in `trusted_proxies`, 8 users open 5 streams each (40 connections, above the per-peer cap of 32), and `POST /api/auth/login` from the same peer still succeeds; a seventh stream for one user is 429 `too_many_streams`. It relies on Task 3b's trusted-proxy exemption: if this task merges before Task 3b, Task 3b's PR adds this test instead.
- [ ] **Step 3: Run** `cargo test -p katwork --test events --test config && cargo test -p katwork-server --test events --test transport`. Expected: FAIL.
- [ ] **Step 4: Implement**, including the `NOTICE` line for the vendored schema and the `main.rs` change that cancels `core.shutdown` on the signal.
- [ ] **Step 5: Run.** Expected: PASS.
- [ ] **Step 6: Commit** with `feat: event log and AG-UI SSE with resume`.

### Task 8: Tool registry and agent loop

**Files:**
- Create: `crates/katwork/src/agent/{mod,tool,ctx,registry,builtin,run,guards,ledger}.rs`, `crates/katwork/tests/agent.rs`

**Interfaces:**
- Consumes: `fence::{Fence, check, lock}`, `types::Question` (Task 2); `Registry`, `Selected`, `ModelProvider`, `ChatRequest`, `ChatMsg`, `ChatChunk`, `ToolSpec`, `ModelError`, `StreamTimeouts`, `guard_stream` (Task 7); `ModelEntry.timeouts` (Task 7b); `EventSink`, `RunEnd`, `AgUiEvent`, `KatworkEvent`, `DotState`, `DeltaCoalescer`, and `tokio-util`'s `CancellationToken` (Task 9); `Dot`, `Dot::config_hash`, `dot::get_on`, `dot::is_granted`, `dot::is_eligible` (Task 5); `Tenant` (Task 4); ids and `types::{SideEffect, SpacePolicy}` (Task 2).
- Produces:
  - `#[async_trait] trait Tool: Send + Sync { fn spec(&self) -> ToolSpec; fn side_effect(&self) -> SideEffect; async fn call(&self, ctx: &ToolCtx, args: serde_json::Value) -> Result<ToolOutput, ToolError>; }`
  - `ToolOutput::{Text(String), Ask(Question)}`: `Text` goes back to the model as the tool result; `Ask` ends the run with that question
  - `ToolCtx { tenant: Tenant, thread_id: ThreadId, space_id: SpaceId, dot_id: DotId, job_id: JobId, task_id: TaskId, run_id: RunId, step: u32, tool_call_id: String, idempotency_key: String, lease_epoch: LeaseEpoch, policy_version: i64, actor: UserId, initiator: Initiator, computer: Option<ComputerHandle>, pool: PgPool, cancel: CancellationToken }`
    - `idempotency_key` is `"{run_id}:{step}:{tool_call_id}"`
    - `space_id` and `policy_version` come from `RunInput`
    - `computer` is always `None` in SP1; `ComputerHandle` is an empty placeholder struct
    - tenant authority and the pool never appear in model-visible arguments
    - `ToolCtx::fence(&self) -> Fence` gives `{task_id, lease_epoch}`. Every tool write transaction starts with `guards::authorize_write(&mut tx, ctx)`, which includes `fence::check`; when it returns a stop reason the tool rolls back and returns `ToolError::Halted(reason)`.
  - `Initiator::{Person, Routine, Handoff, System}` (`Serialize`, `Deserialize`)
  - `ToolError::{NotAllowed, NeedsApproval, InvalidArgs(String), Failed(String), Timeout, OutcomeUnknown, Halted(StopReason)}`. `Halted` means the tool's write found the run no longer authorized and committed nothing.
  - `ToolRegistry::new() -> ToolRegistry`, `register(&mut self, tool: Arc<dyn Tool>) -> Result<(), DuplicateTool>`, `get(&self, id: &str) -> Option<Arc<dyn Tool>>` and `ToolRegistry::builtin() -> ToolRegistry`. `DuplicateTool { name: String }`: registering a name that is already registered is an error and keeps the first handler; a tool is never replaced silently. `builtin()` registers the fixed built-ins, whose names a test proves distinct.
  - Built-in tools. There is no `post_reply`: the final assistant text is the reply, written once by `job::conclude` (Task 10).
    - `read_thread` (`Read`). Arguments `{"limit": integer 1–50, default 20, "before": message id, optional}`, no other properties. It reads only `ctx.thread_id`, newest last, and returns the JSON text `{"messages": [{"id", "author": "user:<name>" | "dot:<name>" | "system", "body", "created_at"}], "next_before": id or null}`, which the loop then truncates like any result.
    - `ask_user` (`Read`). Arguments `{"question": string of 1–500 characters, "options": array of 0–4 strings of 1–100 characters each}`, no other properties; the first option is the recommended one. It returns `ToolOutput::Ask(Question)`. The loop records the call `succeeded` with the question's JSON as its result, sets `RunFacts.pending_question`, and ends the run. Calls listed before `ask_user` in the same turn have already run and keep their own rows; only the calls after it, never dispatched, are recorded `refused` with code `superseded_by_question`. `job::conclude` stores the question as the Dot's message: the body is the question text, and `messages.question` holds the question with its options.
  - `RunKind::{Initial, Repair, Recovery}`
  - `RunInput { tenant: Tenant, run_id: RunId, thread_id: ThreadId, space_id: SpaceId, job_id: JobId, task_id: TaskId, lease_epoch: LeaseEpoch, kind: RunKind, space_policy: SpacePolicy, history: Vec<ChatMsg>, reply_message_id: MessageId, policy_version: i64, actor: UserId, initiator: Initiator, deadline: tokio::time::Instant, cancel: CancellationToken }`
  - `RunEnv<'a> { pool: &'a PgPool, models: &'a Registry, tools: &'a ToolRegistry, sink: &'a dyn EventSink }`
  - `run::execute(env: &RunEnv<'_>, dot: &Dot, input: RunInput, limits: &Limits) -> Result<RunFacts, RunAbort>`. It inserts the `runs` row, in state `running`, first, and never closes it itself. A run's row leaves `running` only in the transaction that writes its terminal event: `job::conclude` for `RUN_FINISHED`, and `EventSink::close_run`, claim's reclaim closing or `job::cancel` for `RUN_ERROR`. So `state = 'running'` means exactly that the run has no terminal event yet.
  - `RunAbort::{LeaseLost, Infrastructure { run_id: Option<RunId>, code: &'static str }}`: a run-scoped write that fails with `lease_lost` gives `LeaseLost`; any other failed write (the `runs` row, a ledger row, an event) gives `Infrastructure` with the error's code. Either way nothing more is dispatched, and the abort never becomes a policy verdict.
  - `RunFacts { run_id: Option<RunId>, reply_message_id: MessageId, final_text: Option<String>, stop: Option<StopReason>, tool_calls: Vec<ToolCallRecord>, artifacts_created: u32, pending_tool_call: Option<PendingCall>, pending_question: Option<Question>, synthetic: bool, executor_unavailable: bool, run_closed: bool, usage: Vec<ModelCallId> }`. `run_closed` is true only when the loop already ended the run with `RUN_ERROR` (the cancellation path). The loop returns facts only; the policy (Task 10) turns them into a job status.
  - `RunFacts::not_run(stop: StopReason) -> RunFacts`: no run id, a fresh `reply_message_id`, everything else empty; used when the harness decides without a run (Tasks 11 and 12)
  - `ToolCallRecord { id: ToolCallRecordId, step: u32, tool_call_id: String, name: String, side_effect: Option<SideEffect>, state: ToolCallState }` and `ToolCallState::{Started, Succeeded, Failed, TimedOut, OutcomeUnknown, Refused}`
  - `PendingCall { record_id: ToolCallRecordId, tool_call_id: String, name: String, args_canonical: String, args_sha256: String }`, stored as `tasks.pending_tool_call`
  - `StopReason::{ProviderUnavailable, ProviderRateLimited, ProviderQuota, ProviderAuthFailed, ModelProtocol, StepLimit, LoopDetected, NeedsApproval, TokenBudget, OutputTruncated, TimeLimit, PolicyChanged, ResidencyDenied, QuotaExceeded, TooManyRequests, WorkspacePaused, DotPaused, NoEligibleDot, Interrupted, OutcomeUnknown, Cancelled}`. `TooManyRequests` is the per-sender throttle at intake (Task 12), kept apart from `QuotaExceeded`, the daily token budget. `StopReason` derives `Serialize` and `Deserialize` with snake_case wire names (`provider_unavailable`, `dot_paused`, …), which Task 10's `PolicyInputs` records.
  - `Limits { max_steps: u32, tool_timeout: Duration, result_max_chars: usize, repeat_limit: u32, repeat_window: (u32, u32), max_total_tokens: u64, task_deadline: Duration, job_deadline: Duration, model_retries: u32 }`. `Limits::default()` holds the initial defaults: 24, 60 s, 8,000, 3, (5, 10), 200,000, 5 min, 10 min and 1. Stream timeouts and the run wall clock are not fixed limits: each model entry carries its own (`ModelEntry.timeouts`, Task 7b). The run deadline is separate from the lease TTL.
  - `guards::authorize(conn: &mut PgConnection, tenant: &Tenant, fence: &Fence, dot_id: DotId, space_id: SpaceId, policy_version: i64) -> Result<Option<StopReason>, KatError>`: runs `fence::check`, then decides in this order: a paused workspace gives `WorkspacePaused`; `dot::is_granted` false (the grant to `space_id` was revoked) gives `PolicyChanged`; granted but `dot::is_eligible` false (the Dot is paused) gives `DotPaused`; otherwise a `policy_version` (read with `dot::get_on`) other than the run's gives `PolicyChanged`. The status check comes before the version check because pausing a Dot also increments its version, and the person must see `dot_paused`, not `policy_changed`. Allowlist, status, grant, autonomy and model-tier changes all increment `policy_version` (Task 5), so a change made while the task was queued is caught on the first check; an edit to `max_concurrency`, `daily_token_budget`, `persona`, `instructions`, `name` or `appearance` never stops a run and applies from the next one.
  - `guards::authorize_write(conn: &mut PgConnection, ctx: &ToolCtx) -> Result<Option<StopReason>, KatError>`: the first statements of every tool write transaction. It runs `fence::check` (the task row, shared), then locks the Dot row `FOR SHARE` (task row before Dot row: the global order), then makes `authorize`'s checks against `ctx.space_id` and `ctx.policy_version`. A Dot edit that bumps `policy_version` updates the Dot row, which conflicts with `FOR SHARE`, so it waits for an open write and applies to later dispatches, and an edit committed before the write's check makes the write commit nothing.
  - `guards::canonical_args(args: &serde_json::Value) -> String`: whitespace normalized, array order and values kept, and object keys sorted at every depth with `Value::sort_all_objects()` on a clone before serializing, because the workspace builds serde_json with `preserve_order` (Global Constraints)
  - `guards::RepeatKey { tool_name: String, canonical_args: String, target_revision: Option<String> }`, where `target_revision` is always `None` in SP1 (SP2 fills it so a render after each edit is not a loop), and `guards::RepeatGuard::new(limit: u32, window: (u32, u32))` with `check(&mut self, key: &RepeatKey) -> bool`, false meaning stop before this call. The provider's call id is never part of the key.
  - `guards::truncate_result(text: &str, max_scalars: usize) -> String`: at most `max_scalars` Unicode scalar values, the marker `"…[truncated]"` included. A JSON result over the cap becomes a plain-string preview prefixed `"[JSON truncated] "`, never cut inside its structure.
  - `ledger::start_tool_call(pool, ctx: &ToolCtx, name: &str, side_effect: SideEffect, args_canonical: &str) -> Result<ToolCallRecordId, KatError>`: inserts a `started` row with the canonical arguments and their SHA-256. For a non-Read call it runs `fence::lock` first, because it also sets `tasks.effects_started_at` in the same transaction; a Read call needs only `fence::check`. Its transaction commits before the tool's `call` is first polled, so `effects_started_at` is durable before any non-Read effect can happen; Task 11's reclaim decides re-queue or `interrupted` from it alone.
  - Every ledger function replaces U+0000 with U+FFFD in the text it stores (`tool_name`, `result`), because the model chooses tool names and tools return text (Global Constraints).
  - `ledger::record_refused(pool, ctx: &ToolCtx, name: &str, side_effect: Option<SideEffect>, args_canonical: &str, code: &str) -> Result<ToolCallRecordId, KatError>`: inserts a finished `refused` row whose `result` is the error text sent to the model, and never touches `effects_started_at`. `side_effect` is `None` only for an unknown tool name. It records allowlist and effect-gate refusals (`NotAllowed`, `NeedsApproval`), invalid arguments, unknown tools, repeat-guard stops and `superseded_by_question`.
  - `ledger::retry_read(pool, fence: &Fence, id: ToolCallRecordId) -> Result<(), KatError>`: a `Read` call's one retry reuses its row: `attempts = 2` and the state goes back to `started`; the `(run_id, step, tool_call_id)` identity never changes
  - `ledger::end_tool_call(pool, fence: &Fence, id: ToolCallRecordId, state: ToolCallState, result: Option<&str>) -> Result<(), KatError>`
  - `ledger::record_model_call(pool, fence: &Fence, row: &ModelCallRow) -> Result<ModelCallId, KatError>`, where `ModelCallRow` mirrors the `model_calls` columns
  - Fencing: every run-scoped write (the `runs` row, each ledger function, each event through the fenced `PgEventSink`, each tool write) runs `fence::check` in the same transaction as the write, and `lease_lost` aborts it. Before each model attempt the loop also runs `guards::authorize`, which includes `fence::check`, so a reclaimed or cancelled worker makes no further model calls. On `lease_lost` the loop stops at once, writes nothing further and returns `Err(RunAbort::LeaseLost)`.
- Loop rules:
  - **Steps.** A step is one model attempt, retries included. An answer on attempt 24 is accepted; a tool batch that would need attempt 25 is not executed (`StepLimit`).
  - **Model attempts.** Each attempt calls `models.for_tier(dot.model_tier, &input.space_policy)`, so a provider change re-checks residency, and wraps the stream in `guard_stream` with `selected.entry.timeouts.stream()`, each timeout capped by the run's remaining time, raced against the run deadline (`TimeLimit`); no attempt starts once the run deadline has passed.
  - **Run deadline.** `input.deadline` is the task and job bound (Task 11). When the first attempt selects its entry, the run deadline becomes `min(input.deadline, run_started + selected.entry.timeouts.run_wall_clock)`, where `run_started` is the `tokio::time::Instant` at which `execute` began; an attempt on another entry later in the run never extends it. A run whose `input.deadline` has already passed stops `TimeLimit` before it selects an entry.
  - **Retries.** A model attempt may be retried only before the first content chunk (a non-empty `TextDelta`, `Progress`, `ToolCall` or `Done`), at most `model_retries` times. Once content has started, a failure ends the attempt with its mapped `StopReason` and is never retried. Record one `model_calls` row per attempt and charge each attempt once. Every attempt also gets one `katwork.usage` event. Task 7's adapters never retry, so this is the only retry. Each request uses `max_output_tokens = min(entry.max_output_tokens, the run's remaining token budget)` and `temperature = entry.temperature`; with less than 256 tokens of budget left, the run stops `TokenBudget` before the attempt.
  - **Provider errors.** Every `ModelError` has exactly one outcome. Provider text never reaches an event, a message, the job reason or any log. The loop logs a failed attempt only with the stop reason's stable code, the attempt number and the `model_calls` id; the provider's own details are already in Task 7's `model_provider_error` line (error kind, HTTP status, provider request id, byte count) and nowhere else.

    | `ModelError` | retried before the first content chunk | outcome when it persists, or at once after content started |
    |---|---|---|
    | `ProviderUnavailable`, `Other(_)`, `Timeout { phase: FirstChunk }` | yes, up to `model_retries` | stop `ProviderUnavailable`, `executor_unavailable = true` |
    | `Timeout { phase: Idle }` | no, content already arrived | stop `ProviderUnavailable`, `executor_unavailable = true` |
    | `RateLimited { retry_after_secs }` | once, only if the wait fits in the run's remaining time | stop `ProviderRateLimited` |
    | `QuotaExceeded { .. }` | no | stop `ProviderQuota` |
    | `AuthFailed` | no | stop `ProviderAuthFailed` |
    | `Protocol(_)` | no | stop `ModelProtocol` |
    | `ResidencyDenied` | no | stop `ResidencyDenied` |

  - **Token budget.** Each attempt is charged the reported input plus output when both counts are present. A missing output count is charged as that request's `max_output_tokens`; a missing input count as `ChatRequest::input_charge_bound()` of that request (Task 7). The charge goes to `model_calls.charged_tokens`, while the reported counts stay NULL when missing. Once the run's charged total reaches `max_total_tokens`, the run stops `TokenBudget` before the next attempt.
  - **Turn validation before dispatch.** Only an `EndTurn` or `ToolUse` turn can dispatch tools. Errored, aborted, filtered or unknown turn endings, malformed arguments and unfinished calls never execute. A `MaxTokens` turn keeps its partial text as `final_text`, stops `OutputTruncated`, and runs none of its tool calls.
  - **Allowlist at call time.** Each call resolves the tool from `dot.tool_allowlist ∩ ToolRegistry`; anything else gets a `ToolError::NotAllowed` error result.
  - **Authority before every model attempt and every tool dispatch.** The loop runs `guards::authorize`; a returned stop reason ends the run before that model call or tool runs.
  - **Effect gate before dispatch.** `Read`, `WorkspaceRead`, `Draft` and `WorkspaceWrite` tools run. `Navigate`, `Exec` and `CredentialUse` return `ToolError::NotAllowed`, since SP1 has no computer. `Destructive` and `Outward(_)` return `ToolError::NeedsApproval` before the tool is called: the run stops `NeedsApproval` and records `pending_tool_call` with the provider's `tool_call_id`.
  - **Repeat guard before dispatch.** The run stops `LoopDetected` before executing the 3rd consecutive identical call, or the 5th identical call within the last 10. Two calls are identical when their `RepeatKey`s are equal: the tool name, the canonical arguments and the target revision. The provider's call id, the step and the earlier call's outcome are never part of the key. The stopping call and every later call of the same turn are each recorded `refused` with code `loop_detected`, under their own name and call id, and none executes.
  - **Timeouts and cancellation.** Each call's future is owned and raced against `min(tool_timeout, run remaining)` and the run's `CancellationToken`, and dropped on either. Cancellation is checked before dispatch and between sibling calls. No task is ever detached. A call that times out or loses its reply after dispatch is `OutcomeUnknown`. A `Read` tool may retry once. For any other effect the run stops `OutcomeUnknown` at once: the call stays `outcome_unknown` in the ledger, no result goes back to the model, and nothing else is dispatched. The effect may already have committed, and a repeat requested by the model would carry a new call id and so a new idempotency key.
  - **No retry after an effect.** The harness retries only a model attempt before its first content chunk and a `Read` call once; it never retries a non-Read call, before or after its effect. SP1 ships no non-Read built-in tool. A non-Read tool added later must write a receipt keyed by `ctx.idempotency_key`, under a unique constraint, in the same transaction as its effect, and return the stored result when the same key comes again; an unknown outcome stays `outcome_unknown` until a person reviews it.
  - **Halted writes.** A tool that returns `ToolError::Halted(reason)` committed nothing: the call ends `failed` with the reason's code as its result, no result goes back to the model, nothing else is dispatched, and the run stops with `reason`. Revoking a Dot's authority therefore reaches a write that is already dispatched, not only later dispatches.
  - **Batch order.** A turn's tool calls are dispatched one at a time, in the order the model listed them, and each gets exactly one ledger row.
  - **Ledger.** Every dispatched call goes through `start_tool_call`, then `end_tool_call` with `succeeded`, `failed`, `timed_out` or `outcome_unknown`. Every call that is not dispatched goes through `record_refused`, so a refused outward call never marks effects as started. A failed ledger write aborts the run before the tool is called.
  - **Result pairing.** An unknown tool name, a validation failure, a timeout and a refusal each become exactly one `ChatMsg::Tool { is_error: true }` with the original call id and name, and the loop continues unless a stop rule above applies. The text given to the model is `truncate_result(…, result_max_chars)`, and that exact text is stored in `tool_calls.result`.
  - **Prompt hygiene.** The system text is the Dot's persona and instructions plus a capability preamble generated from the effective tool set, never hand-written into `instructions`. Tool results, history and retrieved text never use the system role. Prompts never say "never ask".
  - **Events.** `RUN_STARTED`, `katwork.run_started {runId, jobId, dotId, replyMessageId}` with `jobId = input.job_id`, `dot_state working`, then `TEXT_MESSAGE_START` with `messageId = reply_message_id`, the text through `DeltaCoalescer` as `TEXT_MESSAGE_CONTENT`, and `TEXT_MESSAGE_END`. Every model turn of the run streams under the same `reply_message_id` and opens with its own `TEXT_MESSAGE_START`, so text a turn wrote before calling tools is replaced, not kept; the stored reply arrives as `katwork.message_stored` (Task 10). Tool-call events follow only once the turn is accepted; `TOOL_CALL_RESULT` and `katwork.tool_status` follow execution. On cancel, the loop calls `sink.close_run(RunEnd::Cancelled, "cancelled")`, which closes every open call, emits `katwork.run_closed {end: cancelled, code: "cancelled"}` and then `RUN_ERROR{code: "cancelled"}`, and closes the run row in one transaction, then returns with `stop = Cancelled` and `run_closed = true`. A stop the loop finds itself, including `guards::authorize` returning `WorkspacePaused`, `DotPaused` or `PolicyChanged` between attempts, is not a cancellation: the loop emits no `RUN_ERROR`, leaves `run_closed = false`, and `job::conclude` ends the run with `RUN_FINISHED`. Each run therefore gets exactly one terminal event, and one owner for it. The loop never emits `RUN_FINISHED`; `job::conclude` does (Task 10).

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/agent.rs` (FakeProvider scripted; test-only tools registered per test):
  - `plain_text_reply_sets_final_text`
  - `tool_call_then_reply`
  - `unknown_tool_id_returns_error_result_and_continues`
  - `registered_tool_outside_dot_allowlist_is_refused`
  - `allowlist_revoked_mid_run_stops_before_next_tool`
  - `policy_change_between_attempts_stops_before_next_model_call`: the Dot is paused after the first attempt; no second model request reaches the provider and `stop = DotPaused`; in a second case the grant to the thread's Space is revoked instead, and `stop = PolicyChanged`
  - `destructive_and_outward_tools_refused_before_dispatch`: the tool's `call` never runs; `stop = NeedsApproval`; `pending_tool_call` keeps the call id
  - `navigate_exec_and_credential_tools_are_not_allowed`
  - `no_tool_runs_from_a_max_tokens_turn`: the partial text is kept and `stop = OutputTruncated`
  - `no_tool_runs_from_a_failed_turn`
  - `same_tool_same_args_three_times_stops_loop_detected`: the third call is not executed
  - `alternating_two_calls_five_times_in_ten_stops`
  - `call_id_and_key_order_do_not_defeat_repeat_guard`
  - `canonical_args_do_not_depend_on_key_order`: built as the workspace builds serde_json (with `preserve_order`), `{"b":1,"a":{"d":2,"c":3}}` and `{"a":{"c":3,"d":2},"b":1}` give the same `canonical_args` string and the same `args_sha256`
  - `loop_guard_refusal_pairs_every_tool_call`: in a turn whose second call trips the repeat guard, that call and the turn's later calls each get one `refused` ledger row with code `loop_detected` and their own name and call id, and no tool's `call` runs
  - `same_mutation_with_new_call_id_counts_as_repeat`: the same tool and canonical arguments under three different call ids stop the run before the third dispatch
  - `duplicate_tool_name_is_rejected`: a second `register` of a name returns `DuplicateTool` and `get` still returns the first handler; the names in `ToolRegistry::builtin()` are distinct
  - `policy_revoked_before_write_commit_blocks_effect`: a `WorkspaceWrite` test tool is dispatched and blocks before its write transaction; the Dot's `tool_allowlist` changes, bumping `policy_version`; the tool resumes, its transaction commits nothing, the call ends `failed`, and the run stops `PolicyChanged`
  - `policy_edit_waits_for_an_open_write`: a test tool's write transaction has passed `authorize_write` and waits; a `dot::update` that changes `tool_allowlist` blocks until that write commits, and the run's next dispatch stops `PolicyChanged`
  - `effects_marker_is_committed_before_a_non_read_call_runs`: a `WorkspaceWrite` test tool reads `tasks.effects_started_at` on a separate connection inside its `call` and sees it set
  - `nul_in_tool_name_or_result_is_stored_replaced`: an unknown tool name and a tool result holding U+0000 are stored with U+FFFD, and the run goes on
  - `unknown_outcome_write_stops_for_review_before_model_retries_it`: a `WorkspaceWrite` test tool commits and then times out; the model is scripted to request the same write again under a new call id; the second request is never dispatched, `stop = OutcomeUnknown`, and the ledger holds one `outcome_unknown` row
  - `answer_on_step_24_accepted_tool_batch_needing_25_stops`
  - `token_budget_200k_stops_before_next_attempt`
  - `hosted_run_wall_clock_90s_stops_time_limit`: paused tokio clock
  - `local_run_wall_clock_is_300s`: paused tokio clock; a run on a `Local` entry is still going at 90 s and stops `TimeLimit` at 300 s
  - `run_deadline_never_exceeds_input_deadline`: with `input.deadline` 10 s away, a run on a `Local` entry stops `TimeLimit` at 10 s
  - `tool_result_truncated_to_8000_scalars_including_marker_thai`: includes a long single-line Thai string
  - `tool_timeout_60s_returns_error_result`: paused tokio clock
  - `timed_out_tool_cannot_write_afterwards`
  - `old_worker_cannot_emit_text_after_reclaim`: the run's lease is reclaimed mid-stream; its next text event fails with `lease_lost`, nothing more is logged, and no further model attempt starts
  - `old_worker_cannot_commit_tool_write_after_reclaim`: a test tool blocks, the lease is reclaimed, then the tool resumes; its write transaction fails `fence::check` and commits nothing
  - `cancellation_token_stops_with_cancelled`
  - `no_done_after_cancellation`: no `RUN_FINISHED` and no text event after the cancel
  - `ask_user_ends_run_needs_input`: `pending_question` is set, `final_text` is `None`, and a later call in the same turn is recorded `refused` with `superseded_by_question`
  - `ask_user_in_middle_of_batch_keeps_earlier_results`: the batch `[read_thread, ask_user, read_thread]` gives exactly three ledger rows: `succeeded`, `succeeded` and `refused`
  - `ask_user_rejects_arguments_outside_its_schema`: a 501-character question or a fifth option gives an `InvalidArgs` error result
  - `read_thread_returns_a_bounded_page`: `limit` above 50 is rejected; the page and `next_before` match the stored thread
  - `refused_outward_call_does_not_mark_effects_started`
  - `unknown_tool_is_ledgered_refused_with_no_effect`
  - `read_retry_reuses_its_ledger_row`
  - `run_insert_failure_aborts_without_dispatch`: a closed pool; `execute` returns `Infrastructure` and the provider receives no request
  - `ledger_start_failure_prevents_tool_dispatch`: the tool's `call` never runs
  - `event_write_failure_aborts_run`: a failing test `EventSink`; `execute` returns `Infrastructure`
  - `tool_output_never_enters_system_role`
  - `capability_preamble_lists_effective_tools`
  - `usage_recorded_once_per_attempt`
  - `no_retry_after_content_or_progress`: a scripted `TextDelta`, and in a second case only a `Progress`, followed by `ProviderUnavailable`; the provider receives exactly one request, the run stops `ProviderUnavailable`, and one `model_calls` row is charged once
  - `retry_before_first_content_gets_its_own_row`: a first attempt that fails before any content is retried once; two `model_calls` rows, each charged once
  - `attempt_uses_its_entrys_stream_timeouts`: paused tokio clock; an entry with 120 s / 60 s timeouts does not time out at 30 s, and its wait is still capped by the run's remaining time
  - `every_model_error_maps_to_a_stable_stop_reason`: table-driven over the table above; no provider text reaches an event or message
  - `provider_error_text_never_reaches_logs`: a scripted `Protocol` and `Other` error whose diagnostic carries a unique prompt marker; with all tracing output captured at `TRACE`, the marker appears in no log line, event or message
  - `max_output_tokens_capped_by_remaining_budget`
  - `missing_usage_is_charged_conservatively_and_stored_null`
  - `duplicate_call_ids_stay_distinguishable`: the same provider id in two steps gives two ledger rows and two `toolCallId`s
  - `successful_sibling_audited_when_another_fails`
  - `provider_change_rechecks_residency`
- [ ] **Step 2: Run** `cargo test -p katwork --test agent`. Expected: FAIL.
- [ ] **Step 3: Implement.** The loop alternates model stream → validated turn → gated tool calls → tool results until the model stops calling tools or a guard fires.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: tool registry and guarded agent loop`.

### Task 10: Jobs, status transitions and the policy function

**Files:**
- Create: `crates/katwork/src/jobs/{mod,job,task}.rs`, `crates/katwork/src/policy.rs`, `crates/katwork/tests/{jobs.rs,policy.rs}`
- Modify: `web/src/i18n/{en,th}.json` (`reason.too_many_requests`)

**Interfaces:**
- Consumes: `RunFacts`, `StopReason`, `Initiator`, `ToolCallRecord`, `ToolCallState` (Task 8); `log::append`, `EventHub`, `AgUiEvent`, `KatworkEvent`, `DotState`, `MessageStoredEvent`, `RunClosedEvent` (Task 9); `message::append`, `NewMessage`, `Appended`, `NoticeMeta`, `NoticeDot` (Task 6); `dot::get_on` (Task 5); `Tenant`; `types::{ReasonCode, JobStatus, SideEffect}`.
- Produces:
  - `ReasonCode` gains `TooManyRequests` (wire `too_many_requests`), added to `ReasonCode::ALL`, for the per-sender throttle (Task 12); it is never `quota_exceeded`. The same PR adds `reason.too_many_requests` to `web/src/i18n/{en,th}.json`, because Task 13a's `reason_codes_have_en_and_th_messages` checks every code
  - `NextStep::{RunCheapChecks, AskUser, HandToLead, SaveDraftWithReport, Retry, ReviewBeforeRetry, RestartJob}`
  - `Thresholds { version: u32 }` and `Thresholds::SP1` (`version: 1`)
  - `PolicyStatus::{Accepted, Ready, Answered, Stopped, NeedsInput}` with `From<PolicyStatus> for JobStatus`, and `Verdict { status: PolicyStatus, reason: Option<ReasonCode>, next: Option<NextStep> }`. The type, not a runtime check, keeps `Cancelled` and `Error` for the harness: `finish_by_policy` cannot write them.
  - `PolicyInputs { stop: Option<StopReason>, executor_unavailable: bool, has_final_text: bool, pending_question: bool, artifacts_created: u32, tool_calls: u32, effects_dispatched: bool, synthetic: bool }` (`Serialize`, `Deserialize`, camelCase) and `PolicyInputs::from_facts(facts: &RunFacts) -> PolicyInputs`. `effects_dispatched` is true when any call in `facts.tool_calls` has a side effect other than `Read` and a state other than `Refused`.
  - `policy::decide(inputs: &PolicyInputs, t: &Thresholds) -> Verdict` and `policy::decide_job(facts: &RunFacts, t: &Thresholds) -> Verdict`, which is `decide(&PolicyInputs::from_facts(facts), t)`: pure, with no I/O, clock or model. Rules, first match wins:
    1. `executor_unavailable` → `Stopped`, `provider_unavailable`, `Retry`
    2. `stop = Some(s)` → the table below
    3. `pending_question` set → `NeedsInput`, `question_pending`, `AskUser`
    4. `final_text` set → `Answered` when `artifacts_created == 0`, otherwise `Ready`
    5. anything else → `Stopped`, `model_protocol_error`, `Retry`

    Then, whatever rule matched: when `next` is `Retry` and `effects_dispatched` is true, `next` becomes `ReviewBeforeRetry`. A provider can fail after a non-Read call succeeded, and a blind retry would repeat that call's effect.

    | `StopReason` | status | reason | next step |
    |---|---|---|---|
    | `ProviderUnavailable` / `ProviderRateLimited` / `ModelProtocol` / `TimeLimit` | `stopped` | `provider_unavailable` / `provider_rate_limited` / `model_protocol_error` / `time_limit` | `Retry` |
    | `ProviderQuota` / `QuotaExceeded` / `WorkspacePaused` | `stopped` | `provider_quota` / `quota_exceeded` / `workspace_paused` | `Retry` (after the reset or unpause) |
    | `TooManyRequests` | `stopped` | `too_many_requests` | `Retry` (after a minute) |
    | `ProviderAuthFailed` / `DotPaused` / `ResidencyDenied` / `NoEligibleDot` | `stopped` | `provider_auth_failed` / `dot_paused` / `residency_denied` / `no_eligible_dot` | `AskUser` (an admin) |
    | `StepLimit` / `TokenBudget` / `OutputTruncated` | `stopped` | `step_limit` / `token_budget` / `output_truncated` | `SaveDraftWithReport` |
    | `LoopDetected` | `stopped` | `loop_detected` | `HandToLead` |
    | `PolicyChanged` | `stopped` | `policy_changed` | `RestartJob` |
    | `NeedsApproval` | `needs_input` | `approval_pending` | `AskUser` |
    | `Interrupted` (lease reclaimed after a side effect) / `OutcomeUnknown` (a non-Read call's result is unknown) | `stopped` | `interrupted_review_required` / `outcome_unknown` | `ReviewBeforeRetry` |
    | `Cancelled` | `stopped` | `cancelled` | `Retry`; never reached in practice, because cancelled jobs end through the harness |

  - `approval_pending` is not executable in SP1: no route or tool can approve the stored `pending_tool_call`, nothing ever dispatches it, and the person's next message starts a follow-up job (ADR-0002). Sub-project 3's approval host must bind each approval to `PendingCall.args_sha256`, record the approver and a receipt, and dispatch only once that receipt validates (ADR-0002, amended).
  - `policy::decision_event(job_id: JobId, facts: &RunFacts, v: &Verdict, t: &Thresholds) -> KatworkEvent`: builds `katwork.decision`. `inputs` is `PolicyInputs::from_facts(facts)` as JSON (`stop` as its wire name or null, `executorUnavailable`, `hasFinalText`, `pendingQuestion`, `artifactsCreated`, `toolCalls` as a count, `effectsDispatched`, `synthetic`), so `decide` on the recorded inputs gives the recorded verdict again; `verdict` is `{"status", "reason", "next"}` as wire strings; `thresholds` is `Thresholds` as JSON; `policyVersion` is `Thresholds.version`.
  - `job::create(conn: &mut PgConnection, &Tenant, ThreadId, MessageId) -> Result<JobId, KatError>`: a running job (`status` NULL) with `requested_by = tenant.user_id()`
  - `job::create_follow_up(conn: &mut PgConnection, &Tenant, parent: JobId, MessageId) -> Result<JobId, KatError>`: `parent` must be `needs_input` in the same thread, otherwise `job_not_awaiting_input`; sets `parent_job_id` (ADR-0002)
  - `JobSummary { id: JobId, status: Option<JobStatus>, dot_id: Option<DotId> }` and `job::latest_in_thread(conn: &mut PgConnection, &Tenant, ThreadId) -> Result<Option<JobSummary>, KatError>`: the thread's job with the highest `id` (`ORDER BY id DESC LIMIT 1`), never by `created_at`: `jobs.created_at` is the transaction start, so a post that waited for the thread lock can carry an older `created_at` than the job it follows, while job ids are minted inside the lock. `dot_id` is the Dot of that job's newest task by `(created_at, id)`, `None` when it has no task; Task 12 routes a follow-up to it.
  - `job::finish_by_policy(tx: &mut Transaction<'_, Postgres>, job_id: JobId, v: &Verdict, decision_seq: EventSeq) -> Result<EventSeq, KatError>`: sets `status`, `reason`, `decided_by = 'policy'` and `decision_seq`, then appends `katwork.job_status` and returns its seq. The caller has already appended `katwork.decision`.
  - `HarnessEnd::{Cancelled, Error { code: ReasonCode }}` and `job::finish_by_harness(tx: &mut Transaction<'_, Postgres>, job_id: JobId, end: HarnessEnd) -> Result<EventSeq, KatError>`: sets `status`, `reason` and `decided_by = 'harness'`, then appends `katwork.job_status` and returns its seq, inside the caller's transaction (claim and cancel use it), which already holds the job row and the thread row; the caller commits and notifies the hub
  - Finishing a finished job is `job_already_finished`.
  - `job::conclude(tx: &mut Transaction<'_, Postgres>, tenant: &Tenant, job_id: JobId, thread_id: ThreadId, dot_id: Option<DotId>, idem_key: &str, facts: &RunFacts) -> Result<EventSeq, KatError>`: the single place that writes a job's outcome. The caller must already hold, in the global lock order, the job row, the task row when there is one, and the thread row (`lease::complete`, `lease::claim`, intake and cancel do); `conclude` takes no other lock. In the caller's transaction it:
    1. re-reads the job's `status` (the caller holds its row) and returns `job_already_finished` before writing anything when it is set, so a retried conclusion adds no second decision or status event
    2. runs `policy::decide_job(facts, &Thresholds::SP1)`
    3. appends `katwork.decision`
    4. when `dot_id` is set and there is `final_text` or a `pending_question`, appends the Dot's message, followed by its `katwork.message_stored` event (`MessageStoredEvent::from(&appended.message)`), with `id = facts.reply_message_id`, `idempotency_key = "reply:{idem_key}"` and `synthetic = facts.synthetic`. For a question, the body is the question text and `question` holds the question with its options. Every U+0000 in the text becomes U+FFFD. Every message `conclude` writes has `job_id` set to the job.
    5. for a `stopped` verdict, appends a `System` message whose body is the reason code and whose `meta` is `NoticeMeta { code, dots }`, where `dots` names the Dot when `dot_id` is set (its name read with `dot::get_on`, no lock), with `idempotency_key = "notice:{idem_key}"`, followed by its `katwork.message_stored` event
    6. calls `job::finish_by_policy`, which stores `JobStatus::from(v.status)`
    7. when `facts.run_id` is set, appends `katwork.dot_state` (`needs_approval` for an approval stop, `waiting` for a question, `paused` for a workspace pause, `idle` otherwise), then, unless `facts.run_closed` (the loop already ended that run with `RUN_ERROR`), `katwork.run_closed {end: finished, code: None}` immediately followed by `RUN_FINISHED`, and with them it sets the run's `state = 'finished'`, `stop_reason` and `ended_at`, in the same transaction.

    It returns the last seq; the caller commits and notifies the hub.
  - `RouteRule::{Mention, FollowUp, Lead, FirstEligible}` (`Serialize`, `Deserialize`, snake_case) and `Routing { rule: RouteRule, ignored: Vec<DotId> }`: how intake chose the task's Dot, and which eligible Dots the post also mentioned and SP1 skipped (Task 12). Recorded on every task, so routing is testable by assertion and SP4's routing receipts can extend it.
  - `TaskInput { thread_id: ThreadId, request_message_id: MessageId, routing: Routing, actor: UserId, initiator: Initiator }` (`Serialize`, `Deserialize`; stored in `tasks.input`). It holds no history: the worker loads it when the run starts (Task 12's `pipeline::load_history`), so a queued job sees replies that landed while it waited, and a task never stores up to 24,000 characters of copied text.
  - `task::enqueue(conn: &mut PgConnection, job_id: JobId, dot_id: DotId, dot_policy_version: i64, input: &TaskInput, idempotency_key: &str) -> Result<TaskId, KatError>`: idempotent on the key; stores the version the routing decision saw
  - `job::create`, `job::create_follow_up`, `job::latest_in_thread` and `task::enqueue` take a connection so that intake (Task 12) runs them in one transaction.

- [ ] **Step 1: Write the failing tests:**
  - in `crates/katwork/tests/policy.rs`: `policy_same_facts_same_verdict`, `answered_only_without_artifacts_else_ready`, `executor_unavailable_is_stopped_provider_unavailable`, `needs_approval_is_needs_input_approval_pending`, `every_stop_reason_maps_to_its_reason_code` (table-driven over the table above), and:
    - `policy_status_converts_to_the_five_policy_owned_job_statuses`: the five `PolicyStatus` values convert to `accepted`, `ready`, `answered`, `stopped` and `needs_input`, and no value converts to `cancelled` or `error`
    - `provider_failure_after_write_requires_review`: facts with a `succeeded` `WorkspaceWrite` call and `stop = ProviderUnavailable` give `stopped`, `provider_unavailable`, `ReviewBeforeRetry`; the same facts whose only call is `Read`, or whose write was `refused`, give `Retry`
    - `decision_event_replays_same_verdict`: for one facts value per table row, `decide` on the `PolicyInputs` read back from the `katwork.decision` event's `inputs` equals the event's `verdict`
  - in `crates/katwork/tests/jobs.rs`: `finish_by_policy_records_reason_and_decided_by`, `finish_by_harness_sets_cancelled_and_error`, `cannot_finish_twice`, `decision_event_precedes_job_status`, `conclusion_messages_link_to_their_job`, `follow_up_job_links_parent_needs_input_job`, `enqueue_is_idempotent_on_key`, and:
    - `conclude_retry_returns_one_reply_and_one_status`: a second `conclude` with the same `idem_key` returns `job_already_finished` before writing; the job has one Dot reply, one `katwork.decision` and one `katwork.job_status`
    - `stopped_notice_names_the_dot_in_meta`: a `DotPaused` conclusion with `dot_id` set stores a notice whose body is `dot_paused` and whose `meta.dots` holds that Dot's id and name
    - `reply_with_nul_is_stored_with_replacement_character`: a `final_text` holding U+0000 is stored, and announced, with U+FFFD
    - `run_closed_precedes_run_finished`: an answered run's log ends with `katwork.run_closed {end: finished}` at seq n and `RUN_FINISHED` at n + 1
    - `latest_in_thread_orders_by_id_not_created_at`: of two jobs, the one with the higher id but the older `created_at` (set in SQL) is returned, with its newest task's `dot_id`
- [ ] **Step 2: Run** `cargo test -p katwork --test policy --test jobs`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: jobs, policy function, status transitions`.

### Task 11: Leases, worker pool and restart recovery

**Files:**
- Create: `crates/katwork/src/jobs/{lease,worker}.rs`, `crates/katwork/migrations/0007_task_first_claim.sql` (`ALTER TABLE tasks ADD COLUMN first_claimed_at TIMESTAMPTZ NULL`), `crates/katwork/tests/leases.rs`

Reference designs, not dependencies: apalis-postgres (a `SKIP LOCKED` claim with database-side eligibility and an attempt cap), underway (each reclaim starts a new attempt epoch, and a stale attempt's completion fails) and sqlxmq (transactional completion is exactly-once only when every effect is a database write). KatWork depends on none of them: its claim also applies the Dot's policy checks, closes runs with events, and fences every write by `lease_epoch`.

**Interfaces:**
- Consumes: `fence::{Fence, check, lock}` (Task 2); `TaskInput`, `job::{conclude, finish_by_harness}` (Task 10); `run::execute`, `RunAbort`, `RunEnv`, `RunInput`, `RunFacts`, `RunFacts::not_run`, `RunKind`, `Limits`, `StopReason`, `ToolRegistry::builtin` (Task 8); `PgEventSink`, `log::append`, `RunClosedEvent`, `Config.lock_timeout` (Task 9); `dot::get`, `dot::is_granted`, `dot::is_eligible` (Task 5); `thread::get` (Task 6); `space::get`, `tenancy::resolve` (Task 4); `Core`.
- Produces:
  - `lease::claim(core: &Core, worker_id: &str, ttl: Duration, limits: &Limits) -> Result<Option<ClaimedTask>, KatError>`. Candidates are tasks with `state = 'queued'`, or `state = 'leased'` with an expired lease, **whose job is still running** (`jobs.status IS NULL`), selected oldest first, in `(tasks.created_at, tasks.id)` order, with `FOR NO KEY UPDATE OF tasks, jobs SKIP LOCKED`; every time comes from Postgres, and lease expiry is compared with `clock_timestamp()`. That select joins `task_leases` without locking it, so its expiry test may be stale: a renewal committed after the select's snapshot but before it locked the task row changes no task row, so Postgres does not re-check it. Once the task row is locked, claim therefore re-reads the task and its lease in a separate statement (Global Constraints: lock first, then check) and goes on only if the task is still `queued`, or still `leased` with `expires_at <= clock_timestamp()`; otherwise it rolls that candidate back and moves to the next. Each candidate is handled in its own transaction. Oldest-first is best effort: because the select skips locked rows, a task may be overtaken only while an older one is locked by another worker (for example, one waiting for that task's thread lock). Claim never locks the Dot before a task, so it cannot enforce a strict per-Dot queue without breaking the global lock order. A branch that ends without a run commits and claim moves on to the next candidate (at most 16 per call); the first candidate that gets a lease is returned. Each candidate's transaction takes all its locks before any write, in the global order: the job and task rows from the `SKIP LOCKED` select (never waiting), then the thread row (`SELECT … FROM threads WHERE id = $1 FOR NO KEY UPDATE`), then the Dot row (`SELECT … FROM dots WHERE id = $1 FOR NO KEY UPDATE`), all held until commit. Every branch below, including the ones that only emit an event (recovery closing, `dot_busy`, `workspace_paused`), runs after those locks. Then, in order:
    - **closing an expired lease, once:** only a run of this task in state `running` is closed. For each of its open calls (started in `tool_calls` with no end) claim emits `TOOL_CALL_RESULT` and `katwork.tool_status{ok: false}` and marks the call `outcome_unknown`; it marks the run `interrupted`, emits `katwork.run_closed {end: interrupted, code: "lease_lost"}` immediately followed by `RUN_ERROR{code: "lease_lost"}`, before any later `RUN_STARTED`, and deletes the expired lease row. When no effect started (`effects_started_at IS NULL`), the task goes back to `queued` in the same transaction. Only `effects_started_at`, which Task 8 commits before any non-Read call runs, decides this; a missing tool result never re-queues a task, so an effect whose outcome a crash left unknown ends `interrupted`. A later poll then finds a queued task with no running run, so a task held by the branches below is never closed twice. A run leaves `running` only together with its terminal event, so this predicate finds exactly the runs that have none, including a run whose worker died after `execute` returned but before `lease::complete` committed.
    - **deadline:** a task whose `first_claimed_at` is set and is past `first_claimed_at + task_deadline`, or whose job is past `jobs.created_at + job_deadline`, both compared with `clock_timestamp()`, becomes `failed`. Time a task waits in the queue before its first claim never counts against its task deadline, only against its job's. The task's lease row is deleted, its lease row is deleted, and the job ends with `job::conclude` on `RunFacts::not_run(StopReason::TimeLimit)`
    - **retries exhausted:** at `attempts >= max_attempts` (3), the task becomes `failed`, its lease row is deleted, and the job ends with `finish_by_harness(tx, Error { code: RetriesExhausted })`
    - **effects started:** a task whose expired lease was just closed and whose `effects_started_at` is set becomes `interrupted` and ends the job with `job::conclude` on `RunFacts::not_run(StopReason::Interrupted)`
    - skips a task of a paused workspace and sets `hold_reason = 'workspace_paused'`
    - **eligibility,** against the values stored on the task, in `guards::authorize`'s order: `dot::is_granted` false for the thread's Space (a revoked grant) ends the job with `job::conclude` on `RunFacts::not_run(StopReason::PolicyChanged)`; granted but `dot::is_eligible` false (a paused Dot) ends it on `RunFacts::not_run(StopReason::DotPaused)`; otherwise a `policy_version` other than `tasks.dot_policy_version` ends it on `RunFacts::not_run(StopReason::PolicyChanged)`. Either way the task becomes `failed` and its lease row is deleted, so queued history never reaches a model after the Dot lost its authority.
    - **concurrency:** counts the Dot's **other** tasks with a live lease (`state = 'leased'`, lease `expires_at > clock_timestamp()`, a different task id); expired leases never count. At `max_concurrency` or more it skips the candidate and sets `hold_reason = 'dot_busy'`. Because the Dot row is held, a second claimer for the same Dot waits on it and then sees the first lease.
    - emits `katwork.job_status {status: null, holdReason}` when it first sets a hold reason
    - otherwise claims it: attempts + 1 (a `Recovery` run when `attempts > 0`, which covers a task re-queued by the closing above or by `lease::abandon`), increments `tasks.lease_epoch`, writes the lease with the same epoch, clears `hold_reason`, and sets `first_claimed_at = COALESCE(first_claimed_at, clock_timestamp())`, so the task deadline starts at the first claim and a recovery claim keeps it

    Because each no-run branch moves the task to a terminal state and finishes its job in the same transaction, a repeated poll never finishes the same job twice.
  - `ClaimedTask { task_id: TaskId, job_id: JobId, dot_id: DotId, workspace_id: WorkspaceId, thread_id: ThreadId, requested_by: UserId, input: TaskInput, attempts: i32, lease_epoch: LeaseEpoch, policy_version: i64, kind: RunKind, remaining: Duration }`. `policy_version` is the task's stored `dot_policy_version`. `remaining` comes from the claim transaction's last statement, `SELECT GREATEST(0, (EXTRACT(EPOCH FROM LEAST(t.first_claimed_at + $1 * INTERVAL '1 microsecond', j.created_at + $2 * INTERVAL '1 microsecond') - clock_timestamp()) * 1000000)::bigint) AS remaining_micros`, with the task and job deadlines bound as `i64` microseconds; it runs after the claim's update, so `first_claimed_at` is always set. It is decoded as `i64` and converted with `Duration::from_micros(u64::try_from(remaining_micros).unwrap_or(0))`. Because the statement runs last, the time spent acquiring the claim is already subtracted, and the worker's wall clock is never consulted. The deadline branch above uses the same bound microsecond values.
  - `HeartbeatStatus::{Continue, Stop(StopReason)}` and `lease::heartbeat(pool, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch, ttl: Duration) -> Result<HeartbeatStatus, KatError>`: takes `fence::check` (the task row, shared) and then updates the lease row, in the global order; it renews `expires_at = now() + ttl` only while the check passes, so an expired lease is never revived; `Stop(WorkspacePaused)` once the workspace is paused. A task that is no longer leased under this epoch (reclaimed, cancelled or finished by the harness) is `lease_lost`.
  - `lease::complete(core: &Core, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch, facts: RunFacts) -> Result<(), KatError>`: its transaction first reads the task's `job_id` and `thread_id` without locking, then takes, in the global order and before any write, the job row (`FOR NO KEY UPDATE`), the task row with `fence::lock`, and the thread row (`FOR NO KEY UPDATE`). A task that is no longer leased under this epoch is `lease_lost`, and the worker drops its facts, because whoever ended the lease (reclaim or cancel) already closed the run. Otherwise, in one transaction: `job::conclude` (with `idem_key` = the task id); the task state (`done`, or `interrupted` for `WorkspacePaused`) and `pending_tool_call`; the lease release. Then it commits and notifies the hub.
  - `worker::spawn_pool(core: Arc<Core>, n: usize, ttl: Duration) -> JoinSet<()>`. Each worker:
    - polls `claim` (every 1 s when idle)
    - builds the `Tenant` with `tenancy::resolve(&core.pool, requested_by, workspace_id)`, loads the Dot and the thread's Space policy, passes `policy_version` from the claim, and creates a `PgEventSink` for a new `RunId` with `core.config.lock_timeout` and `fence = Some(Fence { task_id, epoch })`
    - runs `run::execute` with `Limits::default()`, `ToolRegistry::builtin()`, `core.registry`, `reply_message_id = MessageId::new()` and `deadline = worker::run_deadline(claimed_at, remaining)`, where `claimed_at = tokio::time::Instant::now()` is sampled as soon as `claim` returns. `RunInput.history` is empty in this task; Task 12 loads it with `pipeline::load_history` right before `run::execute`. `serve` does not spawn the pool until Task 12, so no real provider sees an empty history.
    - heartbeats every `ttl / 3`; on `Stop(reason)` it cancels the run and sets `facts.stop = Some(reason)`. It cancels the run and drops its facts on `lease_lost`, on a failed renewal, or as soon as the last successful renewal is older than `ttl`.
    - on `Ok(facts)` finishes with `lease::complete`; on `Err(RunAbort::LeaseLost)` drops the run, because the reclaim or cancel path already closed it; on `Err(RunAbort::Infrastructure { .. })` calls `lease::abandon`
  - `worker::run_deadline(claimed_at: tokio::time::Instant, remaining: Duration) -> tokio::time::Instant`: `claimed_at + remaining`, the task and job bound; Task 8 shortens it to the selected entry's run wall clock. A zero `remaining` gives `claimed_at`, so the run stops `TimeLimit` before its first model attempt. Only the monotonic clock and the database-derived duration are used, so a skewed worker clock cannot move the deadline.
  - `lease::abandon(core: &Core, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch) -> Result<(), KatError>`: in a transaction that takes only `fence::lock` (the task row, then its lease row), increments `tasks.lease_epoch` and sets the lease's `expires_at = clock_timestamp()`, so the next `claim` applies the normal reclaim rules (re-queue when no effect started, otherwise `interrupted`, and `retries_exhausted` after the third attempt). An infrastructure failure is never turned into a policy verdict. If `abandon` fails as well, the lease simply expires.
  - TTL defaults to 60 s, with a heartbeat every 20 s.

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/leases.rs`:
  - `two_workers_never_claim_same_task`: 50 tasks, 2 workers in parallel, each task claimed once, and no task ever has more than one live lease
  - `expired_lease_is_reclaimed_once`: worker A claims and stops heartbeating; after the TTL, worker B claims it with a higher epoch; A's late `heartbeat` and `complete` are rejected with `lease_lost` and append no events
  - `crash_after_effect_marker_never_requeues`: a run commits `effects_started_at` through `ledger::start_tool_call` for a `WorkspaceWrite` call, then its worker dies without completing; two polls after the TTL leave the task `interrupted`, start no `Recovery` run, and the old run has exactly one terminal event
  - `renewal_committed_between_snapshot_and_lock_is_not_reclaimed`: a lease within 50 ms of expiry is renewed by its worker's `heartbeat` while another worker's `claim` starts, both released from a `tokio::sync::Barrier`, 200 times over. Whenever the renewal commits, the task is not reclaimed: its epoch is unchanged, it has one `runs` row, and the first worker's next `heartbeat` and `lease::complete` succeed
  - `claims_follow_queue_order`: with no other worker holding a task, a Dot's queued tasks are claimed in `(created_at, id)` order
  - `older_task_locked_by_another_worker_may_be_overtaken`: a second connection holds the oldest queued task's row `FOR NO KEY UPDATE`; claim skips it and leases the next task; once the lock is released, the next claim takes the oldest
  - `expired_lease_after_side_effect_is_interrupted_not_rerun`
  - `reclaim_does_not_duplicate_reply`
  - `reclaimed_run_closes_open_calls_before_new_run_started`
  - `worker_killed_between_execute_and_complete_closes_run_once`: `execute` returns facts and the worker stops before `lease::complete`; the reclaim emits exactly one `RUN_ERROR` for that run, and the recovery run ends with one `RUN_FINISHED`
  - `held_recovery_closes_old_run_once`: an expired lease while the workspace is paused; three polls emit one `RUN_ERROR` for the old run and leave the task `queued`; after unpausing, exactly one `Recovery` run starts
  - `retries_exhausted_finishes_job_error`
  - `cancelled_job_task_is_never_claimed`: a job cancelled while its task is queued is never claimed, and no model call happens
  - `deadline_branch_finishes_once_across_repeated_polls`: the task ends `failed` with no lease row, and one job status is written however often workers poll
  - `retries_exhausted_branch_finishes_once_across_repeated_polls`
  - `one_dot_runs_one_task_at_a_time`: a Dot with `max_concurrency = 1`
  - `lead_runs_three_tasks_at_once_specialists_one`: with the default roster's values (Task 5 follow-up), three tasks for the Lead are leased together and a fourth gets `dot_busy`, while a specialist's second task gets `dot_busy`
  - `expired_claim_racing_paused_dot_claim_does_not_deadlock`: one worker claims an expired task (whose recovery closing emits events) while another claims a fresh task of the same paused Dot in the same thread. The two calls start together from a `tokio::sync::Barrier`, 50 times over; both always finish, and no error carries SQLSTATE `40P01`
  - `simultaneous_claims_for_one_dot_dispatch_once`: two workers claim two different tasks of one Dot (`max_concurrency = 1`) at the same moment behind a barrier; exactly one lease is granted and the other task gets `dot_busy`
  - `expired_lease_does_not_block_reclaim_at_max_concurrency_one`: the Dot's only task has an expired lease; it is reclaimed rather than held as `dot_busy`
  - `paused_workspace_claims_nothing`
  - `dot_paused_while_queued_is_not_run`: the Dot is paused between enqueue and claim (which also bumps its `policy_version`); the task ends `failed`, the job `stopped/dot_paused`, not `policy_changed`, and the provider receives no request
  - `grant_revoked_while_queued_is_not_run`: the thread's Space grant is revoked between enqueue and claim; the job ends `stopped/policy_changed` and the provider receives no request
  - `infrastructure_abort_goes_through_reclaim`: an `Infrastructure` abort before any effect re-queues the task as a `Recovery`; the job gets no verdict from the aborted run
  - `worker_cancels_run_when_renewal_fails`: heartbeats start failing; the run is cancelled within `ttl`, before the lease can be reclaimed
  - `pause_between_attempts_before_heartbeat_has_one_terminal_run_event`: the workspace is paused after the first attempt and before the next heartbeat; `guards::authorize` stops the run, and the log holds exactly one terminal event for it, `RUN_FINISHED`, after `katwork.job_status stopped/workspace_paused`
  - `heartbeat_pause_has_one_terminal_run_event`: the heartbeat sees the pause and cancels the run mid-stream; the log holds exactly one terminal event for it, `RUN_ERROR{cancelled}`, and no `RUN_FINISHED`
  - `pausing_workspace_interrupts_running_task`: the task ends `interrupted` and the job `stopped/workspace_paused`
  - `task_past_deadline_finishes_time_limit`
  - `run_deadline_is_claimed_at_plus_remaining`: `remaining` of 10 s gives `claimed_at + 10 s`; zero gives `claimed_at`
  - `remaining_micros_from_sql_for_future_and_expired_deadlines`: runs the real claim query; a deadline 10 s ahead decodes to a `remaining` between 9 and 10 s, and a passed deadline decodes to zero
  - `deadline_comes_from_database_time`: a recovery claim of a task whose `first_claimed_at` is 4 min 50 s in the past by Postgres time gets `remaining` of about 10 s, whatever the worker's clock says
  - `task_deadline_starts_at_first_claim`: a task created 3 minutes ago by Postgres time and never claimed gets `remaining` between 4 min 59 s and 5 min at its first claim, which sets `first_claimed_at`; a later recovery claim keeps that value
  - `zero_remaining_makes_no_model_call`: a run started with `remaining = 0` stops `TimeLimit` and the provider receives no request
- [ ] **Step 2: Run** `cargo test -p katwork --test leases`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: leased worker pool with restart recovery`.

### Task 12: Message → job pipeline (SP1 routing)

**Files:**
- Create: `crates/katwork/src/pipeline.rs`, `crates/katwork/src/jobs/quota.rs`, `crates/katwork/migrations/0008_scheduler_indexes.sql`, `crates/katwork-server/src/routes/jobs.rs`, `crates/katwork-server/tests/pipeline.rs`
- Modify: `crates/katwork/src/jobs/job.rs` (`job::cancel`, `job::view`), `crates/katwork/src/jobs/worker.rs` (loads the history with `pipeline::load_history` right before `run::execute`), `crates/katwork/src/types.rs` (`ReasonCode::ExtraMentionIgnored`), `crates/katwork/src/team/team.rs` (`team::get_on`), `web/src/i18n/{en,th}.json` (`reason.extra_mention_ignored`), `crates/katwork/tests/schema.rs` (EXPLAIN tests), `crates/katwork-server/src/routes/threads.rs` (`POST` messages calls `pipeline::submit` and returns its `job_id`), `crates/katwork-server/src/main.rs` (`serve` spawns the worker pool)

**Interfaces:**
- Consumes: Tasks 4–11, in particular `mentions::resolve`, `UserPost`, `message::validate_user_post`, `Message`, `NoticeMeta`, `NoticeDot` (Task 6); `log::append`, `MessageStoredEvent`, `RunClosedEvent`, `db::set_lock_timeout`, `Config.lock_timeout`, `KatError::Busy` (Task 9); `JobSummary`, `TaskInput`, `Routing`, `RouteRule` (Task 10); `team::eligible_members`, `team::get`, `TeamView` (Task 5).
- Produces:
  - `ReasonCode` gains `ExtraMentionIgnored` (wire `extra_mention_ignored`), added to `ReasonCode::ALL`, with `reason.extra_mention_ignored` in both locales in the same PR (Task 13a's `reason_codes_have_en_and_th_messages`)
  - `team::get_on<'e, E: Executor<'e, Database = Postgres>>(executor: E, &Tenant, TeamId) -> Result<TeamView, KatError>`: `team::get` on any executor, which `team::get` now wraps, so intake reads the team's active members inside its transaction
  - `Submitted { message_id: MessageId, job_id: JobId, created: bool, last_event_seq: Option<EventSeq> }`. `last_event_seq` is the seq of intake's last event, `None` when `created` is false (a retry writes no event).
  - `pipeline::intake(tx: &mut Transaction<'_, Postgres>, core: &Core, tenant: &Tenant, thread_id: ThreadId, post: UserPost) -> Result<Submitted, KatError>`: the whole intake inside the caller's transaction. It takes its locks before any write, in the global order: the thread row first (`SELECT … FROM threads WHERE id = $1 AND workspace_id = $2 FOR NO KEY UPDATE`, which also serializes posts to the thread; no row is `NotFound`, so another workspace's thread id never takes a row lock), then, once routing has chosen a Dot, that Dot's row inside `quota::check`. Steps:
    1. `message::validate_user_post`; lock the thread row; then look up the user message for `(thread, author, client_msg_id)`. If it exists with the same body, return its stored `{message_id, job_id}` with `created: false`; because a message, its job and its task commit together, a stored message always has its `job_id`. If it exists with a different body, fail with 409 `client_msg_id_conflict` before any other work.
    2. reads only: `job::latest_in_thread`, then routing (below); if a Dot was chosen, `quota::check` locks its row and checks the quotas
    3. writes: `message::append` for the user message; `job::create_follow_up` when the latest job is `needs_input`, otherwise `job::create` (ADR-0002), where the unique `jobs.request_message_id` rejects any second job for the message; then at once `message::set_job` for the request message, whatever routing decided
    4. events and the rest, each through `log::append` in this transaction (the thread lock is already held, so the lock order is unchanged, and a rollback removes the events with the rows): `katwork.message_stored` for the request message, now carrying its `job_id` and `client_msg_id`, so a second tab or another member sees the question without reloading; then each notice with its own `katwork.message_stored`; then `task::enqueue` followed by `katwork.job_status {job_id, status: null}`, so a reloaded or second client can offer Stop for a queued job, or `job::conclude` for a job that ends at intake. Every message is written with `job_id` set to the job.
  - `pipeline::submit(core: &Core, tenant: &Tenant, thread_id: ThreadId, post: UserPost) -> Result<Submitted, KatError>`: begins a transaction, runs `db::set_lock_timeout(&mut tx, core.config.lock_timeout)`, runs `intake`, commits, then notifies the hub once with `last_event_seq` when it is set. A thread lock held past the timeout is 503 `busy` with `Retry-After: 1`, and the client's retry with the same `client_msg_id` is safe. A crash at any point before the commit leaves no message, job, task or event, so the retry starts clean; a concurrent retry waits on the thread lock, then finds the first post's message and returns its ids. The route answers 201 when `created`, otherwise 200.
  - Intake rules inside `intake`:
    - SP1 routing (spec §6.2 Stage 0 subset; Laya arrives in SP4). The eligible set is `team::eligible_members(conn, tenant, thread.team_id)` (Task 5): the thread team's active members whose Dot is `active` and granted to the team's Space, which is the thread's Space (Task 6), lead first, then by `position`. Mentions are `mentions::resolve(body, names)` (Task 6), where `names` are the names of all the team's active members, eligible or not, read with `team::get_on` on intake's transaction, so Thai names without a following space and multi-word names resolve; each hit is then classified. Only structure decides, never wording or model output:
      0. **follow-up:** when the latest job is `needs_input` (so `job::create_follow_up` applies) and no mention names an eligible Dot, the Dot that asked (`JobSummary.dot_id`) gets the task if it is still eligible (`RouteRule::FollowUp`); otherwise rules 3 and 4 apply. A bare "yes, option B" therefore goes back to the Dot that asked, not to the Lead.
      1. the first mention that names an eligible Dot gets the task (`RouteRule::Mention`). Every later mention of an eligible Dot is listed in `Routing.ignored` and named in one `System` notice `extra_mention_ignored`, because SP1 runs one Dot per job (team jobs arrive in SP4) and nothing is dropped silently.
      2. a mention of a team Dot that is not eligible posts a `System` notice, `dot_paused` or `dot_not_in_space`, naming that Dot, and matching moves on to the next mention
      3. with no eligible mention, the Lead gets the task if it is eligible (`RouteRule::Lead`), otherwise the first eligible Dot in team order, `team_members.position` (`RouteRule::FirstEligible`)
      4. with no eligible Dot at all, the job ends with `job::conclude` on `RunFacts::not_run(StopReason::NoEligibleDot)`
    - quotas before enqueue: `quota::check`; when it returns a stop reason (`QuotaExceeded` or `TooManyRequests`), the job ends with `job::conclude` on `RunFacts::not_run(that reason)`, which posts a localized notice. Nothing is dropped silently.
    - in a paused workspace the task is still enqueued and waits (Task 11), and the pipeline posts a `System` notice `workspace_paused`
    - notices: each has its code as `body` and `meta = NoticeMeta { code, dots }` naming its Dots. Every `job::conclude` call here uses the job id as `idem_key`. A notice about one Dot (`dot_paused`, `dot_not_in_space`) has the idempotency key `"notice:{job_id}:{code}:{dot_id}"`, so two paused Dots in one post get two notices; `extra_mention_ignored` and `workspace_paused` have `"notice:{job_id}:{code}"`. A retried post never doubles them.
    - builds `TaskInput { thread_id, request_message_id, routing, actor: tenant.user_id(), initiator: Initiator::Person }`, and calls `task::enqueue` with the routed Dot's `policy_version`, read after `quota::check` has locked the Dot row, and key `"job:{job_id}:dot:{dot_id}"`
    - a retried post returns at step 1, before `quota::check`, so it never counts against the throttle
  - `quota::check(conn: &mut PgConnection, dot: &Dot, sender: UserId) -> Result<Option<StopReason>, KatError>`, run inside intake's transaction:
    - it first locks the Dot row (`FOR NO KEY UPDATE`): the last lock intake takes, after the thread row and before any write
    - the daily budget is `dot.daily_token_budget` (`None` means no cap), compared with the sum of `model_calls.charged_tokens` for that Dot with `created_at > statement_timestamp() - interval '24 hours'`, so missing usage counts at its conservative charge, never as zero. The sum runs as its own statement after the Dot lock, so its one cutoff is computed after any lock wait and the `(dot_id, created_at)` index bounds the scan (Global Constraints). Over budget returns `QuotaExceeded`.
    - the per-sender throttle allows at most 6 jobs per minute per (Dot, sender), counted from tasks routed to that Dot whose job that sender requested with `jobs.created_at > statement_timestamp() - interval '60 seconds'`, also after the Dot lock. Over it returns `TooManyRequests` (`too_many_requests`), never `QuotaExceeded`.
    - intake's `task::enqueue` records the routing in the same transaction, so a concurrent submission for the same Dot waits on the Dot row and then counts it
  - `History { messages: Vec<ChatMsg>, cut: u32 }` and `pipeline::build_history(messages: &[Message], dot_id: DotId, budget_chars: usize) -> History`, pure, over messages already in history order with the request last:
    - `System` messages are dropped: a notice such as `dot_paused` never becomes a model turn. A Dot's question is a Dot message and stays.
    - the newest messages up to `budget_chars` (24,000) are kept; the last message, the request, is always kept whole, and `cut` counts the dropped ones
    - the Dot's own messages map to `Assistant`; other Dots and people map to `User` with `name = author_name`
  - `pipeline::load_history(core: &Core, tenant: &Tenant, thread_id: ThreadId, request_message_id: MessageId, dot_id: DotId, budget_chars: usize) -> Result<History, KatError>`: called by the worker right before `run::execute`, with `budget_chars = 24_000`, so a run sees what the thread holds when it starts. One query: a message's position is `coalesce(jobs.request_message_id, messages.id)`, joining `jobs` on `messages.job_id`; it keeps the thread's non-`System` messages whose position is at or before `request_message_id`, ordered by `(position, id)`. The request therefore comes last, later jobs' messages are left out, and a reply to an earlier job sits right after that job's request even though its id was minted at its run's start, after a later request's. A window sum of `char_length(body)` over `(position DESC, id DESC)` keeps only the rows within `budget_chars`, plus the request, so cut bodies never leave the database. It then calls `build_history` on the rows, which applies the same cut and maps the roles.
  - `job::cancel(pool, hub: &EventHub, tenant: &Tenant, job_id: JobId) -> Result<(), KatError>`: another workspace's job is `NotFound`; a finished job is `job_already_finished`. In one transaction it locks the job row (`FOR NO KEY UPDATE`); if the job is already finished it returns `job_already_finished` at once, before taking any other lock. Otherwise it locks, before any write and in the global order, the job's task rows in ascending id, then the thread row, all `FOR NO KEY UPDATE`. Each queued or leased task becomes `cancelled` and its lease row is deleted. For a running run it closes the open calls (`TOOL_CALL_RESULT`, `katwork.tool_status{ok: false}`, ledger state `outcome_unknown`), marks the run `cancelled`, and emits `katwork.dot_state idle`, then `katwork.run_closed {end: cancelled, code: "cancelled"}` immediately followed by `RUN_ERROR{code: "cancelled"}`. It then calls `finish_by_harness(tx, Cancelled)`, commits and notifies the hub. The old worker's next heartbeat or write finds the task no longer leased and gets `lease_lost`.
  - Route `POST /api/workspaces/{workspace_id}/jobs/{job_id}/cancel` returns 202, 404 across tenants, and 409 `job_already_finished`.
  - `JobView { id: JobId, status: Option<JobStatus>, reason: Option<ReasonCode>, hold_reason: Option<HoldReason>, queue_position: Option<u32> }` and `job::view(pool, &Tenant, JobId) -> Result<JobView, KatError>`. `queue_position` is set only while the job's task is `queued`: 1 plus the number of the same Dot's `queued` tasks of running jobs with a smaller `(created_at, id)`, so 1 means first in line. Claim takes queued tasks oldest first on a best-effort basis (Task 11), so the position is a good estimate, not a guarantee. It is null for a leased task or a finished job. Route `GET /api/workspaces/{workspace_id}/jobs/{job_id}` → 200 `JobView`, 404 across tenants; the chat shows the position (Task 13b).
  - Migration `0008_scheduler_indexes.sql`: `jobs_thread_id ON jobs (thread_id, id DESC)` for `job::latest_in_thread`, `jobs_requester_created ON jobs (requested_by, created_at, id)` for the per-sender throttle, and `tasks_job_dot ON tasks (job_id, dot_id)` for the throttle's join, `JobSummary.dot_id`, `job::cancel` and `job::view`. Task 6's `messages_thread_id` serves `load_history`.
  - `serve` builds `Core` and runs `worker::spawn_pool(core, 4, Duration::from_secs(60))`.
  - Event order for an answered job: `katwork.message_stored` (the request), `katwork.job_status` (status null), `RUN_STARTED`, `katwork.run_started`, `katwork.dot_state working`, `TEXT_MESSAGE_*`, `katwork.decision`, `katwork.message_stored` (the reply), `katwork.job_status answered`, `katwork.dot_state idle`, `katwork.run_closed`, `RUN_FINISHED`. `katwork.job_status` with a status is the only success signal; cancelled, needs-input and interrupted outcomes are explicit job statuses, never an omitted one.

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/pipeline.rs` (FakeProvider):
  - `message_without_mention_goes_to_lead_and_job_answered`
  - `mention_routes_to_named_dot_including_thai_name`
  - `mention_without_space_after_latin_name_routes`: `"@Writerช่วยสรุปไฟล์นี้"` creates the task for Writer
  - `routing_rule_is_recorded_on_the_task`: one case per `RouteRule`; the stored `TaskInput.routing.rule` matches, and `ignored` is empty except for the extra-mention case
  - `second_eligible_mention_gets_extra_mention_notice`: `"@Researcher find X, then @Writer draft it"` routes to the Researcher, `Routing.ignored` is `[Writer]`, and one `extra_mention_ignored` notice names Writer in `meta`
  - `two_paused_mentions_post_two_named_notices`: `"@Writer @Analyst"` with both paused gives two `dot_paused` notices whose `meta.dots` differ, and both idempotency keys are stored
  - `follow_up_without_mention_goes_to_the_asking_dot`, `follow_up_with_mention_goes_to_the_mentioned_dot` and `follow_up_when_asking_dot_paused_falls_back_to_lead`: after a Writer job ends `needs_input`, a bare `"ใช่ เอาแบบ B"` goes to Writer (`FollowUp`), `"@Analyst ..."` goes to the Analyst, and with Writer paused it goes to the Lead
  - `follow_up_to_approval_pending_job_never_dispatches_the_pending_call`: after a job ends `needs_input/approval_pending`, the next post starts a follow-up job, and the stored `pending_tool_call` is never dispatched
  - `latest_in_thread_uses_id_order_when_created_at_is_older`: transaction 1 holds the thread lock; transaction 2 begins first, so its `now()` is older, and waits on the lock; 1 commits, then 2; the job 2 created is `latest_in_thread`, and a follow-up decision uses it
  - `paused_dot_leaves_routing_set`
  - `provider_unavailable_ends_job_stopped_with_reason`: Review Focus 5; the notice code is `provider_unavailable` and the job finishes within the test timeout
  - `provider_quota_and_rate_limit_end_stopped_with_named_reason`
  - `local_only_space_with_only_hosted_keys_stops_residency_denied`: through the full pipeline; the notice code is `residency_denied` and no request leaves the install
  - `demo_mode_reply_is_flagged_synthetic`: the reply from the messages API has `synthetic: true`
  - `full_event_sequence`: the order above
  - `user_message_is_announced_with_client_msg_id_and_job_id`: a second subscriber sees `katwork.message_stored {author: user, clientMsgId, jobId}` and `katwork.job_status {status: null}` before any `RUN_STARTED`
  - `run_started_carries_the_job_id`: with two jobs in one thread, each run's `katwork.run_started` carries its own job's id
  - `stream_message_id_equals_stored_reply_id`
  - `stored_reply_snapshot_replaces_provisional_text`: the scripted deltas say "Draft" while the final turn says "Final"; `katwork.message_stored` and the stored message both carry "Final"
  - `restart_mid_run_posts_exactly_one_reply`
  - `retry_with_same_client_msg_id_creates_one_job`
  - `concurrent_retries_with_same_client_msg_id_create_one_job`: two posts with the same `client_msg_id` race; one message, one job and one task exist, and both responses carry the same ids
  - `intake_rolled_back_leaves_no_message_job_or_task`: runs `pipeline::intake` to its last step and drops the transaction without committing (a crash between any two steps rolls back the same way); no message, job or task remains, `events` and `threads.last_event_seq` are unchanged, and a retry then creates exactly one of each
  - `client_msg_id_reuse_with_different_body_is_409_and_creates_no_job`
  - `post_to_other_workspace_thread_is_404_without_waiting`: another transaction holds the foreign thread's row lock; the post returns 404 in under 200 ms
  - `post_fails_busy_when_thread_lock_is_held_past_timeout`: with `lock_timeout` 200 ms and the thread row held by another transaction, the post is 503 `busy` with `Retry-After`; after the release the same `client_msg_id` succeeds with one message and one job
  - `retry_after_intake_stop_returns_same_job`: a post whose job ends at intake (quota) is retried; the same job comes back, no second job or notice appears, and in the database the request message and the notice both carry that `job_id`
  - `reply_to_needs_input_job_starts_linked_follow_up`
  - `history_budget_keeps_latest_user_message_whole`
  - `other_dots_messages_are_user_role_with_name`
  - `queued_job_history_includes_earlier_reply_and_ends_with_request`: posts A, then B, to a Dot with `max_concurrency = 1`; job 2's loaded history is `[A, R1, B]`, with B last, although `R1.id > B.id`
  - `history_excludes_system_notices_and_later_jobs`: a quota notice and a later job's request and reply are absent from an earlier job's history
  - `history_query_stops_at_budget`: 100 messages of 30,000 characters each; the history query returns only the request's row
  - `seventh_message_in_a_minute_is_stopped_too_many_requests`: the notice code is `too_many_requests`, not `quota_exceeded`
  - `rate_limit_is_independent_per_dot_and_per_sender`: six jobs to Dot A from user 1 admit, a seventh is stopped; the same user to Dot B, and user 2 to Dot A, are admitted
  - `retried_post_does_not_consume_the_rate_limit`: five posts plus five retries of the first leave room for a sixth new post
  - `two_simultaneous_submissions_with_five_recent_jobs_admit_one`: one is enqueued and the other ends `stopped/too_many_requests`
  - `daily_budget_exceeded_is_stopped_quota_exceeded`
  - `job_view_shows_queue_position_while_queued`: two jobs queued behind a busy Dot report positions 1 and 2; once the first is claimed, the second reports 1; a running or finished job reports null
  - in `crates/katwork/tests/schema.rs`, each running `EXPLAIN` with `enable_seqscan = off` on the exact query the code runs: `latest_job_in_thread_uses_thread_index` (`jobs_thread_id`), `sender_throttle_uses_requester_and_task_indexes`, `cancel_finds_tasks_by_job_index`, `history_query_uses_thread_id_index` (`messages_thread_id`) and `daily_budget_index_cond_bounds_created_at`. Each asserts the index name and that the time column, where there is one, appears in the plan's `Index Cond`, not only in a `Filter`.
  - `daily_budget_counts_charged_tokens_when_usage_is_missing`
  - `first_eligible_mention_wins_among_several`
  - `paused_lead_routes_to_first_active_specialist`
  - `no_eligible_dot_ends_job_stopped`
  - `cancel_ends_job_cancelled`
  - `follow_up_intake_racing_late_cancel_does_not_deadlock`: a reply to a `needs_input` job, which inserts a follow-up job referencing that parent, and `job::cancel` on the parent start together from a `tokio::sync::Barrier`, 50 times over; the cancel returns `job_already_finished`, the intake commits, and no error carries SQLSTATE `40P01`
  - `cancel_racing_complete_does_not_deadlock`: `job::cancel` and the worker's `lease::complete` for the same job start together from a `tokio::sync::Barrier`, 50 times over; both always finish (one wins and the other gets `job_already_finished` or `lease_lost`), and no error carries SQLSTATE `40P01`
  - `quota_reject_racing_claim_does_not_deadlock`: an intake that ends `stopped/quota_exceeded` and a claim of another task of the same Dot in the same thread start together from a `tokio::sync::Barrier`, 50 times over; both always finish, and no error carries SQLSTATE `40P01`
  - `cancel_queued_job_terminalizes_its_task`: the task becomes `cancelled` with no lease row, and no worker ever claims it
  - `cancel_other_workspace_job_is_404`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test pipeline && cargo test -p katwork --test schema`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: message to job pipeline with mention routing`.

Task 13 is split in two PRs: 13a, the web shell, starts after Task 6 against mocked APIs; 13b adds the Team, Chat and Members pages against the real routes once Tasks 12 and 3b have merged.

### Task 13a: Web shell (login, reset, transcript and event folding)

**Files:**
- Create: `web/` (Vite React TS), `web/index.html` (with a pre-paint theme script), `web/scripts/check-contrast.ts`, `web/src/{main.tsx,app.tsx,router.tsx,api.ts,sse.ts,theme.css,theme.ts}`, `web/src/i18n/{index.ts,en.json,th.json}`, `web/src/components/{Layout,Preferences,Announce,Transcript,Composer}.tsx`, `web/src/pages/{Login,ResetRequest,ResetConfirm,Home}.tsx`, each with its `*.test.ts(x)`, `web/src/testing/fixtures/thai_emoji.sse` (the Thai and emoji SSE fixture, which Task 9 copies), `crates/katwork-server/src/static_files.rs`, `crates/katwork-server/tests/{i18n.rs,static_files.rs}`
- Modify: `crates/katwork-server/src/app.rs` (serve `web/dist` at `/`, SPA fallback), `Dockerfile` (web stage `node:22-bookworm-slim`), `.github/workflows/ci.yml` (`node-version: 22`, `npm run check:contrast`)

**Interfaces:**
- Consumes: the auth routes (Task 3), mocked threads, events and jobs routes, and `ReasonCode::ALL` as it is on main. It depends on nothing from Tasks 9 or 10: it owns its SSE fixture, and it translates only the reason codes already on main.
- Produces:
  - `Transcript` and `Composer` components, which Task 13b's Chat page uses, and `Home`, the signed-in placeholder that 13b replaces with Team and Chat
  - `sse.ts`: `subscribe(url, onEvent): () => void`, using the browser `EventSource` (no Rust or SDK client) and its native `Last-Event-ID` resume. It keeps the last seq and drops duplicates after a reconnect, and folds text by `messageId` and tool arguments by `toolCallId`, never into "the last message". It follows Task 9's folding rules: `TEXT_MESSAGE_START` resets a message's provisional text, `katwork.message_stored` replaces it with the stored body, `synthetic` flag and any question with its options, and `katwork.run_closed` drops that run's unconfirmed provisional message.
  - `i18n.t(key, vars?)` with locales `en` and `th`; the locale switch persists in `localStorage`, and the locale is read with try/catch
  - `i18n.formatDate(date, { calendar: 'gregory' | 'buddhist' })`: Gregorian in the UI chrome, Buddhist Era where a template asks for it
  - every `ReasonCode` on main has a `reason.<code>` key in both locales, checked against `ReasonCode::ALL` by `reason_codes_have_en_and_th_messages`. 13a does not add codes that are not on main yet; because that test runs on every PR, the PR that adds a code adds its two messages (Task 10 for `too_many_requests`)
  - CSS tokens in `theme.css`, redefined under `[data-theme=dark]`, and a pre-paint theme script in `index.html` that sets `data-theme` before first paint
  - `npm run check:contrast`: fails when any text/surface token pair has contrast below 4.5:1
  - copy calls the coworkers "Dots" in both locales; Thai copy describes them as ผู้ช่วย AI and never says พนักงาน AI

- [ ] **Step 1: Write the failing tests** (Vitest + Testing Library, and one Rust test):
  - `login_shows_localized_error_on_401`, in both `en` and `th`
  - `reset_request_shows_same_message_regardless_of_email`
  - `chat_streams_deltas_into_one_bubble`: a mocked EventSource with 3 deltas gives 1 bubble with the concatenated text
  - `chat_streams_thai_and_emoji_split_at_every_byte`: a mocked EventSource fed from `web/src/testing/fixtures/thai_emoji.sse`
  - `sse_folds_by_id_and_drops_duplicates_after_reconnect`
  - `provisional_text_dropped_after_lease_loss_and_recovery`: text from the first run, its `RUN_ERROR{lease_lost}` and `katwork.run_closed`, then a recovery run with its stored reply; the final transcript equals the stored messages
  - `interleaved_runs_one_error_one_stored_reply`: two runs for different Dots stream into one thread at once; one ends with `RUN_ERROR` and `katwork.run_closed`, the other with a stored reply; the other run's live text is never removed early, and the final transcript equals the stored messages
  - `provisional_text_dropped_after_provider_failure`: text, then the run ends `stopped/provider_unavailable` with no stored reply; the final transcript equals the stored messages (the notice only)
  - `final_text_differs_from_deltas_shows_stored_text`: through the real reducer
  - `new_turn_start_resets_provisional_text`
  - `ask_user_question_and_options_render_from_snapshot`
  - `synthetic_reply_shows_badge`: a message with `synthetic: true` shows a visible "Demo / ตัวอย่าง" badge; the same text with `synthetic: false` shows none
  - `replayed_run_error_cleared_by_later_success`
  - `interrupt_does_not_clear_run_error`
  - `composer_enter_during_ime_composition_does_not_send`
  - `transcript_is_live_region`
  - `format_date_buddhist_era_in_thai`
  - `every_key_exists_in_both_locales`: `en.json` and `th.json` have identical key sets
  - Rust, in `crates/katwork-server/tests/i18n.rs`: `reason_codes_have_en_and_th_messages`
  - Rust, in `crates/katwork-server/tests/static_files.rs`: `spa_routes_fall_back_to_index`, `api_paths_never_fall_back`, `missing_asset_is_404_not_index`, `static_responses_carry_security_headers`
- [ ] **Step 2: Run** `cd web && npm test`. Expected: FAIL.
- [ ] **Step 3: Implement** the pages and components, plain CSS with tokens, and keyboard-reachable controls. Serve `dist/` from Axum with SPA fallback; `/api/*` never falls back.
- [ ] **Step 4: Run** `npm test && npm run check:contrast && npm run build && cargo test -p katwork-server --locked`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat: web shell for login, reset and streaming transcript`.

### Task 13b: Team, Chat and Members pages

This task comes after Tasks 12 and 3b and builds on Task 13a. It adds no migration.

**Files:**
- Create: `web/src/pages/{Team,Chat,Members}.tsx` with their `*.test.tsx`, `web/src/testing/fake-event-source.ts` (a `FakeEventSource` fed through `eventsource-parser`), `crates/katwork/src/tenancy/members.rs`, `crates/katwork-server/src/routes/members.rs`, `crates/katwork-server/tests/members.rs`
- Modify: `web/package.json` and its lock file (`use-stick-to-bottom`; `eventsource-parser` as a dev-dependency; both MIT), `web/src/{router.tsx,api.ts,sse.ts}`, `web/src/components/{Layout,Transcript,Composer}.tsx`, `web/src/i18n/{en,th}.json`, `crates/katwork/src/{tenancy/mod.rs,auth/mailer.rs,error.rs}`, `crates/katwork-server/src/{app.rs,error.rs}`

**Interfaces:**
- Consumes: the workspaces, dots, teams, threads, events and jobs routes (Tasks 4–12), in particular the thread list and get routes, `MessagePage` with `event_seq` and each message's `job_status`, `author_name`, `meta` and `client_msg_id` (Task 6); the `?after=` cursor, the `ping`, `resync_required` and `stream_error` named events, `MessageStoredEvent.created_at` and `client_msg_id`, and `RunClosedEvent.code` (Task 9); intake's `katwork.job_status {status: null}` and `katwork.message_stored` for user messages, `JobView` and `GET …/jobs/{job_id}` (Task 12); `accounts::normalize_email`, `accounts::ensure_user`, `invite::issue`, `invite::link`, `Mailer`, `MailKind` (Tasks 3 and 3b); `Tenant`, `Role`, `WorkspaceSummary.role` (Task 4); `Transcript`, `Composer`, `sse.ts`, `api.ts`, `i18n` (Task 13a).
- Produces:
  - `Team.tsx`: the thread's team from `GET …/teams/{team_id}`, lead first, names in the stored script (Thai names shown as stored). `appearance.accent` is drawn only as a decorative ring, never as text or a text background: a `#RRGGBB` value directly, any other value (such as the roster's `blue`) through the theme's `--accent` token. An unknown `avatar` key shows a neutral avatar.
  - `sse.ts`: `subscribe(path: string, onEvent: (e: SeqEvent) => void, options: { after: number; onStatus?: (s: StreamStatus) => void }): () => void` replaces 13a's `subscribe(url, onEvent)`, with `StreamStatus = 'live' | 'reconnecting' | 'resync' | 'unauthenticated'`:
    - every open, the first included, creates `new EventSource(`${path}?after=${lastSeq}`)`, where `lastSeq` starts at `options.after`, because a new `EventSource` never sends `Last-Event-ID`; replayed seqs at or below `lastSeq` are dropped
    - when the browser gives up (`readyState === EventSource.CLOSED`, which any non-200 causes, such as Caddy's 502 during a restart), it reports `reconnecting`, calls `GET /api/auth/me` (a 401 reports `unauthenticated` and stops), and otherwise reopens after a backoff of 1 s doubling to 30 s, each wait drawn at random between half and all of it
    - a watchdog closes and reopens the stream when no frame, log event or `ping`, arrives for 35 s (two missed 15 s pings plus slack)
    - `resync_required` reports `resync`; `stream_error` with `unauthenticated` reports `unauthenticated`, and with `not_found` stops
  - `Chat.tsx`, at route `/chat/:threadId?`:
    - without a thread id it lists the Space's threads, newest first, from `GET …/spaces/{space_id}/threads`, and offers a new thread (`POST …/threads`); with one it loads `GET …/threads/{thread_id}` and `GET …/messages`, then subscribes with `after = event_seq`. `resync` reloads the messages and subscribes again with the new `event_seq`; `unauthenticated` routes to login.
    - one stream per open Chat page: opened on mount, closed on unmount (any route change) and after the page has been hidden for 60 s (`visibilitychange`), and reopened with `after` once it is visible again. Under React StrictMode's double mount exactly one stream stays open.
    - it posts with a fresh `client_msg_id` (a UUID v4 per composed message, reused on every retry of that message) and shows an optimistic bubble keyed by it, re-keyed to the message id by the 201/200 response or by a `katwork.message_stored` whose `clientMsgId` matches, whichever comes first. Other tabs and members see the message from that event.
    - the transcript sorts every bubble, stored or provisional, by message id, the order `message::list` uses (UUIDv7 ids in canonical lowercase form sort in time order as strings). A provisional bubble takes its place from `katwork.run_started.replyMessageId`, so a message posted during a run keeps one order live and after a reload. The time shown comes from `created_at` (REST) and `createdAt` (`katwork.message_stored`).
    - deltas render as received: Task 9's coalescer never releases part of a grapheme cluster or a lone Thai leading vowel, so the client never reveals text per character and never cuts it with `slice`. A delta for a message whose `TEXT_MESSAGE_START` this client never saw (it attached mid-run) is ignored, not thrown.
    - Stop is shown while some job of the thread has `status: null`, known from the messages' `job_status` and from `katwork.job_status` events (intake emits one per enqueued job), so it works after a reload and for a queued job. It calls the cancel route once (clicks while a cancel is pending are ignored), and a 409 `job_already_finished` is silent. A `katwork.run_closed` with code `cancelled` is shown as a neutral stop, never as an error.
    - when a job's latest `katwork.job_status` has `holdReason: "dot_busy"`, it fetches `GET …/jobs/{job_id}` and shows `chat.queue_position` (`Waiting in line: {position}` / `รอคิว ลำดับที่ {position}`), refreshing every 5 s until that job's next `katwork.job_status` or the first `katwork.run_started` whose `jobId` is that job
    - autoscroll uses `use-stick-to-bottom`: it follows new content until the person scrolls up, then shows a "jump to latest" button
    - markup: `role="log"` holds stored messages only; the provisional bubble sits in the same scroll area with `aria-hidden="true"` and is replaced by the stored node at `katwork.message_stored`, so a screen reader hears one addition per finished message. A separate `role="status"` line (`{dot} is replying` / `{dot} กำลังตอบ`) follows `katwork.dot_state`.
    - a question's options are actionable only while the question is the latest message and its job is `needs_input` with reason `question_pending`; otherwise they render read-only. What a click does is an open decision below.
    - `Composer` (13a's) sends on Enter only when `!shiftKey && !isComposing && keyCode !== 229`: Safari delivers the Enter that commits an IME composition after `compositionend`, with `isComposing: false` and `keyCode: 229`
    - `error.<code>` messages in both locales for `message_empty`, `message_invalid`, `message_too_large`, `thread_title_invalid`, `client_msg_id_conflict`, `busy` and `too_many_streams`
    - 13a's reducer comment and test fixtures that put `katwork.run_closed` after `RUN_ERROR` follow Task 9's order, `katwork.run_closed` first; the reducer reads a run's error code from `RunClosedEvent.code`
  - `Member { user_id: UserId, email: String, display_name: String, role: Role }` and `members::list(pool, &Tenant) -> Result<Vec<Member>, KatError>`: owner or admin only, ordered by email
  - `InviteDelivery::{Email, Link}` (serde `email | link`), `Invite { email: String, role: Role, delivery: InviteDelivery }` and `InviteResult { user_id: UserId, created: bool, link: Option<String> }`
  - `members::invite(pool, config: &Config, mailer: &dyn Mailer, &Tenant, Invite) -> Result<InviteResult, KatError>`, in one transaction that first locks the workspace row (`FOR NO KEY UPDATE`, the first class of the global lock order):
    - owner or admin only; only an owner may invite with `role = Owner`, otherwise `forbidden`
    - `delivery = Email` with SMTP unconfigured is 422 `smtp_unconfigured`, before anything is written; the demo console mailer prints only the demo account's mail, so it never counts as configured
    - `accounts::ensure_user` on `accounts::normalize_email(&invite.email)`; an account that is already a member is 409 `member_exists`
    - an existing account (`created = false`) gets the membership and nothing else: never a link, never a mail, so an admin can never take over an account that another workspace or the operator created (ADR-0004)
    - a new account gets the membership and `invite::issue`. `Link` returns `invite::link(config.public_url_or_default(), &token)` in `link`. `Email` returns `link: None` and, after the commit, sends `mailer::invite_mail` without delaying the response; a failed delivery is logged with the mail kind and outcome only, and the invite stands (the person can still use password reset).
  - `members::set_role(pool, &Tenant, UserId, Role) -> Result<(), KatError>`, in one transaction that first locks the workspace row: owner or admin only; only an owner may grant `Owner` or change an owner's role; demoting the workspace's last owner is 422 `last_owner_required`; a user who is not a member is 404 `not_found`. It applies on that user's next request, because `tenancy::resolve` reads the membership every time.
  - `MailKind::Invite` and `mailer::invite_mail(to: String, locale: Locale, link: &str, workspace: &str) -> Mail`: bilingual in the invitee's locale order, like the reset mail. Subject `คำเชิญเข้าร่วม KatWork / You're invited to KatWork` (halves swapped for `en`). Thai: `คุณได้รับเชิญเข้าร่วม {workspace} บน KatWork ใช้ลิงก์นี้ภายใน 72 ชั่วโมงเพื่อตั้งรหัสผ่าน`. English: `You have been invited to {workspace} on KatWork. Use this link within 72 hours to set your password.` The link stands alone on the last line.
  - New codes: `member_exists` (409), `last_owner_required` (422) and `smtp_unconfigured` (422), each with an `error.<code>` message in both locales
  - Routes, each calling `require_admin()` before parsing any body:
    - `GET /api/workspaces/{workspace_id}/members` → 200 `[Member]`
    - `POST /api/workspaces/{workspace_id}/members` with `Invite` → 201 `InviteResult`
    - `PUT /api/workspaces/{workspace_id}/members/{user_id}/role` with `{"role": Role}` → 204
  - `Members.tsx`, linked from `Layout` only when the signed-in user's role in the workspace (`WorkspaceSummary.role`) is owner or admin: the member list with a role select per member; an invite form with email, role, and a choice between "send by email" and "copy a link"; a returned link is shown once with a copy button and is gone once the person leaves the page; `smtp_unconfigured` shows a message that suggests the link option.

- [ ] **Step 1: Write the failing Team and Chat tests** (Vitest + Testing Library, mocked `fetch` and `EventSource`):
  - `team_page_lists_lead_first_with_thai_names`
  - `accent_not_hex_falls_back_to_token`: `#1A7F5A` draws the ring in that color; `blue` and `red; x` draw it with `--accent`, and the accent never colors text
  - `chat_posts_with_client_msg_id_and_streams_the_reply`
  - `chat_lists_threads_newest_first_and_reopens_one_after_reload`
  - `utf8_fixture_every_byte_split_reaches_one_bubble`: `thai_emoji.sse`, split at every byte offset, through a streaming `TextDecoder`, `eventsource-parser` and `FakeEventSource` into `sse.ts`; the final bubble text equals the source
  - `fresh_load_connects_with_after_param`: the first URL holds `after=<event_seq>` from the messages response
  - `reconnects_after_closed_with_after_param_and_no_duplicates`: fake timers; the fake reports `readyState = CLOSED`; after the backoff a new instance opens with `after=<last seq>`, and replayed seqs are dropped
  - `closed_stream_with_401_routes_to_login`
  - `stale_ping_triggers_reconnect`: no frame for 35 s closes the stream and reopens it with `after`
  - `resync_required_reloads_the_snapshot`
  - `leaving_chat_closes_stream`, `strict_mode_double_mount_leaves_one_open_stream` and `hidden_page_closes_stream_after_60s_and_resumes_with_after`
  - `optimistic_bubble_rekeys_on_message_stored_before_post_returns`
  - `second_tab_sees_user_message_from_event`
  - `message_posted_during_run_orders_same_live_and_after_reload`
  - `unknown_message_id_delta_is_ignored_not_thrown`
  - `stop_button_cancels_running_job`
  - `stop_enabled_after_reload_from_job_status_null`, `stop_409_job_already_finished_is_silent`, `stop_double_click_posts_once` and `cancelled_close_is_neutral_not_an_error`
  - `chat_shows_queue_position_while_the_dot_is_busy`: a `katwork.job_status {holdReason: "dot_busy"}` makes the chat fetch the job and show its `queue_position` in both locales; the line goes away at the first `katwork.run_started` whose `jobId` is that job, and another queued job's line in the same thread stays
  - `stale_question_options_are_disabled`: a question that is no longer the latest message, or whose job is no longer `needs_input/question_pending`, renders its options read-only
  - `transcript_log_excludes_provisional_text` and `typing_status_role_status`
  - `composer_ime_sequence_never_sends_early` (real DOM events): `compositionStart`; `keyDown {key: 'Enter', isComposing: true, keyCode: 229}`; `compositionEnd`; `keyDown {key: 'Enter', isComposing: false, keyCode: 229}` sends nothing; a following plain Enter sends once; Shift+Enter inserts a newline
- [ ] **Step 2: Run** `cd web && npm test`. Expected: FAIL.
- [ ] **Step 3: Implement** `Team.tsx`, `Chat.tsx` and the `sse.ts`, `Transcript` and `Composer` changes, replacing `Home`'s placeholders. Run again. Expected: PASS. Then make one manual pass of the Chat page with a screen reader and note the result in the PR.
- [ ] **Step 4: Members page and routes.** Write the failing tests:
  - in `crates/katwork-server/tests/members.rs`:
    - `member_cannot_list_invite_or_change_roles`: 403, with a valid and with an invalid body
    - `invite_new_email_by_link_returns_a_single_use_link`: the `/invite` link sets a password once through `POST /api/auth/invite/accept`, and a second accept is 400 `invite_token_invalid`; the new account then lists the workspace with the invited role
    - `invite_by_email_sends_a_bilingual_invite_mail`: with `CapturingMailer`; the response has no link
    - `invite_by_email_without_smtp_is_422_and_writes_nothing`
    - `invite_of_an_existing_account_adds_membership_without_a_link`: Review Focus 4; `link` is null, no mail is sent, and the account's password and sessions are unchanged
    - `invite_of_an_existing_member_is_409`
    - `only_an_owner_can_invite_or_promote_an_owner`
    - `last_owner_cannot_be_demoted`
    - `role_change_applies_on_the_next_request`
    - `concurrent_invites_for_one_email_create_one_account`
    - `members_routes_hide_other_workspaces`: 404
    - `invite_tokens_never_reach_logs`
  - in `web/src/pages/Members.test.tsx`: `members_page_is_offered_only_to_owners_and_admins`, `members_page_invites_by_link_and_shows_it_once`, `members_page_explains_email_invites_without_smtp`, `members_page_changes_a_role`

  Run `cargo test -p katwork-server --test members && (cd web && npm test)`. Expected: FAIL. Implement `tenancy/members.rs`, `routes/members.rs`, `MailKind::Invite` with `mailer::invite_mail`, the new codes and `Members.tsx`. Run again. Expected: PASS.
- [ ] **Step 5: Run** `npm test && npm run check:contrast && npm run build && cargo test --workspace --locked`. Expected: PASS.
- [ ] **Step 6: Commit** with `feat: team, chat and members pages`.

**Open decisions for the founder.** These are product choices, not technical ones; this plan does not decide them. Each answer becomes one more step in this task, with the change and the test it names.

1. **Does Stop erase the partial reply or keep it?** As written, Task 9's folding rule drops a run's provisional text at `katwork.run_closed`, and nothing more is shown. Erase: add a localized notice "Stopped; the partial reply was not saved" (test `stop_shows_notice_and_drops_partial_text`). Keep: Task 10 stores the partial text as the Dot's message when a job is cancelled (test `stop_keeps_partial_text_as_stored_message`).
2. **Does clicking a question option send immediately or prefill the composer?** Send: the option is posted as a normal message with a fresh `client_msg_id`, single-flight, then the group is disabled (test `option_click_is_single_flight`); it starts a follow-up job and spends tokens. Prefill: the option fills the composer and nothing is sent until the person sends it.
3. **Should the synthetic badge show at `run_started`?** As written, `synthetic` is known only at `katwork.message_stored`, so a demo reply streams without its badge. Showing it earlier adds `synthetic` to `RunStartedEvent` from the selected entry's `Capabilities.synthetic` (test `synthetic_badge_on_provisional_bubble`).
4. **One error slot per Dot?** As written, 13a's reducer keeps one `runError` per thread. Per Dot: a Dot's error stays until that Dot's next `answered` job, even while another Dot answers (test `error_slot_is_per_dot`).
5. **Is the Team page REST-only in SP1?** Events are per thread and there is no workspace stream, so a live "working" badge on the roster would need a new stream. REST-only: the page shows `Dot.status` (active, paused) and live state appears only in the Chat header (test `team_page_has_no_live_state_badge`).

### Task 14: Compose smoke, backup before migrate, first-run scripts, HTTPS and demo profiles

**Files:**
- Create: `scripts/smoke.sh`, `scripts/compose-backup.sh`, `scripts/init-env.sh`, `scripts/demo.sh`, `Caddyfile`, `docker-compose.smoke.yml`, `docker-compose.https.yml`, `docker-compose.demo.yml`, `crates/katwork-server/tests/e2e_compose.rs` (`#[ignore]`, run in a CI job)
- Modify: `docker-compose.yml`, `Dockerfile` (build argument `CARGO_FEATURES`), `crates/katwork/{Cargo.toml,src/models/fake.rs,src/models/registry.rs}` (the `smoke-fixtures` feature), `.env.example` (`KATWORK_HTTPS_HOST`), `.github/workflows/ci.yml` (`e2e` and `upgrade_from_previous_release` jobs), `README.md` (a "Try it" section, first run, HTTPS, the offline demo, backups and upgrades)

**Interfaces:**
- Consumes: everything above.
- Compose start order. The runtime image is distroless with no shell, so each step is its own service, and every step is gated by an explicit `depends_on` condition, because Compose otherwise starts services by order alone:
  0. `postgres` gets a healthcheck, `pg_isready -U katwork -d katwork`.
  1. `backup`: the `postgres:17` image on the `backend` network, run once (`restart: "no"`), with `depends_on: {postgres: {condition: service_healthy}}`. `scripts/compose-backup.sh` runs with `set -eu` and `pg_dump --format=custom` into the `./backups` bind mount, writing `/backups/katwork-<UTC timestamp>.dump.partial` and renaming it to `.dump` only after `pg_dump` succeeds, but only when the database already has KatWork's schema (`_sqlx_migrations` exists); on a fresh database it does nothing. Any failure exits non-zero, which stops `migrate` and `serve`: an upgrade never runs without its backup.
  2. `migrate`: the KatWork image with `command: ["migrate"]` and `restart: "no"`, with `depends_on: {backup: {condition: service_completed_successfully}}`.
  3. `katwork`: `serve`, with `depends_on: {migrate: {condition: service_completed_successfully}}`.
- A dump holds one database: `pg_dump` leaves out roles and tablespaces, so a restore first creates the `katwork` role and database, then runs `pg_restore --no-owner --dbname`. The README documents this, and the smoke test restores a real dump.
- Produces, for the restart scenario, a deterministic in-flight run:
  - the `katwork` crate gets a Cargo feature `smoke-fixtures`, off by default and never enabled in release images. With it, `FakeProvider::demo_held(hold: Duration) -> FakeProvider` behaves like `demo()`, except that each run's first attempt waits `hold` before its first chunk; `Registry::from_config` selects it when `KATWORK_SMOKE_HOLD_SECS` is set. Without the feature the variable is ignored.
  - the `Dockerfile` takes a build argument `CARGO_FEATURES` (empty by default) and passes it to `cargo build --features`
  - `docker-compose.smoke.yml` builds with `CARGO_FEATURES=katwork/smoke-fixtures`, sets `KATWORK_SMOKE_HOLD_SECS=20` (under the demo entry's 30 s first-chunk timeout) and `KATWORK_DEMO=1`, sets `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `KATWORK_LOCAL_BASE_URL` and `KATWORK_LOCAL_MODEL` to empty strings, so demo mode selects the fake even when the host has provider keys, and sets `RUST_LOG=debug,rig=trace`, so a broken `dependency_log_allowed` filter would show up in the logs; it also passes `KATWORK_ALLOW_DEMO_ON_HTTPS: ${KATWORK_ALLOW_DEMO_ON_HTTPS:-}`, which only the HTTPS scenario sets
- HTTPS and the offline demo are Compose override files, `docker-compose.https.yml` (the Caddy profile) and `docker-compose.demo.yml` (Steps 5 and 6), not Compose `profiles:` entries, because a profile cannot change another service's environment.

- [ ] **Step 1: Write `scripts/smoke.sh`.** It must:
  1. Before starting anything, check the first-run pieces (Steps 3–6):
     - `KATWORK_ENV_FILE=<temp dir>/.env scripts/init-env.sh` creates the file with mode `600`, a `KATWORK_SECRET` of at least 32 bytes and a `DATABASE_URL` that holds the generated `POSTGRES_PASSWORD`; a second run exits 1 and leaves the file unchanged
     - `docker compose --env-file /dev/null config -q` fails, and its message names `KATWORK_SECRET` and `scripts/init-env.sh`
     - `docker compose -f docker-compose.yml -f docker-compose.https.yml config` with `KATWORK_HTTPS_HOST=smoke.example` gives the `katwork` service `KATWORK_TRUSTED_PROXIES=172.31.250.2`, `KATWORK_PUBLIC_URL=https://smoke.example` and no `ports`, and the `caddy` service the volumes `caddy_data:/data` and `caddy_config:/config`
     - `docker compose -f docker-compose.yml -f docker-compose.demo.yml config` gives it `KATWORK_DEMO=1` and empty `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `KATWORK_LOCAL_BASE_URL` and `SMTP_HOST`
  2. Export `COMPOSE_FILE=docker-compose.yml:docker-compose.smoke.yml` once, so that every later `docker compose` command in the script, restarts included, uses the override. Then run `docker compose up -d --build` with `KATWORK_DEMO_PASSWORD` set, and assert the effective configuration: `docker inspect` on the `katwork` container shows `KATWORK_SMOKE_HOLD_SECS=20`, `KATWORK_DEMO=1`, `RUST_LOG=debug,rig=trace`, and empty `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `KATWORK_LOCAL_BASE_URL` and `KATWORK_LOCAL_MODEL`.
  3. Wait for `/healthz`.
  4. Run `seed-demo`.
  5. Log in as demo and post `"สวัสดี ช่วยแนะนำทีมหน่อย"` with `Origin: $KATWORK_PUBLIC_URL` and a `client_msg_id`.
  6. Read SSE until `katwork.job_status`.
  7. Assert status `answered`, and that the Dot reply from the messages API has `synthetic: true`. Assert that `docker compose logs katwork` has no line whose target starts with `rig` (Global Constraints).
  8. Scenario `restart_smoke_posts_one_reply`: post again, then poll Postgres (`docker compose exec -T postgres psql …`) until the task is `leased` and its `runs` row is `running`. Only then run `docker compose kill katwork` and `docker compose up -d katwork`, and assert the same effective configuration on the new container. After recovery, assert that:
     - a second `runs` row with `kind = 'recovery'` exists for the task, so a recovery attempt really happened
     - the job has exactly one Dot reply message
     - the job has exactly one `katwork.decision` event and one terminal `katwork.job_status`, and its status is `answered`
  9. Scenario `backup_archive_restores_seeded_data`: run `docker compose up -d` again on the existing volume and assert that a new `.dump` appeared in `./backups` and no `.partial` file remains. Start a disposable `postgres:17` container, create the `katwork` role and database, `pg_restore --no-owner --dbname` the new dump into it, and assert that `demo@katwork.local` exists and every `_sqlx_migrations` version of the live database is present. Remove the container.
  10. Scenario `backup_failure_blocks_migrate`: with a temporary third override whose `backup` service has `entrypoint: ["sh", "-c", "exit 1"]`, `docker compose up -d` on the existing volume fails; `docker compose ps -a` shows `backup` exited with 1 and `migrate` and `katwork` never started. Then bring the normal stack back up.
  11. Scenario `https_profile_survives_caddy_restart`: `docker compose down`, then start with `COMPOSE_FILE=docker-compose.yml:docker-compose.smoke.yml:docker-compose.https.yml`, `KATWORK_HTTPS_HOST=localhost` (Caddy then issues from its internal CA, with no network) and `KATWORK_ALLOW_DEMO_ON_HTTPS=1`. Assert that `https://localhost/healthz` is 200 (trusting the root from `caddy_data`), and that after a post through Caddy its first event arrives on the SSE stream within 1 s, uncompressed. Run `docker compose restart caddy`; `/healthz` is 200 again and the served certificate's SHA-256 fingerprint is unchanged, so Caddy's state persisted in `caddy_data`.
  12. Print `SMOKE OK`.
- [ ] **Step 2: Run** `scripts/smoke.sh`. Expected: FAIL.
- [ ] **Step 3: Write `scripts/init-env.sh [--https <host>]`.** It writes `.env` beside `docker-compose.yml`, or at `$KATWORK_ENV_FILE` when that is set, from `.env.example`: `KATWORK_SECRET` from `openssl rand -base64 48`, `POSTGRES_PASSWORD` from `openssl rand -hex 24` (hex, so it needs no escaping in a URL), and `DATABASE_URL=postgres://katwork:<that password>@postgres:5432/katwork`; every other variable keeps its `.env.example` value. It creates the file with mode `600` and refuses to overwrite an existing file (exit 1). With `--https <host>` it also writes `KATWORK_HTTPS_HOST=<host>` and `COMPOSE_FILE=docker-compose.yml:docker-compose.https.yml`. It prints only the path it wrote, never a secret.
- [ ] **Step 4: Compose required-value checks.** In `docker-compose.yml`, `DATABASE_URL`, `KATWORK_SECRET` and `POSTGRES_PASSWORD` (on both the `postgres` and `katwork` services) change from `${VAR:-}` to `${VAR:?set VAR in .env, or run scripts/init-env.sh}`, with the variable's own name in each message, so `docker compose` stops before starting anything when one is missing. Every optional variable keeps `${VAR:-}`. The CI job that runs `docker compose config -q` and `docker compose build` first creates its `.env` with `scripts/init-env.sh`.
- [ ] **Step 5: The Caddy profile, `docker-compose.https.yml`.** It adds a `caddy:2` service that publishes ports 80 and 443 and proxies `${KATWORK_HTTPS_HOST:?set KATWORK_HTTPS_HOST in .env}` to `katwork:3000` (`Caddyfile`), on a new network `edge` with subnet `172.31.250.0/29`, where caddy has the fixed address `172.31.250.2`, with the named volumes `caddy_data:/data` and `caddy_config:/config`, so certificates and keys survive a restart. It joins the `katwork` service to `edge`, removes its host port with `ports: !reset []` (Docker Compose 2.24.4 or newer, stated in the README), so behind Caddy the app is reachable only through it, and sets its `KATWORK_TRUSTED_PROXIES: 172.31.250.2` (a bare IP, Task 3b) and `KATWORK_PUBLIC_URL: https://${KATWORK_HTTPS_HOST}`, so the client IP behind Caddy is the real one and the session cookie is `Secure`. The `Caddyfile` has no `encode` directive: its default matcher covers `text/*`, which would compress `text/event-stream`; any later `encode` must exclude that type. Start it with `scripts/init-env.sh --https <host>`, then `docker compose up -d`.
- [ ] **Step 6: The offline demo profile, `docker-compose.demo.yml` and `scripts/demo.sh`.** The override sets `KATWORK_DEMO: "1"` and empty `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `KATWORK_LOCAL_BASE_URL`, `KATWORK_LOCAL_MODEL` and `SMTP_HOST`, so no request leaves the host. `scripts/demo.sh` is the one command: it runs `scripts/init-env.sh` when `.env` is missing, runs `docker compose -f docker-compose.yml -f docker-compose.demo.yml up -d`, waits for `/healthz`, runs `seed-demo` the way `scripts/smoke.sh` does, and prints `http://localhost:3000`, `demo@katwork.local` and the password `seed-demo` printed (or a note to use `KATWORK_DEMO_PASSWORD` when that is set). Once the images are built, it needs no network.
- [ ] **Step 7: Implement** the `backup` and `migrate` services, `scripts/compose-backup.sh`, and any glue the smoke test exposes. Document in `README.md`: first run (`scripts/init-env.sh`, then `create-user` for the first owner), and that `reset-link` can take over any account, so only operators may run commands on the host (ADR-0004); the local model variables, and that `KATWORK_LOCAL_BASE_URL` must point inside the install's network, because residency is what the operator declares (ADR-0005); HTTPS (Docker Compose 2.24.4 or newer for the override); the offline demo; backups, which hold one database and no roles, and restore: create the `katwork` role and database, then `pg_restore --no-owner --dbname`.
- [ ] **Step 8: Add the CI jobs.** `e2e` runs `scripts/smoke.sh`. `upgrade_from_previous_release` is skipped while no release tag exists; after the first tag, it starts the previous release's image, seeds demo data, then runs this build's `migrate` and the smoke test.
- [ ] **Step 9: Run** `scripts/smoke.sh`. Expected: `SMOKE OK`.
- [ ] **Step 10: Commit** with `test: compose smoke, backup before migrate, first-run scripts, https and demo profiles`.

---

## Dependency order and parallelism

```
1 → 2 → 3 → 4 → 5 → 6 → 9 → 8 → 10 → 11 → 12 → 13b → 14
                    6 → 3b ────────────────────↗   (3b after 6 and 13a, before 13b)
    2 → 7 → 7b ──────────↗                         (7b before 8)
                    6 → 13a ───────────────────↗   (13a before 13b)
```

- Task 7 (model gateway) depends only on Task 2: it takes `ModelTier` and `SpacePolicy` from `crate::types` and `Config` from Task 1. It can run in parallel with Tasks 3–6 and 9. Tasks 7 and 9 both add fields to `Core` in `runtime.rs`; whichever merges second rebases.
- Task 7b (local model provider) is a small PR after PR #21 (merged), and must merge before Task 8, which reads `ModelEntry.timeouts`.
- Task 3b (operator accounts and first-run convenience) comes after Task 4, can be written in parallel with Task 5, merges after Task 6 (its migration `0006` follows Task 6's `0005`) and PR #27 (Task 13a, whose `web/` its invite page joins), lands as its own PR, and must merge before Task 13b, whose Members page uses its account functions. Tasks 3b, 7b and 9 all edit `config.rs`; whichever of 3b and 7b merges second adopts `ConfigInvalid { var }`, and Task 9's `KATWORK_LOCK_TIMEOUT_MS` error uses it.
- Task 6 adds `unicode-segmentation` and makes Task 5's `name_key` crate-visible; Task 9 adds `tokio-util`, which Task 8 then uses. Task 9's `streams_through_trusted_proxy_peer_do_not_block_login` needs Task 3b's trusted-proxy exemption; whichever of Tasks 3b and 9 merges second carries that test.
- Task 5's follow-up PR (Steps 6–10: Lead concurrency default, reply-language line, Thai demo seed, default team for `create-user`'s new workspace) merges after PR #24 and Task 3b and before Task 11, whose `lead_runs_three_tasks_at_once_specialists_one` uses the Lead default.
- Task 9 comes before Task 8 because the agent loop emits through `EventSink`.
- Task 13a (PR #27) starts against mocked APIs after Task 6 and needs nothing from Tasks 9 or 10. Task 9 merges after it, because Task 9's SSE fixture is a copy of 13a's, and Task 10 adds its new reason code's messages to 13a's i18n files. Task 13b needs Tasks 12, 3b and 13a.
- Migration numbers follow this order (Global Constraints): 0004 (Task 5), 0005 (Task 6), 0006 (Task 3b), 0007 (Task 11), 0008 (Task 12).
