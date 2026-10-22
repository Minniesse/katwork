# Sub-project 1 — Core Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repo, tasks run as Orca workers; the model that didn't write a PR reviews it (spec §19.1).

**Goal:** A self-hosted KatWork server you can start with `docker compose up`. A person logs in (with password reset and demo accounts), sees a workspace with a default AI team, sends a chat message to a Dot, and watches a streamed reply. That reply is produced by a leased, restart-safe job running the Dot's agent loop through a model gateway.

**Architecture:**
- One Rust library crate, `katwork`, holds the config, the domain, persistence, auth, the model gateway, the agent loop, the policy function and the scheduler as focused modules. It never depends on axum.
- A binary crate, `katwork-server`, exposes them over Axum: a JSON API, AG-UI events over SSE, and the built React/Vite web app as static files. The axum extractors for `AuthUser` and `Tenant` live here.
- `katwork::runtime::Core` holds the shared runtime state: the pool, the config, the model registry and the event hub. The server, the worker pool and the pipeline all take it.
- State lives in PostgreSQL. The jobs, tasks and leases tables use `FOR UPDATE SKIP LOCKED`, and every lease time comes from Postgres `now()`.
- Every model call goes through a `ModelProvider` trait. Production uses a Rig adapter for model I/O only; KatWork owns the agent loop (ADR-0001). Tests and keyless demo mode use `FakeProvider`.
- The loop returns facts. One pure policy function turns facts into a job status (spec §5.1).

**Tech Stack:**
- Rust (edition 2024): tokio 1 (with `test-util` as a dev-dependency feature, for paused-clock tests), tokio-util (`CancellationToken`), axum 0.8, sqlx (exact release pinned at Task 2 start after a crates.io check; default features off, with postgres, runtime-tokio, tls-rustls, macros, uuid, chrono, json, migrate), serde/serde_json, uuid 1 (v7), chrono, argon2 0.5, lettre 0.11, rig-core (exact release checked and pinned at Task 7 start; used directly, with explicit reqwest and rustls features), reqwest (the version requirement Rig's reqwest transport declares, checked at Task 7 start, so the lock file holds one reqwest), async-trait, futures (`BoxStream`), unicode-segmentation, tracing, tracing-subscriber, thiserror 2.
- Web: React 19, Vite, TypeScript, Vitest, Node 22 LTS.
- Infrastructure: PostgreSQL 17, Qdrant (container only in SP1), Docker Compose with an optional Caddy HTTPS profile, GitHub Actions.

**Spec:** `docs/spec/katwork-design.md` (§2 decisions, §4 domain model and statuses, §5 architecture, §6 flow, §13 security, §15 testing, §19 sub-project 1).

**Decisions:** `docs/adr/0001-katwork-owns-the-agent-loop.md`, `docs/adr/0002-needs-input-is-final-in-sp1.md`, `docs/adr/0003-own-ag-ui-event-types.md`.

## Global Constraints

- All KatWork code is Rust; the UI is React + Vite in TypeScript, served by the Rust server (spec §2).
- Licence Apache-2.0. **No GPL/AGPL dependencies**: `cargo deny check licenses` runs in CI with those licences denied.
- Self-host via **Docker Compose** on one Linux host (spec §1.1 goal 1).
- Every cargo command in CI runs with `--locked`. The workspace `rust-version` is checked by the CI job `msrv` (Task 2).
- **Axum 0.8 path captures use `{name}`; `/:name` panics when the router is built.** Routes name their captures `{workspace_id}`, `{space_id}`, `{dot_id}`, `{team_id}`, `{thread_id}` and `{job_id}`; the `Tenant` extractor reads `{workspace_id}`.
- **English and Thai together.** Every user-facing string comes from `web/src/i18n/{en,th}.json`. API errors are JSON `{"code": "<stable_id>"}`, never prose; each task adds the `KatError` variants for the codes it names.
- **Words.** User-facing copy calls the coworkers "Dots" in both languages. Thai copy describes a Dot as ผู้ช่วย AI and the team as ทีม, and never says พนักงาน AI (spec §3).
- **The harness never runs model output through a shell, in the core or in checks; checks run as argv arrays. Tool ids are matched against a registry** (spec §5.1, §13). From SP2, commands inside a Dot's sandbox run only through the agentOS exec API, as a per-Dot capability that is off by default, bounded, gated like any tool call, and recorded.
- Every model call goes through `ModelProvider`; provider and Space data-residency rules are enforced in the gateway (spec §11).
- The job status set is exactly `accepted | ready | answered | stopped | needs_input | cancelled | error` (spec §4). Only the policy function sets the first five; the harness sets `cancelled` and `error`. In SP1 a job can end as `answered`, `stopped`, `needs_input`, `cancelled` or `error` (`ready` needs an artifact, which arrives in SP2). `needs_input` is final in SP1: the person's answer starts a follow-up job linked by `parent_job_id` (ADR-0002).
- Agent loop limits, initial defaults:
  - at most 24 steps per run, where a step is one model attempt, retries included
  - run wall-clock 90 s; token budget per run 200,000 input+output tokens charged over attempts (a missing count is charged conservatively, Task 8); task deadline 5 minutes; job deadline 10 minutes. Every model or tool call's timeout is `min(its own, the run's remaining time)`
  - tool timeout 60 s; a model stream gets 30 s to its first chunk and at most 30 s between chunks; at most 1 retry per model call, only before the first chunk
  - loop detection: the same tool with the same canonical arguments 3 times in a row, **or** 5 times within the last 10 calls, stops the run before that call runs
  - tool results capped at 8,000 Unicode scalar values, the truncation marker included
  - the history passed to a run is the newest messages up to 24,000 characters
- **Secrets.** Install secrets come only from environment variables; `.env.example` lists them and never holds values. `Config` never prints a secret (its `Debug` redacts them). Per-Dot credentials (SP2) live in an encrypted vault, never in env. Every child process gets an env allowlist, never the server's environment.
- **Demo mode with no keys must work.** Synthetic replies carry `messages.synthetic = true`, set only on the `FakeProvider::demo` path; the UI badge reads the flag (spec §15).
- **Reply vs send.** A *reply* goes to the thread the request came from; the harness fills the target and it is not gated. A *send* goes to any other target, or is a proactive post; it is `Outward` and produces a Proposal (SP3). In SP1 the only write is the reply, written once by `job::conclude` (Task 10).
- **Lock order.** Every transaction that waits for row locks takes them in one global order: workspace row → job row → task row, then its `task_leases` row → thread row (the row `log::append` updates for `last_event_seq`) → Dot row, then its `dot_spaces` rows. Within one class, rows are locked in ascending id. A path that needs a later lock takes every lock it needs at its start, in this order, with `SELECT … FOR UPDATE` (`FOR SHARE` when it only reads the row), before any write. A transaction that will update a row locks it `FOR UPDATE` from the start and never upgrades from `FOR SHARE`. Rows a transaction inserts are invisible to others until commit, so creating a message, job or task is not taking a lock, and a `SKIP LOCKED` select never waits. Every wait is therefore for a lock later in the order than any lock already held, so no cycle of waits, and no deadlock, can form. Tasks 2, 4 and 8–12 name the locks each path takes.
- Default team roles: Lead, Researcher, Writer, Analyst, Scheduler, Reviewer (spec §2).

## Review Focus

1. **Cross-tenant access.** A user of workspace A requests workspace B's Dots, threads, jobs or event stream by id. Expected: 404, never data. (Task 4, Task 9, Task 12.)
2. **Lease double-run and repeated side effects after a crash.** Two workers poll at the same time, and a worker dies mid-run. Expected: a task never runs twice at once. An expired lease is re-queued at most once, and only if the task has not started a non-Read side effect; otherwise it ends `interrupted`. A restart mid-run gives exactly one Dot reply and one job status, or `stopped/interrupted_review_required`. (Task 11, Task 12, Task 14.)
3. **SSE reconnect with `Last-Event-ID`.** The browser drops and reconnects mid-reply, including a Thai reply. Expected: no duplicate and no missing events, no gap when two appenders write to the same thread at once, and text never split inside a grapheme cluster. (Task 9.)
4. **Password reset misuse.**
   - A reset requested for an unknown email gets the same response as a known one.
   - A token used twice, or after 30 minutes, fails.
   - A reset ends all of that user's existing sessions.

   (Task 3.)
5. **Provider down, limited or no keys.** The model provider errors, times out, rate-limits, runs out of quota, rejects the key, or isn't configured. Expected: the job ends `stopped` with reason one of `provider_unavailable | provider_rate_limited | provider_quota | provider_auth_failed` within the timeout, the thread shows a localized message, nothing hangs. In demo mode the reply has `synthetic: true`. (Task 7, Task 12.)
6. **Tool gate bypass.** A tool outside the Dot's allowlist, a destructive or outward tool, a tool call from a failed or `MaxTokens` turn, a call after the allowlist changed mid-run, or a write repeated after its first outcome was lost. Expected: never dispatched; an error result, `needs_input/approval_pending`, `stopped/policy_changed` or `stopped/outcome_unknown`. (Task 8.)
7. **Cross-site and wrong-host requests.** A POST from another origin, or any request with an unknown `Host`. Expected: 403 before a handler runs. (Task 3.)
8. **Duplicate or stranded messages.** A client retries a post, two retries race, or the server crashes mid-intake. Expected: one message, one job and one task, or none at all; never a message without its job. (Task 12.)
9. **Deadlocks between terminal writers.** Cancel racing completion, a recovery claim racing a claim for a paused Dot, a quota rejection racing a claim. Expected: every path takes its locks in the global order, so none fails with SQLSTATE `40P01`. (Tasks 11 and 12.)

---

## File Structure

```
Cargo.toml                      workspace: crates/katwork, crates/katwork-server
rust-toolchain.toml             channel = "stable"; workspace rust-version checked by the msrv CI job
deny.toml                       licence + advisory policy (GPL/AGPL denied)
.env.example                    DATABASE_URL, KATWORK_SECRET, POSTGRES_PASSWORD, KATWORK_DEMO, KATWORK_PUBLIC_URL,
                                KATWORK_DEMO_PASSWORD, KATWORK_BIND, KATWORK_HTTPS_HOST, SMTP_*, OPENAI_*, ANTHROPIC_*
docker-compose.yml              postgres:17 + qdrant on an internal network; backup → migrate → katwork; https profile (caddy)
Caddyfile                       the optional https profile (Task 14)
Dockerfile                      multi-stage: web build (node 22) → cargo build --locked → distroless runtime
.github/workflows/ci.yml        fmt, clippy, test, msrv, deny, web, e2e
scripts/smoke.sh                compose smoke (Task 14)
scripts/compose-backup.sh       pg_dump before migrate, run by the compose backup service (Task 14)
crates/katwork/
  migrations/0001_init.sql      all SP1 tables (Task 2)
  src/lib.rs
  src/config.rs                 Config (Task 1)
  src/error.rs                  KatError + stable codes
  src/ids.rs                    typed UUIDv7 ids, EventSeq, LeaseEpoch
  src/types.rs                  shared domain types (ModelTier, SpacePolicy, Locale, DotStatus, SideEffect, ReasonCode)
  src/runtime.rs                Core
  src/fence.rs                  Fence: the lease check inside every run-scoped write
  src/db.rs                     pool + migrate
  src/auth/{mod,password,session,reset,mailer}.rs
  src/tenancy/{mod,workspace,space}.rs
  src/team/{mod,dot,team,roles}.rs
  src/chat/{mod,thread,message,mentions}.rs
  src/events/{mod,agui,log,hub,sink,coalesce}.rs
  src/models/{mod,provider,registry,guard,fake,rig_adapter}.rs
  src/agent/{mod,tool,ctx,registry,builtin,run,guards,ledger}.rs
  src/policy.rs                 decide_job: facts → verdict (pure)
  src/jobs/{mod,job,task,lease,worker,quota}.rs
  src/pipeline.rs               message → job → task (SP1 routing and history)
  tests/*.rs, tests/fixtures/{models,agui}/
crates/katwork-server/
  src/main.rs                   `serve` | `migrate` | `seed-demo [--reset-password]`
  src/lib.rs src/config.rs      config.rs re-exports katwork::config
  src/app.rs                    AppState { core: Arc<Core> }, build_router
  src/error.rs                  ApiError: KatError → HTTP status + {"code"}
  src/extract.rs                FromRequestParts<AppState> for AuthUser and Tenant
  src/guards.rs                 Host, Origin, Sec-Fetch-Site, JSON-only, body limit, security headers
  src/routes/{health,auth,workspaces,dots,teams,threads,events,jobs}.rs
  src/static_files.rs
  tests/*.rs                    HTTP-level tests via tower::ServiceExt; tests/fixtures/sse/
web/
  package.json vite.config.ts tsconfig.json index.html
  scripts/check-contrast.ts
  src/main.tsx src/app.tsx src/api.ts src/sse.ts src/theme.css
  src/i18n/{index.ts,en.json,th.json}
  src/pages/{Login,ResetRequest,ResetConfirm,Team,Chat}.tsx
  src/**/*.test.tsx
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
  - `Dockerfile`: a `node:20-bookworm-slim` web stage (Task 13 moves it to Node 22), `cargo build --locked --release -p katwork-server`, and a `gcr.io/distroless/cc-debian12:nonroot` runtime.
- Tests: `health_returns_ok_and_version`; `config_from_lookup_validates_required_values`, `optional_configuration_requires_host_or_api_key`, `optional_configuration_reads_all_fields`, `smtp_port_is_optional_and_must_be_a_valid_port_when_set`, `config_debug_redacts_secrets`, `redaction_handles_encoded_query_and_malformed_database_passwords`; `maintenance_commands_are_stubs`, `rejects_unknown_commands_and_extra_arguments`, `serve_rejects_missing_configuration`, `serve_reports_database_cause_without_password`.
- Left for later tasks: the MSRV CI job (Task 2), HTTP guards (Task 3), Node 22 (Task 13), backup before migrate and the HTTPS profile (Task 14).

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
  - `types::SideEffect::{Read, Navigate, WorkspaceRead, Draft, WorkspaceWrite, Exec, CredentialUse, Destructive, Outward(OutwardKind)}` and `types::OutwardKind::{Send, Publish, Pay, Share, ExternalApi}`, with `SideEffect::wire(&self) -> &'static str` giving `read, navigate, workspace_read, draft, workspace_write, exec, credential_use, destructive, outward_send, outward_publish, outward_pay, outward_share, outward_external_api`
  - `types::ReasonCode` with `wire(&self) -> &'static str` and `ReasonCode::ALL`: `provider_unavailable, provider_rate_limited, provider_quota, provider_auth_failed, model_protocol_error, time_limit, quota_exceeded, workspace_paused, step_limit, token_budget, output_truncated, residency_denied, loop_detected, policy_changed, approval_pending, question_pending, interrupted_review_required, dot_paused, retries_exhausted, cancelled, outcome_unknown, no_eligible_dot, dot_not_in_space, lease_lost`. Each value is a job `reason`, a notice body or a `RUN_ERROR` code, and has a message in both locales (Task 13).
  - `db::connect(url: &str) -> Result<PgPool, KatError>` and `db::migrate(pool: &PgPool) -> Result<(), KatError>`
  - `fence::Fence { task_id: TaskId, epoch: LeaseEpoch }` and `fence::check(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError>`. Run inside the caller's write transaction, it selects the task joined to its lease where `tasks.state = 'leased'`, `tasks.lease_epoch` and `task_leases.lease_epoch` both equal `f.epoch`, and `expires_at > now()`, taking `FOR SHARE OF tasks`; no row is `lease_lost`. The share lock lasts until the write commits. Claim, cancel and every terminal transition take `FOR UPDATE` on the task row, so they wait for an in-flight fenced write, and any write after them fails the check. `fence::lock(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError>` is the same check taking `FOR UPDATE OF tasks`, for a caller that will update the task row (`ledger::start_tool_call` for a non-Read call, `lease::complete`, `lease::abandon`), because a lock is never upgraded from `FOR SHARE`.
  - `runtime::Core { pool: PgPool, config: Arc<Config> }` and `Core::new(pool: PgPool, config: Arc<Config>) -> Core`. Task 7 adds `registry: Arc<Registry>` and Task 9 adds `events: EventHub`; both are filled inside `Core::new`, so its signature never changes.
  - `katwork_server::app::AppState { core: Arc<Core> }`
  - `katwork_server::error::ApiError(pub KatError)` with `From<KatError>` and `IntoResponse`: the body is `{"code": e.code()}` and the status comes from one `match` that each later task extends
- Tables, all in `0001_init.sql`. Every tenant-owned row carries `workspace_id UUID NOT NULL` with an FK.
  - `workspaces(id, name, paused BOOLEAN NOT NULL DEFAULT false, created_at)`
  - `users(id, email UNIQUE, password_hash, display_name, locale, created_at)`; `memberships(workspace_id, user_id, role, PRIMARY KEY (workspace_id, user_id))` with `role IN ('owner','admin','member')`; `sessions(id, user_id, token_sha256 UNIQUE, created_at, expires_at, revoked_at NULL)`; `password_resets(id, user_id, token_sha256 UNIQUE, expires_at, used_at NULL)`
  - `spaces(id, workspace_id, name, allow_external BOOLEAN NOT NULL)`, with no column default
  - `dots(id, workspace_id, default_space_id UUID NOT NULL, name, role, persona, instructions, tool_allowlist TEXT[] NOT NULL, model_tier, autonomy, appearance JSONB NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','paused')), max_concurrency INT NOT NULL DEFAULT 1 CHECK (max_concurrency > 0), policy_version BIGINT NOT NULL DEFAULT 1, daily_token_budget BIGINT NULL)`; a NULL budget means no cap
  - `dot_spaces(workspace_id, dot_id, space_id, PRIMARY KEY (dot_id, space_id))`, then `ALTER TABLE dots ADD FOREIGN KEY (id, default_space_id) REFERENCES dot_spaces(dot_id, space_id) DEFERRABLE INITIALLY DEFERRED`, so a Dot's default Space is always among its grants
  - `teams(id, workspace_id, space_id, name)`; `team_members(team_id, dot_id, is_lead, position)` with a partial unique index that allows one lead per team
  - `threads(id, workspace_id, space_id, team_id, title, last_event_seq BIGINT NOT NULL DEFAULT 0)`
  - `messages(id, workspace_id, thread_id, author_kind, author_user_id NULL, author_dot_id NULL, body, client_msg_id TEXT NULL CHECK (char_length(client_msg_id) <= 64), idempotency_key TEXT NULL UNIQUE, synthetic BOOLEAN NOT NULL DEFAULT false, question JSONB NULL, job_id UUID NULL, created_at, UNIQUE (thread_id, author_user_id, client_msg_id))`; `question` holds a `Question` when the message asks one; the `job_id` FK is added once `jobs` exists
  - `jobs(id, workspace_id, thread_id, request_message_id UUID NOT NULL UNIQUE REFERENCES messages(id), requested_by UUID NOT NULL REFERENCES users(id), parent_job_id UUID NULL REFERENCES jobs(id), status NULL, reason TEXT NULL, decided_by TEXT NULL CHECK (decided_by IN ('policy','harness')), decision_seq BIGINT NULL, contract_sha256 TEXT NULL, created_at, finished_at NULL)`
    - `status` NULL means running, with `CHECK (status IS NULL OR status IN ('accepted','ready','answered','stopped','needs_input','cancelled','error'))`
    - `reason` holds a `ReasonCode` wire value; `decision_seq` is the per-thread seq of the job's `katwork.decision` event; `contract_sha256` is reserved and always NULL in SP1
    - one job per request message: the unique `request_message_id` means a retried or concurrent intake can never create a second job
  - `tasks(id, workspace_id, job_id, dot_id, dot_policy_version BIGINT NOT NULL, input JSONB, state, attempts INT NOT NULL DEFAULT 0, max_attempts INT NOT NULL DEFAULT 3, idempotency_key TEXT UNIQUE, effects_started_at TIMESTAMPTZ NULL, hold_reason TEXT NULL, lease_epoch BIGINT NOT NULL DEFAULT 0, pending_tool_call JSONB NULL, created_at)`
    - `CHECK (state IN ('queued','leased','done','failed','interrupted','cancelled'))` and `CHECK (hold_reason IN ('dot_busy','provider_quota','workspace_paused'))`
    - terminal states: `done` (concluded by the policy after a run), `failed` (the harness gave up: deadline or retries exhausted), `interrupted` (stopped after a side effect, or by a workspace pause), `cancelled` (its job was cancelled). Every move to a terminal state deletes the task's `task_leases` row in the same transaction.
    - `dot_policy_version` is the Dot's `policy_version` when the task was routed; claim and the loop compare against it, never against whatever the Dot holds when the run starts
    - `pending_tool_call` is `{tool_call_record_id, tool_call_id, name, args_canonical, args_sha256}`, kept when a run stops on `NeedsApproval`: the exact call, with its canonical arguments, bound to its ledger row, so sub-project 3 can rebuild and resume it after a restart (ADR-0002)
  - `task_leases(task_id PRIMARY KEY, worker_id, lease_epoch BIGINT NOT NULL, expires_at)`; `expires_at` is always computed with Postgres `now()`, never the worker clock
  - `runs(id, workspace_id, job_id, task_id, dot_id, kind, lease_epoch BIGINT NOT NULL, dot_config_hash TEXT NOT NULL, model TEXT NULL, state, stop_reason TEXT NULL, started_at, ended_at NULL)` with `kind IN ('initial','repair','recovery')` and `state IN ('running','finished','interrupted','cancelled','error')`. Repairs (at most 2, set by the policy from SP3) and recoveries (owned by the scheduler) are counted separately through `kind`.
  - `tool_calls(id, workspace_id, run_id, step INT, tool_call_id TEXT, tool_name TEXT, side_effect TEXT NULL, args_canonical TEXT NOT NULL, args_sha256 TEXT, state TEXT, attempts INT NOT NULL DEFAULT 1 CHECK (attempts IN (1, 2)), result TEXT NULL, started_at, ended_at NULL, UNIQUE (run_id, step, tool_call_id))`, with `side_effect` checked against the 13 `SideEffect::wire` values, `CHECK (side_effect IS NOT NULL OR state = 'refused')` (an unknown tool has no effect), and `state IN ('started','succeeded','failed','timed_out','outcome_unknown','refused')`. `result` holds the exact text passed to the next model call; `attempts` is 2 only after a `Read` retry, which reuses the row.
  - `model_calls(id, workspace_id, space_id, job_id, task_id, run_id, dot_id, provider_id, model, residency, attempt INT, input_tokens BIGINT NULL, cache_read_tokens BIGINT NULL, cache_write_tokens BIGINT NULL, output_tokens BIGINT NULL, usage_complete BOOLEAN NOT NULL, charged_tokens BIGINT NOT NULL, cost_micros BIGINT NULL, latency_ms INT, outcome TEXT, created_at)`: one row per model attempt, retries included; a missing count is NULL, never 0; `charged_tokens` is the attempt's budget charge (Task 8) and is never NULL
  - `events(thread_id, seq BIGINT, workspace_id, run_id NULL, kind, payload JSONB, created_at, PRIMARY KEY (thread_id, seq))`

- [ ] **Step 1: Pin versions and check the MSRV.** Check crates.io for the latest released sqlx and pin it exactly (`=x.y.z`) in `[workspace.dependencies]` with the features above; `macros` must be listed, because default features are off and `migrate` does not enable `#[sqlx::test]` or the `sqlx::Type` derive. Add `uuid` (v7, serde), `chrono` (serde), `serde`, and tokio's `test-util` feature under `[dev-dependencies]`. Add the CI job `msrv`: `cargo +<rust-version> check --workspace --locked`. If the locked graph needs a newer compiler than 1.85 (let-chains under edition 2024 need 1.88), raise `rust-version` in `Cargo.toml` to the lowest version that passes and use the same value in the job.
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
  - `missing_usage_is_stored_as_null`
  - `space_allow_external_has_no_default`: an insert without `allow_external` errors
  - `side_effect_wire_values_pass_the_schema_check`: every `SideEffect::wire` value inserts into `tool_calls.side_effect`, and `'shell'` is rejected
  - `only_refused_calls_may_have_no_side_effect`
- [ ] **Step 3: Run** `cargo test -p katwork --test schema`. Expected: FAIL.
- [ ] **Step 4: Implement** the migration, the ids, `types.rs`, `db.rs`, `runtime.rs` and `ApiError`. Wire `katwork-server migrate` to `db::connect` + `db::migrate`, and make `serve` build `Core`. Switch `AppState` to `{ core: Arc<Core> }` and update `tests/health.rs`. In `tests/cli.rs`, `maintenance_commands_are_stubs` keeps only `seed-demo`.
- [ ] **Step 5: Run** `cargo test --workspace --locked`. Expected: PASS.
- [ ] **Step 6: Commit** with `feat: initial schema, typed ids, shared types`.

### Task 3: HTTP guards, password auth, sessions, reset, demo accounts

**Files:**
- Create: `crates/katwork/src/auth/{mod,password,session,reset,mailer}.rs`, `crates/katwork-server/src/{extract.rs,guards.rs,routes/auth.rs}`, `crates/katwork-server/tests/{http_guards.rs,auth.rs}`
- Modify: `crates/katwork/src/config.rs`, `crates/katwork-server/src/{app.rs,main.rs}`, `crates/katwork-server/tests/{health.rs,cli.rs}`

**Interfaces:**
- Consumes: Task 2 tables and ids, `Core`, `AppState`, `ApiError`.
- Produces:
  - `Config::public_url_or_default(&self) -> &str`, which is `http://localhost:3000` when `public_url` is unset, and `Config::cookie_secure(&self) -> bool`, true when that URL starts with `https://`
  - `katwork_server::guards::apply(router: Router, config: &Config) -> Router`, applied by `build_router`:
    - a Host allowlist: the hostname of `public_url_or_default()` plus `localhost` (port ignored); any other or missing `Host` is 403 `host_not_allowed`
    - a request that is not GET or HEAD needs an `Origin` exactly equal to the public URL's origin, otherwise 403 `cross_site_request`; `Sec-Fetch-Site: cross-site` on such a request is also 403 `cross_site_request`
    - a mutation with a body must be `application/json`, otherwise 415 `json_required`
    - `DefaultBodyLimit::max(1_048_576)`
    - every response carries `X-Content-Type-Options: nosniff` and `Content-Security-Policy: default-src 'self'; frame-ancestors 'none'`
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
- `seed-demo` creates `demo@katwork.local` and a demo workspace, idempotently. The password is `KATWORK_DEMO_PASSWORD` when set; otherwise it is random and printed when the user is first created. `seed-demo --reset-password` sets the password again (from `KATWORK_DEMO_PASSWORD`, or a new random one) and prints it.

- [ ] **Step 1: Write the failing guard tests** in `crates/katwork-server/tests/http_guards.rs`:
  - `cross_site_post_is_403`: `Origin: https://evil.example` gives 403 `cross_site_request`
  - `unrecognized_host_is_403`: `Host: evil.example` gives 403 `host_not_allowed`
  - `non_json_mutation_is_415`
  - `responses_carry_nosniff_and_csp`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test http_guards`. Expected: FAIL.
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
- [ ] **Step 5: Run** `cargo test -p katwork-server --test auth`. Expected: FAIL.
- [ ] **Step 6: Implement** the modules and routes. Reset tokens are 32 random bytes in URL-safe base64, stored hashed, with `expires_at = now() + 30 min` and `used_at`; the emailed link is `{public_url}/reset?token=…`. Login is rate-limited to 10 attempts per 15 minutes per email+IP in memory. Delete `maintenance_commands_are_stubs` from `tests/cli.rs`.
- [ ] **Step 7: Run.** Expected: PASS.
- [ ] **Step 8: Commit** with `feat: http guards, password auth, sessions, reset, demo seed`.

### Task 4: Workspaces, Spaces and tenant scoping

**Files:**
- Create: `crates/katwork/src/tenancy/{mod,workspace,space}.rs`, `crates/katwork-server/src/routes/workspaces.rs`, `crates/katwork-server/tests/tenancy.rs`
- Modify: `crates/katwork-server/src/{extract.rs,main.rs}`

**Interfaces:**
- Consumes: `AuthUser`, `types::SpacePolicy`.
- Produces:
  - `katwork::tenancy::Tenant { workspace_id: WorkspaceId, user_id: UserId, role: Role }` and `Role::{Owner, Admin, Member}`
  - `tenancy::resolve(pool, user_id: UserId, workspace_id: WorkspaceId) -> Result<Tenant, KatError>`: a non-member gets `NotFound`. The extractor uses it for the signed-in user, and the worker (Task 11) for a job's requester.
  - `impl FromRequestParts<AppState> for Tenant` in `extract.rs`: reads `{workspace_id}`, requires `AuthUser`, calls `resolve`; a non-member gets 404 `not_found`
  - `workspace::create(pool, name: &str, owner: UserId) -> Result<WorkspaceId, KatError>`
  - `workspace::list_for(pool, user_id: UserId) -> Result<Vec<WorkspaceSummary>, KatError>` and `WorkspaceSummary { id: WorkspaceId, name: String, role: Role, paused: bool }`
  - `workspace::set_paused(pool, &Tenant, paused: bool) -> Result<(), KatError>`: owner or admin, otherwise `forbidden`. It locks and updates only the workspace row, the first class in the lock order. Running tasks see the pause through `guards::authorize` or their heartbeat and end through `lease::complete` (Tasks 8 and 11).
  - `space::create(pool, &Tenant, name: &str, policy: SpacePolicy) -> Result<SpaceId, KatError>`: owner or admin
  - `space::list(pool, &Tenant) -> Result<Vec<Space>, KatError>` and `space::get(pool, &Tenant, SpaceId) -> Result<Space, KatError>`
  - `Space { id: SpaceId, name: String, policy: SpacePolicy }`
- Routes:
  - `GET/POST /api/workspaces`
  - `GET/POST /api/workspaces/{workspace_id}/spaces`. `POST` takes `{name, allow_external}`; without `allow_external` it returns 422 `space_policy_required`, because the admin chooses local-only or hosted-allowed for every Space (spec §11).
  - `PUT /api/workspaces/{workspace_id}/paused` with `{"paused": bool}`
- `seed-demo` creates the demo Space with `allow_external = true`.

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/tenancy.rs`:
  - `member_lists_own_spaces`
  - `non_member_gets_404_not_403`: Review Focus 1
  - `member_cannot_create_space_admin_can`
  - `create_space_without_policy_is_422`
  - `seed_demo_space_allows_external`
  - `member_cannot_pause_workspace_admin_can`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test tenancy`. Expected: FAIL.
- [ ] **Step 3: Implement.** Every repository function takes `&Tenant` and filters by `workspace_id` in SQL.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: workspaces, spaces, tenant scoping`.

### Task 5: Dots, Teams and the default roster

**Files:**
- Create: `crates/katwork/src/team/{mod,dot,team,roles}.rs`, `crates/katwork-server/src/routes/{dots,teams}.rs`, `crates/katwork-server/tests/team.rs`
- Modify: `crates/katwork-server/src/main.rs` (`seed-demo`)

**Interfaces:**
- Consumes: `Tenant`, `SpaceId`, `types::{ModelTier, Locale, DotStatus}`.
- Produces:
  - `Dot { id: DotId, default_space_id: SpaceId, spaces: Vec<SpaceId>, name: String, role: DotRole, persona: String, instructions: String, tool_allowlist: Vec<String>, model_tier: ModelTier, autonomy: Autonomy, status: DotStatus, appearance: Appearance, max_concurrency: u16, daily_token_budget: Option<i64>, policy_version: i64 }`
  - `Dot::config_hash(&self) -> String`: hex SHA-256 of the canonical JSON of `persona`, `instructions`, `tool_allowlist`, `model_tier`, `autonomy` and `policy_version`
  - `DotRole::{Lead, Researcher, Writer, Analyst, Scheduler, Reviewer, Custom(String)}` and `Autonomy::{AskFirst, Standard}`
  - `Appearance { avatar: String, accent: String }`, stored in `dots.appearance`
  - `NewDot { name, role, persona, instructions, tool_allowlist, model_tier, autonomy, appearance }`
  - `DotPatch { name, persona, instructions, tool_allowlist, model_tier, autonomy, status, appearance, max_concurrency, daily_token_budget }`, every field an `Option`. `daily_token_budget: Option<Option<i64>>` carries `#[serde(default, deserialize_with = "double_option")]`, where `double_option` is a small local deserializer: an omitted field stays `None` (no change), JSON `null` becomes `Some(None)` (remove the cap), and a number becomes `Some(Some(n))`. So `{"daily_token_budget": null}` clears the cap.
  - `HireDot { name: String, role: DotRole, persona: String }`, the hire request
  - `roles::template(role: &DotRole, locale: Locale) -> NewDot`: the defaults a hire starts from: the role's instructions, `tool_allowlist = ["read_thread", "ask_user"]`, `model_tier = Standard`, `autonomy = AskFirst`, and the role's avatar (a neutral avatar for `Custom`); the request's name and persona replace the template's
  - Who may change what: any member may hire a Dot, edit its `name`, `persona` and `appearance`, create teams and change team membership. Only an owner or admin may change execution-affecting settings (`instructions`, `tool_allowlist`, `model_tier`, `autonomy`, `status`, `max_concurrency`, `daily_token_budget`) and grants; a member gets 403 `forbidden`.
  - Dot names are unique within a team, compared case-insensitively after Unicode NFC normalization; `team::create`, `team::add_member` and a rename through `dot::update` return 422 `dot_name_conflict` otherwise.
  - `Team { id: TeamId, space_id: SpaceId, name: String }` and `TeamView { team: Team, members: Vec<Dot> }`, with the lead first
  - `roles::default_roster(locale: Locale) -> Vec<NewDot>`: 6 Dots with English and Thai display names, for example Lead/หัวหน้าทีม, Researcher/นักวิจัย, Writer/นักเขียน, Analyst/นักวิเคราะห์, Scheduler/เลขา, Reviewer/ผู้ตรวจ. Each role gets a distinct avatar, and every Dot gets `tool_allowlist = ["read_thread", "ask_user"]`.
  - `dot::hire(pool, &Tenant, SpaceId, NewDot) -> Result<DotId, KatError>`: the Space becomes the Dot's default and its first grant
  - `dot::list(pool, &Tenant, SpaceId) -> Result<Vec<Dot>, KatError>`: the Dots granted to that Space
  - `dot::get(pool, &Tenant, DotId) -> Result<Dot, KatError>`
  - `dot::update(pool, &Tenant, DotId, DotPatch) -> Result<Dot, KatError>`
  - `dot::grant_space(pool, &Tenant, DotId, SpaceId) -> Result<(), KatError>` and `dot::revoke_space(pool, &Tenant, DotId, SpaceId) -> Result<(), KatError>`; revoking the default Space is 422 `default_space_required`
  - Any change to `tool_allowlist`, `status` or grants increments `policy_version` in the same statement.
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
- [ ] **Step 2: Run** `cargo test -p katwork-server --test team`. Expected: FAIL.
- [ ] **Step 3: Implement.** `seed-demo` also creates the default team in the demo Space.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: dots, teams, default roster`.

### Task 6: Threads and messages (web channel)

**Files:**
- Create: `crates/katwork/src/chat/{mod,thread,message,mentions}.rs`, `crates/katwork-server/src/routes/threads.rs`, `crates/katwork-server/tests/chat.rs`

**Interfaces:**
- Consumes: `Tenant`, `SpaceId`, `TeamId`, `DotId`, `JobId`, `types::Question`.
- Produces:
  - `Thread { id: ThreadId, space_id: SpaceId, team_id: TeamId, title: String }`
  - `thread::create(pool, &Tenant, SpaceId, TeamId, title: &str) -> Result<ThreadId, KatError>` and `thread::get(pool, &Tenant, ThreadId) -> Result<Thread, KatError>`; another workspace's thread is `NotFound`
  - `Author::{User(UserId), Dot(DotId), System}` (`Eq`, `Hash`)
  - `Message { id: MessageId, thread_id: ThreadId, author: Author, body: String, synthetic: bool, question: Option<Question>, job_id: Option<JobId>, created_at: DateTime<Utc> }`
  - `NewMessage { id: Option<MessageId>, author: Author, body: String, client_msg_id: Option<String>, idempotency_key: Option<String>, synthetic: bool, question: Option<Question>, job_id: Option<JobId> }`; `id: None` generates a new id, and `job_id` links the message to its job when it is inserted
  - `message::append(conn: &mut PgConnection, &Tenant, ThreadId, NewMessage) -> Result<Appended, KatError>` and `Appended { message_id: MessageId, job_id: Option<JobId>, created: bool }`. It runs `INSERT … ON CONFLICT DO NOTHING RETURNING`, and when no row comes back it selects the committed row: a repeat of the same `(thread, author, client_msg_id)` or of the same `idempotency_key` returns the original row with `created: false`, and a concurrent duplicate waits on the unique index, then returns the first row. It takes a connection so that intake (Task 12) and `job::conclude` (Task 10) can write inside their transactions.
  - `message::set_job(conn: &mut PgConnection, &Tenant, MessageId, JobId) -> Result<(), KatError>`
  - `message::list(pool, &Tenant, ThreadId, before: Option<MessageId>, limit: u16) -> Result<Vec<Message>, KatError>`
  - `mentions::parse(body: &str) -> Vec<String>`: `@name` tokens, Thai-safe. It ignores fenced and inline code and strips trailing punctuation (`"@Writer,"` → `Writer`).
- Routes:
  - `POST /api/workspaces/{workspace_id}/threads`
  - `GET /api/workspaces/{workspace_id}/threads/{thread_id}/messages`; each message includes `synthetic`
  - `POST /api/workspaces/{workspace_id}/threads/{thread_id}/messages` with `{body, client_msg_id?}`. A `client_msg_id` longer than 64 characters is 422 `client_msg_id_too_long`. The first post returns 201 `{message_id, job_id}`; a retry with the same `client_msg_id` returns 200 with the original `{message_id, job_id}`. `job_id` is filled in by Task 12.

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/chat.rs`:
  - `post_and_list_messages_in_order`
  - `mentions_parse_english_and_thai`: `"@Writer ช่วยสรุป"` → `["Writer"]`; `"@นักวิเคราะห์ ดูไฟล์"` → `["นักวิเคราะห์"]`
  - `mentions_ignore_code_and_trailing_punctuation`: `` "`@Writer` and @Analyst," `` → `["Analyst"]`
  - `message_body_limit_32kb`: a larger body returns 413 `message_too_large`
  - `retry_with_same_client_msg_id_returns_original_ids`: one row is stored, and the retry returns 200 with the same `message_id`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test chat`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: threads and messages`.

### Task 7: Model gateway, FakeProvider and Rig adapter

**Files:**
- Create: `crates/katwork/src/models/{mod,provider,registry,guard,fake,rig_adapter}.rs`, `crates/katwork/tests/{models.rs,rig_fixtures.rs}`, `crates/katwork/tests/fixtures/models/` (recorded provider bytes)
- Modify: `Cargo.toml`, `Cargo.lock`, `crates/katwork/src/{runtime.rs,config.rs}`, `crates/katwork-server/src/main.rs`

**Interfaces:**
- Consumes: `Config`, `ProviderEnv` (Task 1); `types::{ModelTier, SpacePolicy}` and `Core` (Task 2). Nothing from Tasks 3–6.
- Produces:
  - `#[async_trait] trait ModelProvider: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> Capabilities; async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError>; }`
  - `ChatRequest { system: String, messages: Vec<ChatMsg>, tools: Vec<ToolSpec>, max_output_tokens: u32, temperature: Option<f32> }` (`Serialize`).
  - `ChatRequest::input_charge_bound(&self) -> u64`: the byte length of the whole request serialized as JSON: system text, every message and every tool schema, with their JSON framing. Tokenizers that work on UTF-8 bytes (byte-level BPE, and byte-fallback tokenizers) emit at most one token per byte, so this is an upper bound on input tokens even for emoji and Thai, where one character can be several tokens; the JSON framing of each message is longer in bytes than the few framing tokens a provider adds. The model table in `registry.rs` lists only models whose tokenizer has this property.
  - System text lives only in `ChatRequest.system`; there is no system role.
  - `enum ChatMsg { User { content: String, name: Option<String> }, Assistant { content: String, tool_calls: Vec<ToolCall>, replay: Option<ProviderReplay> }, Tool { tool_call_id: String, name: String, content: String, is_error: bool } }` (`Serialize`, `Deserialize`)
  - `ToolCall { id: String, name: String, args_json: String }`
  - `ProviderReplay { provider_id: String, version: u32, data: serde_json::Value }`: opaque, versioned provider state, passed back only to the same provider in the same workspace and thread
  - `ToolSpec { name: String, description: String, parameters: serde_json::Value }`, where `parameters` is JSON Schema
  - `ChatChunk::{TextDelta(String), ToolCall(ToolCall), Usage(Usage), Done { stop: StopKind, turn: AssistantTurn }}`. Text deltas are provisional. `ToolCall` chunks are emitted only after the whole turn is accepted. `turn` is the canonical final turn, never joined deltas.
  - `AssistantTurn { content: String, tool_calls: Vec<ToolCall>, replay: Option<ProviderReplay> }`
  - `StopKind::{EndTurn, ToolUse, MaxTokens, Refusal, Other(String)}`
  - `Usage { input: Option<u64>, cache_read: Option<u64>, cache_write: Option<u64>, output: Option<u64>, complete: bool }`. Cache counters are subsets of `input`, not extra. A missing count is `None`, never 0.
  - `ModelError::{ProviderUnavailable, ResidencyDenied, Timeout { phase: TimeoutPhase }, RateLimited { retry_after_secs: Option<u64> }, QuotaExceeded { resets_at: Option<DateTime<Utc>> }, AuthFailed, Protocol(String), Other(String)}` and `TimeoutPhase::{FirstChunk, Idle}`. `Protocol` and `Other` hold private diagnostics only; the provider's raw error body never reaches a tenant.
  - `StreamTimeouts { first_chunk: Duration, idle: Duration }`, default 30 s each
  - `guard::guard_stream(inner: BoxStream<'static, Result<ChatChunk, ModelError>>, t: StreamTimeouts) -> BoxStream<'static, Result<ChatChunk, ModelError>>`. It times the actual stream polling, not only the opener: a stall yields `Timeout { phase }`. A stream that ends without `Done` yields `Protocol`, and so does `Done { stop: ToolUse }` with no tool call.
  - `Capabilities { tools: bool, residency: Residency, synthetic: bool }` and `Residency::{Local, External}`
  - `TokenPrice { input_micros_per_mtok: u64, output_micros_per_mtok: u64 }`
  - `ModelEntry { provider_id: String, model: String, tier: ModelTier, capabilities: Capabilities, max_output_tokens: u32, temperature: Option<f32>, price: Option<TokenPrice> }`. Capabilities and residency come from the entry, never from the provider name or base URL. SP1 configures no prices, so `model_calls.cost_micros` stays NULL; cost is only ever computed from usage the provider reported.
  - `Selected { provider: Arc<dyn ModelProvider>, entry: ModelEntry }`
  - `Registry::from_config(&Config) -> Registry` and `Registry::from_entries(Vec<(ModelEntry, Arc<dyn ModelProvider>)>) -> Registry`
  - `Registry::for_tier(&self, tier: ModelTier, space_policy: &SpacePolicy) -> Result<Selected, ModelError>`
  - `FakeTurn::{Chunks(Vec<ChatChunk>), Error(ModelError), Hang, ChunksThenHang(Vec<ChatChunk>)}`
  - `FakeProvider::scripted(Vec<FakeTurn>) -> FakeProvider` (`Clone`; each `stream` call takes the next turn) and `FakeProvider::received(&self) -> Vec<ChatRequest>`, the requests it got
  - `FakeProvider::demo() -> FakeProvider`: canned English and Thai replies, `Local`, with `capabilities().synthetic == true`
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
  - a provider tool call with no id gets `kw_call_{index}`; a duplicate id within one turn is `Protocol`

- [ ] **Step 1: Pin dependencies.** Check crates.io for the latest rig-core release; pin it exactly (`=x.y.z`) with `default-features = false` and explicit reqwest and rustls features. If the release moves its reqwest transport into a separate crate, pin that crate to the same exact version. Add `reqwest` with the same version requirement that transport declares (for example `0.13`), never a different line, so `Cargo.lock` resolves one reqwest. Then re-run the `msrv` check on the new locked graph; if it fails, raise `rust-version` in `Cargo.toml` and the CI job's toolchain together to the lowest version that passes. Add `async-trait`, `futures` and `tracing-subscriber`. In `katwork-server`, initialize tracing from `RUST_LOG`, defaulting to `info,rig=info`, because Rig's `rig::completions` TRACE level logs full prompts.
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
  - `replay_round_trips_to_the_same_provider`
  - `oversized_response_is_rejected_before_decoding`
  - `oversized_error_body_is_capped_before_rig_reads_it`
  - `unterminated_sse_frame_over_cap_is_rejected`
  - `live_openai_chat_smoke` (`#[ignore]`): runs only when `OPENAI_API_KEY` is set, never in CI
- [ ] **Step 4: Run** `cargo test -p katwork --test models --test rig_fixtures`. Expected: FAIL.
- [ ] **Step 5: Implement.**
- [ ] **Step 6: Run.** Expected: PASS.
- [ ] **Step 7: Commit** with `feat: model gateway with residency policy, fake and rig providers`.

### Task 9: Event log, hub and AG-UI SSE

This task comes before Task 8, because the agent loop emits through `EventSink`.

**Files:**
- Create: `crates/katwork/src/events/{mod,agui,log,hub,sink,coalesce}.rs`, `crates/katwork/tests/events.rs`, `crates/katwork/tests/fixtures/agui/`, `crates/katwork-server/src/routes/events.rs`, `crates/katwork-server/tests/events.rs`, `crates/katwork-server/tests/fixtures/sse/thai_emoji.sse`
- Modify: `crates/katwork/src/runtime.rs`, `Cargo.toml` (`unicode-segmentation`)

**Interfaces:**
- Consumes: `Tenant`, `thread::get`, `Author` (Task 6), ids, `types::{SideEffect, ReasonCode, JobStatus, HoldReason, Question}`, `fence::{Fence, check, lock}`, `Core`.
- Produces:
  - `enum AgUiEvent`: KatWork's own serde types for AG-UI protocol 1.0, covering only the events KatWork emits (ADR-0003). Serialized with `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]` and camelCase fields. Variants: `RunStarted{threadId,runId}`, `TextMessageStart{messageId,role}`, `TextMessageContent{messageId,delta}`, `TextMessageEnd{messageId}`, `ToolCallStart{toolCallId,toolCallName,parentMessageId}`, `ToolCallArgs{toolCallId,delta}`, `ToolCallEnd{toolCallId}`, `ToolCallResult{messageId,toolCallId,content}`, `RunFinished{threadId,runId}`, `RunError{message,code}`, `Custom{name,value}`.
  - Ids: `threadId` and `runId` are KatWork's. `toolCallId` is the `ToolCallRecordId`, so a provider id reused in a later step stays distinct. `TextMessageStart.messageId` equals the stored reply's `MessageId`. `TOOL_CALL_END` means the arguments are complete; `TOOL_CALL_RESULT` means execution is complete.
  - `KatworkEvent`, serialized as `Custom{name,value}` through `From<KatworkEvent> for AgUiEvent`. Each payload is a serde struct with camelCase fields, typed only with Task 2 and Task 6 types:
    - `katwork.dot_state`: `DotStateEvent { dot_id: DotId, state: DotState }`, with `DotState::{Idle, Working, Researching, Waiting, NeedsApproval, Error, Paused}`
    - `katwork.job_status`: `JobStatusEvent { job_id: JobId, status: Option<JobStatus>, reason: Option<ReasonCode>, hold_reason: Option<HoldReason> }`; `status` is null while the job runs
    - `katwork.decision`: `DecisionEvent { job_id: JobId, inputs: serde_json::Value, verdict: serde_json::Value, policy_version: u32, thresholds: serde_json::Value }`; Task 10's `policy::decision_event` owns the conversion and defines the shapes
    - `katwork.tool_status`: `ToolStatusEvent { tool_call_id: ToolCallRecordId, ok: bool, code: Option<String> }`
    - `katwork.usage`: `UsageEvent { run_id: RunId, dot_id: DotId, model_call_id: ModelCallId, input: Option<u64>, output: Option<u64>, complete: bool }`
    - `katwork.message_stored`: `MessageStoredEvent { message_id: MessageId, author: Author, body: String, synthetic: bool, question: Option<Question> }`, emitted once a message is committed
    - `katwork.run_started`: `RunStartedEvent { run_id: RunId, dot_id: DotId, reply_message_id: MessageId }`, emitted right after `RUN_STARTED`, so the client knows which provisional message belongs to which run
  - Client folding rules, which Task 13 implements: a `TEXT_MESSAGE_START` for a `messageId` starts that message's provisional text over; `TEXT_MESSAGE_CONTENT` appends to it; `katwork.message_stored` is authoritative and replaces whatever the client holds for that `messageId`, with the stored body, `synthetic` flag and question. When a run ends (`RUN_FINISHED` or `RUN_ERROR` for its `runId`), the client drops that run's provisional message (the `replyMessageId` from `katwork.run_started`) unless `katwork.message_stored` confirmed it, so text from a run that lost its lease or failed never stays beside the stored transcript. The rules fold the event log deterministically, so a reconnect that replays from any seq ends at the same transcript.
  - `DotState::for_tool(SideEffect) -> DotState`: `Read`, `WorkspaceRead` and `Navigate` give `Researching`; every other effect gives `Working`. Only the harness emits `dot_state`: `Working` while the model streams, `NeedsApproval` on an approval stop, `Waiting` on a question, `Error` on a run error, `Paused` for a paused Dot, `Idle` otherwise.
  - `log::append(tx: &mut Transaction<'_, Postgres>, workspace_id: WorkspaceId, thread_id: ThreadId, run_id: Option<RunId>, e: &AgUiEvent) -> Result<EventSeq, KatError>`: runs `UPDATE threads SET last_event_seq = last_event_seq + 1 WHERE id = $1 RETURNING last_event_seq` and then the insert, inside the caller's transaction, so appenders serialize per thread. The `UPDATE` takes the thread lock; a caller that also needs job or task locks has taken them already, and one that needs the Dot row takes it afterwards (global lock order).
  - `log::since(pool, &Tenant, ThreadId, after: EventSeq) -> Result<Vec<(EventSeq, AgUiEvent)>, KatError>`
  - `EventHub` (`Clone`): `EventHub::new() -> EventHub`, `subscribe(&self, ThreadId) -> tokio::sync::broadcast::Receiver<EventSeq>` and `notify(&self, ThreadId, EventSeq)`. The broadcast is only a wake-up; the log is the truth.
  - `#[async_trait] trait EventSink: Send + Sync { async fn emit(&self, e: AgUiEvent) -> Result<EventSeq, KatError>; }`
  - `PgEventSink::new(pool: PgPool, hub: EventHub, workspace_id: WorkspaceId, thread_id: ThreadId, run_id: Option<RunId>, fence: Option<Fence>) -> PgEventSink`: each `emit` runs, in its own transaction, `fence::check` (when `fence` is set; a shared task-row lock) and then `log::append` (the thread lock), in the global order, commits, and only then calls `hub.notify`. A failed check returns `lease_lost` and writes nothing, so a worker whose lease was reclaimed or cancelled cannot add events after the harness closed its run.
  - `DeltaCoalescer::new() -> DeltaCoalescer`, `push(&mut self, delta: &str, now: Instant) -> Option<String>` and `finish(&mut self) -> Option<String>`: it buffers text deltas and releases them at ≥100 ms or ≥512 bytes, cut only at a grapheme-cluster boundary (`unicode-segmentation`)
  - `Core` gains `events: EventHub`, built in `Core::new`
  - Route `GET /api/workspaces/{workspace_id}/threads/{thread_id}/events`: SSE with `id:` set to the per-thread `EventSeq`. It subscribes to the hub first, then replays `since(Last-Event-ID or 0)`, then on each wake-up reads `since(last sent)` from the log. It drops any event with `seq ≤` the last one sent, re-reads from the log when the receiver lags, and sends a `:` comment keep-alive every 15 s.

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/events.rs`:
  - `events_serialize_as_agui_json`: `TextMessageContent` → `{"type":"TEXT_MESSAGE_CONTENT","messageId":"…","delta":"…"}`
  - `agui_fixtures_match_protocol_1_0`: every emitted variant round-trips against the protocol JSON in `tests/fixtures/agui/`, and every `katwork.*` payload against its fixture in `tests/fixtures/agui/katwork/`
  - `concurrent_appenders_never_skip_on_resume`: two tasks append to one thread at once, and a reader resuming at any seq sees every event
  - `fenced_sink_rejects_writes_after_epoch_change`: after the task's epoch is incremented, `emit` returns `lease_lost` and the log is unchanged
  - `thai_delta_never_splits_a_grapheme_cluster`: `"ที่นี่สวัสดีครับ 👋🏽"` pushed in every split; every released chunk ends on a grapheme boundary, and the concatenation is byte-equal
- [ ] **Step 2: Write the failing route tests** in `crates/katwork-server/tests/events.rs`, reading the response as raw bytes, never `String`s:
  - `reconnect_with_last_event_id_has_no_gap_or_duplicate`: Review Focus 3
  - `replay_to_live_gap_loses_nothing`: an event appended between replay and the live phase arrives once
  - `lagged_subscriber_recovers_from_log`
  - `sse_thai_and_emoji_event_parses_at_every_byte_split`: uses `tests/fixtures/sse/thai_emoji.sse`, which Task 13 reuses
  - `sse_crlf_framing_and_comment_keepalives_parse`
  - `duplicate_replay_is_dropped`
  - `other_workspace_cannot_subscribe`: 404
- [ ] **Step 3: Run** `cargo test -p katwork --test events && cargo test -p katwork-server --test events`. Expected: FAIL.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run.** Expected: PASS.
- [ ] **Step 6: Commit** with `feat: event log and AG-UI SSE with resume`.

### Task 8: Tool registry and agent loop

**Files:**
- Create: `crates/katwork/src/agent/{mod,tool,ctx,registry,builtin,run,guards,ledger}.rs`, `crates/katwork/tests/agent.rs`
- Modify: `Cargo.toml` (`tokio-util`)

**Interfaces:**
- Consumes: `fence::{Fence, check, lock}`, `types::Question` (Task 2); `Registry`, `Selected`, `ModelProvider`, `ChatRequest`, `ChatMsg`, `ChatChunk`, `ToolSpec`, `ModelError`, `StreamTimeouts`, `guard_stream` (Task 7); `EventSink`, `AgUiEvent`, `KatworkEvent`, `DotState`, `DeltaCoalescer` (Task 9); `Dot`, `Dot::config_hash` (Task 5); `Tenant` (Task 4); ids and `types::{SideEffect, SpacePolicy}` (Task 2).
- Produces:
  - `#[async_trait] trait Tool: Send + Sync { fn spec(&self) -> ToolSpec; fn side_effect(&self) -> SideEffect; async fn call(&self, ctx: &ToolCtx, args: serde_json::Value) -> Result<ToolOutput, ToolError>; }`
  - `ToolOutput::{Text(String), Ask(Question)}`: `Text` goes back to the model as the tool result; `Ask` ends the run with that question
  - `ToolCtx { tenant: Tenant, thread_id: ThreadId, dot_id: DotId, job_id: JobId, task_id: TaskId, run_id: RunId, step: u32, tool_call_id: String, idempotency_key: String, lease_epoch: LeaseEpoch, actor: UserId, initiator: Initiator, computer: Option<ComputerHandle>, pool: PgPool, cancel: CancellationToken }`
    - `idempotency_key` is `"{run_id}:{step}:{tool_call_id}"`
    - `computer` is always `None` in SP1; `ComputerHandle` is an empty placeholder struct
    - tenant authority and the pool never appear in model-visible arguments
    - `ToolCtx::fence(&self) -> Fence` gives `{task_id, lease_epoch}`; every tool write runs `fence::check` inside its own write transaction
  - `Initiator::{Person, Routine, Handoff, System}` (`Serialize`, `Deserialize`)
  - `ToolError::{NotAllowed, NeedsApproval, InvalidArgs(String), Failed(String), Timeout, OutcomeUnknown}`
  - `ToolRegistry::new() -> ToolRegistry`, `register(&mut self, tool: Arc<dyn Tool>)`, `get(&self, id: &str) -> Option<Arc<dyn Tool>>` and `ToolRegistry::builtin() -> ToolRegistry`
  - Built-in tools. There is no `post_reply`: the final assistant text is the reply, written once by `job::conclude` (Task 10).
    - `read_thread` (`Read`). Arguments `{"limit": integer 1–50, default 20, "before": message id, optional}`, no other properties. It reads only `ctx.thread_id`, newest last, and returns the JSON text `{"messages": [{"id", "author": "user:<name>" | "dot:<name>" | "system", "body", "created_at"}], "next_before": id or null}`, which the loop then truncates like any result.
    - `ask_user` (`Read`). Arguments `{"question": string of 1–500 characters, "options": array of 0–4 strings of 1–100 characters each}`, no other properties; the first option is the recommended one. It returns `ToolOutput::Ask(Question)`. The loop records the call `succeeded` with the question's JSON as its result, sets `RunFacts.pending_question`, and ends the run. Calls listed before `ask_user` in the same turn have already run and keep their own rows; only the calls after it, never dispatched, are recorded `refused` with code `superseded_by_question`. `job::conclude` stores the question as the Dot's message: the body is the question text, and `messages.question` holds the question with its options.
  - `RunKind::{Initial, Repair, Recovery}`
  - `RunInput { tenant: Tenant, run_id: RunId, thread_id: ThreadId, space_id: SpaceId, job_id: JobId, task_id: TaskId, lease_epoch: LeaseEpoch, kind: RunKind, space_policy: SpacePolicy, history: Vec<ChatMsg>, reply_message_id: MessageId, policy_version: i64, actor: UserId, initiator: Initiator, deadline: tokio::time::Instant, cancel: CancellationToken }`
  - `RunEnv<'a> { pool: &'a PgPool, models: &'a Registry, tools: &'a ToolRegistry, sink: &'a dyn EventSink }`
  - `run::execute(env: &RunEnv<'_>, dot: &Dot, input: RunInput, limits: &Limits) -> Result<RunFacts, RunAbort>`. It inserts the `runs` row first and closes it at the end.
  - `RunAbort::{LeaseLost, Infrastructure { run_id: Option<RunId>, code: &'static str }}`: a run-scoped write that fails with `lease_lost` gives `LeaseLost`; any other failed write (the `runs` row, a ledger row, an event) gives `Infrastructure` with the error's code. Either way nothing more is dispatched, and the abort never becomes a policy verdict.
  - `RunFacts { run_id: Option<RunId>, reply_message_id: MessageId, final_text: Option<String>, stop: Option<StopReason>, tool_calls: Vec<ToolCallRecord>, artifacts_created: u32, pending_tool_call: Option<PendingCall>, pending_question: Option<Question>, synthetic: bool, executor_unavailable: bool, run_closed: bool, usage: Vec<ModelCallId> }`. `run_closed` is true only when the loop already ended the run with `RUN_ERROR` (the cancellation path). The loop returns facts only; the policy (Task 10) turns them into a job status.
  - `RunFacts::not_run(stop: StopReason) -> RunFacts`: no run id, a fresh `reply_message_id`, everything else empty; used when the harness decides without a run (Tasks 11 and 12)
  - `ToolCallRecord { id: ToolCallRecordId, step: u32, tool_call_id: String, name: String, side_effect: Option<SideEffect>, state: ToolCallState }` and `ToolCallState::{Started, Succeeded, Failed, TimedOut, OutcomeUnknown, Refused}`
  - `PendingCall { record_id: ToolCallRecordId, tool_call_id: String, name: String, args_canonical: String, args_sha256: String }`, stored as `tasks.pending_tool_call`
  - `StopReason::{ProviderUnavailable, ProviderRateLimited, ProviderQuota, ProviderAuthFailed, ModelProtocol, StepLimit, LoopDetected, NeedsApproval, TokenBudget, OutputTruncated, TimeLimit, PolicyChanged, ResidencyDenied, QuotaExceeded, WorkspacePaused, DotPaused, NoEligibleDot, Interrupted, OutcomeUnknown, Cancelled}`
  - `Limits { max_steps: u32, tool_timeout: Duration, result_max_chars: usize, repeat_limit: u32, repeat_window: (u32, u32), max_total_tokens: u64, run_wall_clock: Duration, task_deadline: Duration, job_deadline: Duration, model_retries: u32, stream: StreamTimeouts }`. `Limits::default()` holds the initial defaults: 24, 60 s, 8,000, 3, (5, 10), 200,000, 90 s, 5 min, 10 min, 1, and 30 s / 30 s. The run deadline is separate from the lease TTL.
  - `guards::authorize(conn: &mut PgConnection, fence: &Fence, dot_id: DotId, space_id: SpaceId, policy_version: i64) -> Result<Option<StopReason>, KatError>`: runs `fence::check`, then reads the Dot and its workspace. A paused workspace gives `WorkspacePaused`, a paused Dot `DotPaused`, and a missing grant to `space_id` or a `policy_version` other than `input.policy_version` gives `PolicyChanged`. Status, grant and allowlist changes all increment `policy_version` (Task 5), so a change made while the task was queued is caught on the first check.
  - `guards::canonical_args(args: &serde_json::Value) -> String`: object keys sorted and whitespace normalized; array order and values kept
  - `guards::RepeatKey { tool_name: String, canonical_args: String, target_revision: Option<String> }`, where `target_revision` is always `None` in SP1 (SP2 fills it so a render after each edit is not a loop), and `guards::RepeatGuard::new(limit: u32, window: (u32, u32))` with `check(&mut self, key: &RepeatKey) -> bool`, false meaning stop before this call. The provider's call id is never part of the key.
  - `guards::truncate_result(text: &str, max_scalars: usize) -> String`: at most `max_scalars` Unicode scalar values, the marker `"…[truncated]"` included. A JSON result over the cap becomes a plain-string preview prefixed `"[JSON truncated] "`, never cut inside its structure.
  - `ledger::start_tool_call(pool, ctx: &ToolCtx, name: &str, side_effect: SideEffect, args_canonical: &str) -> Result<ToolCallRecordId, KatError>`: inserts a `started` row with the canonical arguments and their SHA-256. For a non-Read call it runs `fence::lock` first, because it also sets `tasks.effects_started_at` in the same transaction; a Read call needs only `fence::check`.
  - `ledger::record_refused(pool, ctx: &ToolCtx, name: &str, side_effect: Option<SideEffect>, args_canonical: &str, code: &str) -> Result<ToolCallRecordId, KatError>`: inserts a finished `refused` row whose `result` is the error text sent to the model, and never touches `effects_started_at`. `side_effect` is `None` only for an unknown tool name. It records allowlist and effect-gate refusals (`NotAllowed`, `NeedsApproval`), invalid arguments, unknown tools, repeat-guard stops and `superseded_by_question`.
  - `ledger::retry_read(pool, fence: &Fence, id: ToolCallRecordId) -> Result<(), KatError>`: a `Read` call's one retry reuses its row: `attempts = 2` and the state goes back to `started`; the `(run_id, step, tool_call_id)` identity never changes
  - `ledger::end_tool_call(pool, fence: &Fence, id: ToolCallRecordId, state: ToolCallState, result: Option<&str>) -> Result<(), KatError>`
  - `ledger::record_model_call(pool, fence: &Fence, row: &ModelCallRow) -> Result<ModelCallId, KatError>`, where `ModelCallRow` mirrors the `model_calls` columns
  - Fencing: every run-scoped write (the `runs` row, each ledger function, each event through the fenced `PgEventSink`, each tool write) runs `fence::check` in the same transaction as the write, and `lease_lost` aborts it. Before each model attempt the loop also runs `guards::authorize`, which includes `fence::check`, so a reclaimed or cancelled worker makes no further model calls. On `lease_lost` the loop stops at once, writes nothing further and returns `Err(RunAbort::LeaseLost)`.
- Loop rules:
  - **Steps.** A step is one model attempt, retries included. An answer on attempt 24 is accepted; a tool batch that would need attempt 25 is not executed (`StepLimit`).
  - **Model attempts.** Each attempt calls `models.for_tier(dot.model_tier, &input.space_policy)`, so a provider change re-checks residency, and wraps the stream in `guard_stream`, raced against `input.deadline` (`TimeLimit`); no attempt starts once `input.deadline` has passed. It retries only before the first chunk, at most `model_retries` times. Every attempt gets one `model_calls` row and one `katwork.usage` event. Each request uses `max_output_tokens = min(entry.max_output_tokens, the run's remaining token budget)` and `temperature = entry.temperature`; with less than 256 tokens of budget left, the run stops `TokenBudget` before the attempt.
  - **Provider errors.** Every `ModelError` has exactly one outcome. Provider text stays in private, redacted logs, never in events, messages or the job reason.

    | `ModelError` | retried before the first chunk | outcome when it persists |
    |---|---|---|
    | `ProviderUnavailable`, `Other(_)`, `Timeout { phase: FirstChunk }` | yes, up to `model_retries` | stop `ProviderUnavailable`, `executor_unavailable = true` |
    | `Timeout { phase: Idle }` | no, a chunk already arrived | stop `ProviderUnavailable`, `executor_unavailable = true` |
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
  - **Repeat guard before dispatch.** The run stops `LoopDetected` before executing the 3rd consecutive identical call, or the 5th identical call within the last 10. Two calls are identical when their `RepeatKey`s are equal: the tool name, the canonical arguments and the target revision. The provider's call id, the step and the earlier call's outcome are never part of the key.
  - **Timeouts and cancellation.** Each call's future is owned and raced against `min(tool_timeout, run remaining)` and the run's `CancellationToken`, and dropped on either. Cancellation is checked before dispatch and between sibling calls. No task is ever detached. A call that times out or loses its reply after dispatch is `OutcomeUnknown`. A `Read` tool may retry once. For any other effect the run stops `OutcomeUnknown` at once: the call stays `outcome_unknown` in the ledger, no result goes back to the model, and nothing else is dispatched. The effect may already have committed, and a repeat requested by the model would carry a new call id and so a new idempotency key.
  - **Batch order.** A turn's tool calls are dispatched one at a time, in the order the model listed them, and each gets exactly one ledger row.
  - **Ledger.** Every dispatched call goes through `start_tool_call`, then `end_tool_call` with `succeeded`, `failed`, `timed_out` or `outcome_unknown`. Every call that is not dispatched goes through `record_refused`, so a refused outward call never marks effects as started. A failed ledger write aborts the run before the tool is called.
  - **Result pairing.** An unknown tool name, a validation failure, a timeout and a refusal each become exactly one `ChatMsg::Tool { is_error: true }` with the original call id and name, and the loop continues unless a stop rule above applies. The text given to the model is `truncate_result(…, result_max_chars)`, and that exact text is stored in `tool_calls.result`.
  - **Prompt hygiene.** The system text is the Dot's persona and instructions plus a capability preamble generated from the effective tool set, never hand-written into `instructions`. Tool results, history and retrieved text never use the system role. Prompts never say "never ask".
  - **Events.** `RUN_STARTED`, `katwork.run_started {runId, dotId, replyMessageId}`, `dot_state working`, then `TEXT_MESSAGE_START` with `messageId = reply_message_id`, the text through `DeltaCoalescer` as `TEXT_MESSAGE_CONTENT`, and `TEXT_MESSAGE_END`. Every model turn of the run streams under the same `reply_message_id` and opens with its own `TEXT_MESSAGE_START`, so text a turn wrote before calling tools is replaced, not kept; the stored reply arrives as `katwork.message_stored` (Task 10). Tool-call events follow only once the turn is accepted; `TOOL_CALL_RESULT` and `katwork.tool_status` follow execution. On cancel, the loop emits `TOOL_CALL_RESULT` (content: `cancelled`) and `katwork.tool_status{ok: false, code: "cancelled"}` for every open call, then `RUN_ERROR{code: "cancelled"}`, and returns with `stop = Cancelled` and `run_closed = true`. A stop the loop finds itself, including `guards::authorize` returning `WorkspacePaused`, `DotPaused` or `PolicyChanged` between attempts, is not a cancellation: the loop emits no `RUN_ERROR`, leaves `run_closed = false`, and `job::conclude` ends the run with `RUN_FINISHED`. Each run therefore gets exactly one terminal event, and one owner for it. The loop never emits `RUN_FINISHED`; `job::conclude` does (Task 10).

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/agent.rs` (FakeProvider scripted; test-only tools registered per test):
  - `plain_text_reply_sets_final_text`
  - `tool_call_then_reply`
  - `unknown_tool_id_returns_error_result_and_continues`
  - `registered_tool_outside_dot_allowlist_is_refused`
  - `allowlist_revoked_mid_run_stops_before_next_tool`
  - `policy_change_between_attempts_stops_before_next_model_call`: the Dot is paused after the first attempt; no second model request reaches the provider and `stop = DotPaused`
  - `destructive_and_outward_tools_refused_before_dispatch`: the tool's `call` never runs; `stop = NeedsApproval`; `pending_tool_call` keeps the call id
  - `navigate_exec_and_credential_tools_are_not_allowed`
  - `no_tool_runs_from_a_max_tokens_turn`: the partial text is kept and `stop = OutputTruncated`
  - `no_tool_runs_from_a_failed_turn`
  - `same_tool_same_args_three_times_stops_loop_detected`: the third call is not executed
  - `alternating_two_calls_five_times_in_ten_stops`
  - `call_id_and_key_order_do_not_defeat_repeat_guard`
  - `same_mutation_with_new_call_id_counts_as_repeat`: the same tool and canonical arguments under three different call ids stop the run before the third dispatch
  - `unknown_outcome_write_stops_for_review_before_model_retries_it`: a `WorkspaceWrite` test tool commits and then times out; the model is scripted to request the same write again under a new call id; the second request is never dispatched, `stop = OutcomeUnknown`, and the ledger holds one `outcome_unknown` row
  - `answer_on_step_24_accepted_tool_batch_needing_25_stops`
  - `token_budget_200k_stops_before_next_attempt`
  - `run_wall_clock_90s_stops_time_limit`: paused tokio clock
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
  - `every_model_error_maps_to_a_stable_stop_reason`: table-driven over the table above; no provider text reaches an event or message
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

**Interfaces:**
- Consumes: `RunFacts`, `StopReason`, `Initiator` (Task 8); `ChatMsg` (Task 7); `log::append`, `EventHub`, `AgUiEvent`, `KatworkEvent`, `DotState` (Task 9); `message::append`, `NewMessage` (Task 6); `Tenant`; `types::{ReasonCode, JobStatus}`.
- Produces:
  - `NextStep::{RunCheapChecks, AskUser, HandToLead, SaveDraftWithReport, Retry, RestartJob}`
  - `Thresholds { version: u32 }` and `Thresholds::SP1` (`version: 1`)
  - `Verdict { status: JobStatus, reason: Option<ReasonCode>, next: Option<NextStep> }`; `status` is never `Cancelled` or `Error`
  - `policy::decide_job(facts: &RunFacts, t: &Thresholds) -> Verdict`: pure, with no I/O, clock or model. Rules, first match wins:
    1. `executor_unavailable` → `Stopped`, `provider_unavailable`, `Retry`
    2. `stop = Some(s)` → the table below
    3. `pending_question` set → `NeedsInput`, `question_pending`, `AskUser`
    4. `final_text` set → `Answered` when `artifacts_created == 0`, otherwise `Ready`
    5. anything else → `Stopped`, `model_protocol_error`, `Retry`

    | `StopReason` | status | reason | next step |
    |---|---|---|---|
    | `ProviderUnavailable` / `ProviderRateLimited` / `ModelProtocol` / `TimeLimit` | `stopped` | `provider_unavailable` / `provider_rate_limited` / `model_protocol_error` / `time_limit` | `Retry` |
    | `ProviderQuota` / `QuotaExceeded` / `WorkspacePaused` | `stopped` | `provider_quota` / `quota_exceeded` / `workspace_paused` | `Retry` (after the reset or unpause) |
    | `ProviderAuthFailed` / `DotPaused` / `ResidencyDenied` / `NoEligibleDot` | `stopped` | `provider_auth_failed` / `dot_paused` / `residency_denied` / `no_eligible_dot` | `AskUser` (an admin) |
    | `StepLimit` / `TokenBudget` / `OutputTruncated` | `stopped` | `step_limit` / `token_budget` / `output_truncated` | `SaveDraftWithReport` |
    | `LoopDetected` | `stopped` | `loop_detected` | `HandToLead` |
    | `PolicyChanged` | `stopped` | `policy_changed` | `RestartJob` |
    | `NeedsApproval` | `needs_input` | `approval_pending` | `AskUser` |
    | `Interrupted` (lease reclaimed after a side effect) / `OutcomeUnknown` (a non-Read call's result is unknown) | `stopped` | `interrupted_review_required` / `outcome_unknown` | `Retry` after review |
    | `Cancelled` | `stopped` | `cancelled` | `Retry`; never reached in practice, because cancelled jobs end through the harness |

  - `policy::decision_event(job_id: JobId, facts: &RunFacts, v: &Verdict, t: &Thresholds) -> KatworkEvent`: builds `katwork.decision`. `inputs` is `{"stop": the stop reason's wire name or null, "executorUnavailable", "hasFinalText", "pendingQuestion", "artifactsCreated", "toolCalls": count, "synthetic"}`; `verdict` is `{"status", "reason", "next"}` as wire strings; `thresholds` is `Thresholds` as JSON; `policyVersion` is `Thresholds.version`.
  - `job::create(conn: &mut PgConnection, &Tenant, ThreadId, MessageId) -> Result<JobId, KatError>`: a running job (`status` NULL) with `requested_by = tenant.user_id`
  - `job::create_follow_up(conn: &mut PgConnection, &Tenant, parent: JobId, MessageId) -> Result<JobId, KatError>`: `parent` must be `needs_input` in the same thread, otherwise `job_not_awaiting_input`; sets `parent_job_id` (ADR-0002)
  - `JobSummary { id: JobId, status: Option<JobStatus> }` and `job::latest_in_thread(conn: &mut PgConnection, &Tenant, ThreadId) -> Result<Option<JobSummary>, KatError>`
  - `job::finish_by_policy(tx: &mut Transaction<'_, Postgres>, job_id: JobId, v: &Verdict, decision_seq: EventSeq) -> Result<EventSeq, KatError>`: sets `status`, `reason`, `decided_by = 'policy'` and `decision_seq`, then appends `katwork.job_status` and returns its seq. The caller has already appended `katwork.decision`.
  - `HarnessEnd::{Cancelled, Error { code: ReasonCode }}` and `job::finish_by_harness(tx: &mut Transaction<'_, Postgres>, job_id: JobId, end: HarnessEnd) -> Result<EventSeq, KatError>`: sets `status`, `reason` and `decided_by = 'harness'`, then appends `katwork.job_status` and returns its seq, inside the caller's transaction (claim and cancel use it), which already holds the job row and the thread row; the caller commits and notifies the hub
  - Finishing a finished job is `job_already_finished`.
  - `job::conclude(tx: &mut Transaction<'_, Postgres>, tenant: &Tenant, job_id: JobId, thread_id: ThreadId, dot_id: Option<DotId>, idem_key: &str, facts: &RunFacts) -> Result<EventSeq, KatError>`: the single place that writes a job's outcome. The caller must already hold, in the global lock order, the job row, the task row when there is one, and the thread row (`lease::complete`, `lease::claim`, intake and cancel do); `conclude` takes no other lock. In the caller's transaction it:
    1. runs `policy::decide_job(facts, &Thresholds::SP1)`
    2. appends `katwork.decision`
    3. when `dot_id` is set and there is `final_text` or a `pending_question`, appends the Dot's message, followed by its `katwork.message_stored` event, with `id = facts.reply_message_id`, `idempotency_key = "reply:{idem_key}"` and `synthetic = facts.synthetic`. For a question, the body is the question text and `question` holds the question with its options. Every message `conclude` writes has `job_id` set to the job.
    4. for a `stopped` verdict, appends a `System` message whose body is the reason code, with `idempotency_key = "notice:{idem_key}"`, followed by its `katwork.message_stored` event
    5. calls `job::finish_by_policy`
    6. when `facts.run_id` is set, appends `katwork.dot_state` (`needs_approval` for an approval stop, `waiting` for a question, `paused` for a workspace pause, `idle` otherwise), then `RUN_FINISHED` unless `facts.run_closed` (the loop already ended that run with `RUN_ERROR`)

    It returns the last seq; the caller commits and notifies the hub.
  - `TaskInput { thread_id: ThreadId, history: Vec<ChatMsg>, history_cut: u32, actor: UserId, initiator: Initiator }` (`Serialize`, `Deserialize`; stored in `tasks.input`)
  - `task::enqueue(conn: &mut PgConnection, job_id: JobId, dot_id: DotId, dot_policy_version: i64, input: &TaskInput, idempotency_key: &str) -> Result<TaskId, KatError>`: idempotent on the key; stores the version the routing decision saw
  - `job::create`, `job::create_follow_up`, `job::latest_in_thread` and `task::enqueue` take a connection so that intake (Task 12) runs them in one transaction.

- [ ] **Step 1: Write the failing tests:**
  - in `crates/katwork/tests/policy.rs`: `policy_same_facts_same_verdict`, `answered_only_without_artifacts_else_ready`, `executor_unavailable_is_stopped_provider_unavailable`, `needs_approval_is_needs_input_approval_pending`, `every_stop_reason_maps_to_its_reason_code` (table-driven over the table above), `policy_never_returns_cancelled_or_error`
  - in `crates/katwork/tests/jobs.rs`: `finish_by_policy_records_reason_and_decided_by`, `finish_by_harness_sets_cancelled_and_error`, `cannot_finish_twice`, `decision_event_precedes_job_status`, `conclude_twice_writes_one_reply`, `conclusion_messages_link_to_their_job`, `follow_up_job_links_parent_needs_input_job`, `enqueue_is_idempotent_on_key`
- [ ] **Step 2: Run** `cargo test -p katwork --test policy --test jobs`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: jobs, policy function, status transitions`.

### Task 11: Leases, worker pool and restart recovery

**Files:**
- Create: `crates/katwork/src/jobs/{lease,worker}.rs`, `crates/katwork/tests/leases.rs`

**Interfaces:**
- Consumes: `fence::{Fence, check, lock}` (Task 2); `TaskInput`, `job::{conclude, finish_by_harness}` (Task 10); `run::execute`, `RunAbort`, `RunEnv`, `RunInput`, `RunFacts`, `RunFacts::not_run`, `RunKind`, `Limits`, `StopReason`, `ToolRegistry::builtin` (Task 8); `PgEventSink`, `log::append` (Task 9); `dot::get` (Task 5); `thread::get` (Task 6); `space::get`, `tenancy::resolve` (Task 4); `Core`.
- Produces:
  - `lease::claim(core: &Core, worker_id: &str, ttl: Duration, limits: &Limits) -> Result<Option<ClaimedTask>, KatError>`. Candidates are tasks with `state = 'queued'`, or `state = 'leased'` with an expired lease, **whose job is still running** (`jobs.status IS NULL`), selected with `FOR UPDATE OF tasks, jobs SKIP LOCKED`; every time comes from `now()`. Each candidate is handled in its own transaction. A branch that ends without a run commits and claim moves on to the next candidate (at most 16 per call); the first candidate that gets a lease is returned. Each candidate's transaction takes all its locks before any write, in the global order: the job and task rows from the `SKIP LOCKED` select (never waiting), then the thread row (`SELECT … FROM threads WHERE id = $1 FOR UPDATE`), then the Dot row (`SELECT … FROM dots WHERE id = $1 FOR UPDATE`), all held until commit. Every branch below, including the ones that only emit an event (recovery closing, `dot_busy`, `workspace_paused`), runs after those locks. Then, in order:
    - **closing an expired lease, once:** only a run of this task in state `running` is closed. For each of its open calls (started in `tool_calls` with no end) claim emits `TOOL_CALL_RESULT` and `katwork.tool_status{ok: false}` and marks the call `outcome_unknown`; it marks the run `interrupted`, emits `RUN_ERROR{code: "lease_lost"}` before any later `RUN_STARTED`, and deletes the expired lease row. When no effect started (`effects_started_at IS NULL`), the task goes back to `queued` in the same transaction. A later poll then finds a queued task with no running run, so a task held by the branches below is never closed twice.
    - **deadline:** a task past `created_at + task_deadline`, or whose job is past `created_at + job_deadline`, becomes `failed`, its lease row is deleted, and the job ends with `job::conclude` on `RunFacts::not_run(StopReason::TimeLimit)`
    - **retries exhausted:** at `attempts >= max_attempts` (3), the task becomes `failed`, its lease row is deleted, and the job ends with `finish_by_harness(tx, Error { code: RetriesExhausted })`
    - **effects started:** a task whose expired lease was just closed and whose `effects_started_at` is set becomes `interrupted` and ends the job with `job::conclude` on `RunFacts::not_run(StopReason::Interrupted)`
    - skips a task of a paused workspace and sets `hold_reason = 'workspace_paused'`
    - **eligibility,** against the values stored on the task: the Dot must be `active`, still granted to the thread's Space, and at `policy_version = tasks.dot_policy_version`. A paused Dot ends the job with `job::conclude` on `RunFacts::not_run(StopReason::DotPaused)`; a revoked grant or any other policy change ends it on `RunFacts::not_run(StopReason::PolicyChanged)`. Either way the task becomes `failed` and its lease row is deleted, so queued history never reaches a model after the Dot lost its authority.
    - **concurrency:** counts the Dot's **other** tasks with a live lease (`state = 'leased'`, lease `expires_at > now()`, a different task id); expired leases never count. At `max_concurrency` or more it skips the candidate and sets `hold_reason = 'dot_busy'`. Because the Dot row is held, a second claimer for the same Dot waits on it and then sees the first lease.
    - emits `katwork.job_status {status: null, holdReason}` when it first sets a hold reason
    - otherwise claims it: attempts + 1 (a `Recovery` run when `attempts > 0`, which covers a task re-queued by the closing above or by `lease::abandon`), increments `tasks.lease_epoch`, writes the lease with the same epoch, and clears `hold_reason`

    Because each no-run branch moves the task to a terminal state and finishes its job in the same transaction, a repeated poll never finishes the same job twice.
  - `ClaimedTask { task_id: TaskId, job_id: JobId, dot_id: DotId, workspace_id: WorkspaceId, thread_id: ThreadId, requested_by: UserId, input: TaskInput, attempts: i32, lease_epoch: LeaseEpoch, policy_version: i64, kind: RunKind, remaining: Duration }`. `policy_version` is the task's stored `dot_policy_version`. `remaining` is `GREATEST(LEAST(task created_at + task_deadline, job created_at + job_deadline) - clock_timestamp(), 0)`, computed in SQL as the claim transaction's last statement, so the time spent acquiring the claim is already subtracted and the worker's wall clock is never consulted
  - `HeartbeatStatus::{Continue, Stop(StopReason)}` and `lease::heartbeat(pool, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch, ttl: Duration) -> Result<HeartbeatStatus, KatError>`: takes `fence::check` (the task row, shared) and then updates the lease row, in the global order; it renews `expires_at = now() + ttl` only while the check passes, so an expired lease is never revived; `Stop(WorkspacePaused)` once the workspace is paused. A task that is no longer leased under this epoch (reclaimed, cancelled or finished by the harness) is `lease_lost`.
  - `lease::complete(core: &Core, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch, facts: RunFacts) -> Result<(), KatError>`: its transaction first reads the task's `job_id` and `thread_id` without locking, then takes, in the global order and before any write, the job row (`FOR UPDATE`), the task row with `fence::lock`, and the thread row (`FOR UPDATE`). A task that is no longer leased under this epoch is `lease_lost`, and the worker drops its facts, because whoever ended the lease (reclaim or cancel) already closed the run. Otherwise, in one transaction: `job::conclude` (with `idem_key` = the task id); the task state (`done`, or `interrupted` for `WorkspacePaused`) and `pending_tool_call`; the lease release. Then it commits and notifies the hub.
  - `worker::spawn_pool(core: Arc<Core>, n: usize, ttl: Duration) -> JoinSet<()>`. Each worker:
    - polls `claim` (every 1 s when idle)
    - builds the `Tenant` with `tenancy::resolve(&core.pool, requested_by, workspace_id)`, loads the Dot and the thread's Space policy, passes `policy_version` from the claim, and creates a `PgEventSink` for a new `RunId` with `fence = Some(Fence { task_id, epoch })`
    - runs `run::execute` with `Limits::default()`, `ToolRegistry::builtin()`, `core.registry`, `reply_message_id = MessageId::new()` and `deadline = worker::run_deadline(claimed_at, remaining, run_wall_clock)`, where `claimed_at = tokio::time::Instant::now()` is sampled as soon as `claim` returns
    - heartbeats every `ttl / 3`; on `Stop(reason)` it cancels the run and sets `facts.stop = Some(reason)`. It cancels the run and drops its facts on `lease_lost`, on a failed renewal, or as soon as the last successful renewal is older than `ttl`.
    - on `Ok(facts)` finishes with `lease::complete`; on `Err(RunAbort::LeaseLost)` drops the run, because the reclaim or cancel path already closed it; on `Err(RunAbort::Infrastructure { .. })` calls `lease::abandon`
  - `worker::run_deadline(claimed_at: tokio::time::Instant, remaining: Duration, run_wall_clock: Duration) -> tokio::time::Instant`: `claimed_at + min(run_wall_clock, remaining)`. A zero `remaining` gives `claimed_at`, so the run stops `TimeLimit` before its first model attempt. Only the monotonic clock and the database-derived duration are used, so a skewed worker clock cannot move the deadline.
  - `lease::abandon(core: &Core, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch) -> Result<(), KatError>`: in a transaction that takes only `fence::lock` (the task row, then its lease row), sets the lease's `expires_at = now()`, so the next `claim` applies the normal reclaim rules (re-queue when no effect started, otherwise `interrupted`, and `retries_exhausted` after the third attempt). An infrastructure failure is never turned into a policy verdict. If `abandon` fails as well, the lease simply expires.
  - TTL defaults to 60 s, with a heartbeat every 20 s.

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/leases.rs`:
  - `two_workers_never_claim_same_task`: 50 tasks, 2 workers in parallel, each task claimed once
  - `expired_lease_is_reclaimed_once`: worker A claims and stops heartbeating; after the TTL, worker B claims it with a higher epoch; A's late `heartbeat` and `complete` are rejected with `lease_lost`
  - `expired_lease_after_side_effect_is_interrupted_not_rerun`
  - `reclaim_does_not_duplicate_reply`
  - `reclaimed_run_closes_open_calls_before_new_run_started`
  - `held_recovery_closes_old_run_once`: an expired lease while the workspace is paused; three polls emit one `RUN_ERROR` for the old run and leave the task `queued`; after unpausing, exactly one `Recovery` run starts
  - `retries_exhausted_finishes_job_error`
  - `cancelled_job_task_is_never_claimed`: a job cancelled while its task is queued is never claimed, and no model call happens
  - `deadline_branch_finishes_once_across_repeated_polls`: the task ends `failed` with no lease row, and one job status is written however often workers poll
  - `retries_exhausted_branch_finishes_once_across_repeated_polls`
  - `one_dot_runs_one_task_at_a_time`
  - `expired_claim_racing_paused_dot_claim_does_not_deadlock`: one worker claims an expired task (whose recovery closing emits events) while another claims a fresh task of the same paused Dot in the same thread. The two calls start together from a `tokio::sync::Barrier`, 50 times over; both always finish, and no error carries SQLSTATE `40P01`
  - `simultaneous_claims_for_one_dot_dispatch_once`: two workers claim two different tasks of one Dot (`max_concurrency = 1`) at the same moment behind a barrier; exactly one lease is granted and the other task gets `dot_busy`
  - `expired_lease_does_not_block_reclaim_at_max_concurrency_one`: the Dot's only task has an expired lease; it is reclaimed rather than held as `dot_busy`
  - `paused_workspace_claims_nothing`
  - `dot_paused_while_queued_is_not_run`: the Dot is paused between enqueue and claim; the task ends `failed`, the job `stopped/dot_paused`, and the provider receives no request
  - `grant_revoked_while_queued_is_not_run`: the thread's Space grant is revoked between enqueue and claim; the job ends `stopped/policy_changed` and the provider receives no request
  - `infrastructure_abort_goes_through_reclaim`: an `Infrastructure` abort before any effect re-queues the task as a `Recovery`; the job gets no verdict from the aborted run
  - `worker_cancels_run_when_renewal_fails`: heartbeats start failing; the run is cancelled within `ttl`, before the lease can be reclaimed
  - `pause_between_attempts_before_heartbeat_has_one_terminal_run_event`: the workspace is paused after the first attempt and before the next heartbeat; `guards::authorize` stops the run, and the log holds exactly one terminal event for it, `RUN_FINISHED`, after `katwork.job_status stopped/workspace_paused`
  - `heartbeat_pause_has_one_terminal_run_event`: the heartbeat sees the pause and cancels the run mid-stream; the log holds exactly one terminal event for it, `RUN_ERROR{cancelled}`, and no `RUN_FINISHED`
  - `pausing_workspace_interrupts_running_task`: the task ends `interrupted` and the job `stopped/workspace_paused`
  - `task_past_deadline_finishes_time_limit`
  - `run_deadline_clamps_to_the_shorter_limit`: `remaining` of 10 s with a 90 s wall clock gives `claimed_at + 10 s`; zero gives `claimed_at`
  - `deadline_comes_from_database_time`: a task whose `created_at` is 4 min 50 s in the past by Postgres time is claimed with `remaining` of about 10 s, whatever the worker's clock says
  - `zero_remaining_makes_no_model_call`: a run started with `remaining = 0` stops `TimeLimit` and the provider receives no request
- [ ] **Step 2: Run** `cargo test -p katwork --test leases`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: leased worker pool with restart recovery`.

### Task 12: Message → job pipeline (SP1 routing)

**Files:**
- Create: `crates/katwork/src/pipeline.rs`, `crates/katwork/src/jobs/quota.rs`, `crates/katwork-server/src/routes/jobs.rs`, `crates/katwork-server/tests/pipeline.rs`
- Modify: `crates/katwork/src/jobs/job.rs` (`job::cancel`), `crates/katwork-server/src/routes/threads.rs` (`POST` messages calls `pipeline::submit` and returns its `job_id`), `crates/katwork-server/src/main.rs` (`serve` spawns the worker pool)

**Interfaces:**
- Consumes: Tasks 4–11.
- Produces:
  - `UserPost { body: String, client_msg_id: Option<String> }` and `Submitted { message_id: MessageId, job_id: JobId, created: bool }`
  - `pipeline::intake(tx: &mut Transaction<'_, Postgres>, core: &Core, tenant: &Tenant, thread_id: ThreadId, post: UserPost) -> Result<Submitted, KatError>`: the whole intake inside the caller's transaction. It takes its locks before any write, in the global order: the thread row first (`SELECT … FROM threads WHERE id = $1 FOR UPDATE`, which also serializes posts to the thread), then, once routing has chosen a Dot, that Dot's row inside `quota::check`. Steps:
    1. lock the thread row; then look up the user message for `(thread, author, client_msg_id)`. If it exists, return its stored `{message_id, job_id}`; because a message, its job and its task commit together, a stored message always has its `job_id`.
    2. reads only: `job::latest_in_thread`, then routing (below); if a Dot was chosen, `quota::check` locks its row and checks the quotas
    3. writes: `message::append` for the user message; `job::create_follow_up` when the latest job is `needs_input`, otherwise `job::create` (ADR-0002), where the unique `jobs.request_message_id` rejects any second job for the message; then at once `message::set_job` for the request message, whatever routing decided
    4. `task::enqueue`, or `job::conclude` for a job that ends at intake, plus the notices, each written with `job_id` set to the job
  - `pipeline::submit(core: &Core, tenant: &Tenant, thread_id: ThreadId, post: UserPost) -> Result<Submitted, KatError>`: begins a transaction, runs `intake`, commits, then notifies the hub. A crash at any point before the commit leaves no message, job or task, so the client's retry with the same `client_msg_id` starts clean; a concurrent retry waits on the thread lock, then finds the first post's message and returns its ids. The route answers 201 when `created`, otherwise 200.
  - Intake rules inside `intake`:
    - SP1 routing (spec §6.2 Stage 0 subset; Laya arrives in SP4). The eligible set is the thread team's Dots that are `active` and granted to the thread's Space. Mentions from `mentions::parse` are matched, in text order, against the team's Dot names (case-insensitive, NFC; names are unique within a team, Task 5):
      1. the first mention that names an eligible Dot gets the task; later mentions are ignored in SP1 (team jobs arrive in SP4)
      2. a mention of a team Dot that is not eligible posts a `System` notice, `dot_paused` or `dot_not_in_space`, and matching moves on to the next mention
      3. with no eligible mention, the Lead gets the task if it is eligible, otherwise the first eligible Dot in team order (`team_members.position`)
      4. with no eligible Dot at all, the job ends with `job::conclude` on `RunFacts::not_run(StopReason::NoEligibleDot)`
    - quotas before enqueue: `quota::check`; when exceeded, the job ends with `job::conclude` on `RunFacts::not_run(StopReason::QuotaExceeded)`, which posts a localized notice. Nothing is dropped silently.
    - in a paused workspace the task is still enqueued and waits (Task 11), and the pipeline posts a `System` notice `workspace_paused`
    - every `job::conclude` call here uses the job id as `idem_key`, and each notice uses the idempotency key `"notice:{job_id}:{code}"`, so a retried post never doubles them
    - builds `TaskInput` with `build_history`, and calls `task::enqueue` with the routed Dot's `policy_version`, read after `quota::check` has locked the Dot row, and key `"job:{job_id}:dot:{dot_id}"`
  - `quota::check(conn: &mut PgConnection, dot: &Dot, sender: UserId) -> Result<Option<StopReason>, KatError>`, run inside intake's transaction:
    - it first locks the Dot row (`FOR UPDATE`): the last lock intake takes, after the thread row and before any write
    - the daily budget is `dot.daily_token_budget` (`None` means no cap), compared with the sum of `model_calls.charged_tokens` for that Dot over the last 24 hours, so missing usage counts at its conservative charge, never as zero
    - the rate limit is at most 6 jobs per minute per (Dot, sender), counted from tasks routed to that Dot whose job that sender requested in the last 60 s
    - intake's `task::enqueue` records the routing in the same transaction, so a concurrent submission for the same Dot waits on the Dot row and then counts it
  - `History { messages: Vec<ChatMsg>, cut: u32 }` and `pipeline::build_history(messages: &[Message], dot_id: DotId, names: &HashMap<Author, String>, budget_chars: usize) -> History`:
    - the newest messages up to `budget_chars` (24,000); the latest user message is always kept whole, and the number of cut messages is recorded
    - the Dot's own messages map to `Assistant`; other Dots and people map to `User` with their server-side `name`
    - system notes become plain `User` notes, never the system role
  - `job::cancel(pool, hub: &EventHub, tenant: &Tenant, job_id: JobId) -> Result<(), KatError>`: another workspace's job is `NotFound`; a finished job is `job_already_finished`. In one transaction it takes, before any write and in the global order, the job row, then its task rows in ascending id, then the thread row, all `FOR UPDATE`. Each queued or leased task becomes `cancelled` and its lease row is deleted. For a running run it closes the open calls (`TOOL_CALL_RESULT`, `katwork.tool_status{ok: false}`, ledger state `outcome_unknown`), marks the run `cancelled`, and emits `RUN_ERROR{code: "cancelled"}` and `katwork.dot_state idle`. It then calls `finish_by_harness(tx, Cancelled)`, commits and notifies the hub. The old worker's next heartbeat or write finds the task no longer leased and gets `lease_lost`.
  - Route `POST /api/workspaces/{workspace_id}/jobs/{job_id}/cancel` returns 202, 404 across tenants, and 409 `job_already_finished`.
  - `serve` builds `Core` and runs `worker::spawn_pool(core, 4, Duration::from_secs(60))`.
  - Event order for an answered job: `RUN_STARTED`, `katwork.run_started`, `katwork.dot_state working`, `TEXT_MESSAGE_*`, `katwork.decision`, `katwork.message_stored`, `katwork.job_status answered`, `katwork.dot_state idle`, `RUN_FINISHED`. `katwork.job_status` is the only success signal; cancelled, needs-input and interrupted outcomes are explicit job statuses, never an omitted one.

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/pipeline.rs` (FakeProvider):
  - `message_without_mention_goes_to_lead_and_job_answered`
  - `mention_routes_to_named_dot_including_thai_name`
  - `paused_dot_leaves_routing_set`
  - `provider_unavailable_ends_job_stopped_with_reason`: Review Focus 5; the notice code is `provider_unavailable` and the job finishes within the test timeout
  - `provider_quota_and_rate_limit_end_stopped_with_named_reason`
  - `local_only_space_with_only_hosted_keys_stops_residency_denied`: through the full pipeline; the notice code is `residency_denied` and no request leaves the install
  - `demo_mode_reply_is_flagged_synthetic`: the reply from the messages API has `synthetic: true`
  - `full_event_sequence`: the order above
  - `stream_message_id_equals_stored_reply_id`
  - `stored_reply_snapshot_replaces_provisional_text`: the scripted deltas say "Draft" while the final turn says "Final"; `katwork.message_stored` and the stored message both carry "Final"
  - `restart_mid_run_posts_exactly_one_reply`
  - `retry_with_same_client_msg_id_creates_one_job`
  - `concurrent_retries_with_same_client_msg_id_create_one_job`: two posts with the same `client_msg_id` race; one message, one job and one task exist, and both responses carry the same ids
  - `intake_rolled_back_leaves_no_message_job_or_task`: runs `pipeline::intake` to its last step and drops the transaction without committing (a crash between any two steps rolls back the same way); no message, job or task remains, and a retry then creates exactly one of each
  - `retry_after_intake_stop_returns_same_job`: a post whose job ends at intake (quota) is retried; the same job comes back, no second job or notice appears, and in the database the request message and the notice both carry that `job_id`
  - `reply_to_needs_input_job_starts_linked_follow_up`
  - `history_budget_keeps_latest_user_message_whole`
  - `other_dots_messages_are_user_role_with_name`
  - `seventh_message_in_a_minute_is_stopped_quota_exceeded`
  - `two_simultaneous_submissions_with_five_recent_jobs_admit_one`: one is enqueued and the other ends `stopped/quota_exceeded`
  - `daily_budget_counts_charged_tokens_when_usage_is_missing`
  - `first_eligible_mention_wins_among_several`
  - `paused_lead_routes_to_first_active_specialist`
  - `no_eligible_dot_ends_job_stopped`
  - `cancel_ends_job_cancelled`
  - `cancel_racing_complete_does_not_deadlock`: `job::cancel` and the worker's `lease::complete` for the same job start together from a `tokio::sync::Barrier`, 50 times over; both always finish (one wins and the other gets `job_already_finished` or `lease_lost`), and no error carries SQLSTATE `40P01`
  - `quota_reject_racing_claim_does_not_deadlock`: an intake that ends `stopped/quota_exceeded` and a claim of another task of the same Dot in the same thread start together from a `tokio::sync::Barrier`, 50 times over; both always finish, and no error carries SQLSTATE `40P01`
  - `cancel_queued_job_terminalizes_its_task`: the task becomes `cancelled` with no lease row, and no worker ever claims it
  - `cancel_other_workspace_job_is_404`
- [ ] **Step 2: Run** `cargo test -p katwork-server --test pipeline`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: message to job pipeline with mention routing`.

### Task 13: Web app (login, reset, team, chat)

**Files:**
- Create: `web/` (Vite React TS), `web/index.html` (with a pre-paint theme script), `web/scripts/check-contrast.ts`, `web/src/{main.tsx,app.tsx,api.ts,sse.ts,theme.css}`, `web/src/i18n/{index.ts,en.json,th.json}`, `web/src/pages/{Login,ResetRequest,ResetConfirm,Team,Chat}.tsx` with `*.test.tsx`, `crates/katwork-server/src/static_files.rs`, `crates/katwork-server/tests/i18n.rs`
- Modify: `crates/katwork-server/src/app.rs` (serve `web/dist` at `/`, SPA fallback), `Dockerfile` (web stage `node:22-bookworm-slim`), `.github/workflows/ci.yml` (`node-version: 22`, `npm run check:contrast`)

**Interfaces:**
- Consumes: the auth, workspaces, teams, threads, events and jobs routes; `crates/katwork-server/tests/fixtures/sse/thai_emoji.sse` (Task 9); `ReasonCode::ALL` (Task 2).
- Produces:
  - `sse.ts`: `subscribe(url, onEvent): () => void`, using the browser `EventSource` (no Rust or SDK client) and its native `Last-Event-ID` resume. It keeps the last seq and drops duplicates after a reconnect, and folds text by `messageId` and tool arguments by `toolCallId`, never into "the last message". It follows Task 9's folding rules: `TEXT_MESSAGE_START` resets a message's provisional text, `katwork.message_stored` replaces it with the stored body, `synthetic` flag and any question with its options, and the end of a run drops that run's unconfirmed provisional message.
  - `i18n.t(key, vars?)` with locales `en` and `th`; the locale switch persists in `localStorage`, and the locale is read with try/catch
  - `i18n.formatDate(date, { calendar: 'gregory' | 'buddhist' })`: Gregorian in the UI chrome, Buddhist Era where a template asks for it
  - every `ReasonCode` has a `reason.<code>` key in both locales
  - a Stop button in the chat that calls the cancel route while a job runs
  - CSS tokens in `theme.css`, redefined under `[data-theme=dark]`, and a pre-paint theme script in `index.html` that sets `data-theme` before first paint
  - `npm run check:contrast`: fails when any text/surface token pair has contrast below 4.5:1
  - copy calls the coworkers "Dots" in both locales; Thai copy describes them as ผู้ช่วย AI and never says พนักงาน AI

- [ ] **Step 1: Write the failing tests** (Vitest + Testing Library, and one Rust test):
  - `login_shows_localized_error_on_401`, in both `en` and `th`
  - `reset_request_shows_same_message_regardless_of_email`
  - `team_page_lists_lead_first_with_thai_names`
  - `chat_streams_deltas_into_one_bubble`: a mocked EventSource with 3 deltas gives 1 bubble with the concatenated text
  - `chat_streams_thai_and_emoji_split_at_every_byte`: a mocked EventSource fed from the Task 9 fixture
  - `sse_folds_by_id_and_drops_duplicates_after_reconnect`
  - `provisional_text_dropped_after_lease_loss_and_recovery`: text from the first run, its `RUN_ERROR{lease_lost}`, then a recovery run with its stored reply; the final transcript equals the stored messages
  - `provisional_text_dropped_after_provider_failure`: text, then the run ends `stopped/provider_unavailable` with no stored reply; the final transcript equals the stored messages (the notice only)
  - `final_text_differs_from_deltas_shows_stored_text`: through the real reducer
  - `new_turn_start_resets_provisional_text`
  - `ask_user_question_and_options_render_from_snapshot`
  - `synthetic_reply_shows_badge`: a message with `synthetic: true` shows a visible "Demo / ตัวอย่าง" badge; the same text with `synthetic: false` shows none
  - `stop_button_cancels_running_job`
  - `replayed_run_error_cleared_by_later_success`
  - `interrupt_does_not_clear_run_error`
  - `composer_enter_during_ime_composition_does_not_send`
  - `transcript_is_live_region`
  - `format_date_buddhist_era_in_thai`
  - `every_key_exists_in_both_locales`: `en.json` and `th.json` have identical key sets
  - Rust, in `crates/katwork-server/tests/i18n.rs`: `reason_codes_have_en_and_th_messages`
- [ ] **Step 2: Run** `cd web && npm test`. Expected: FAIL.
- [ ] **Step 3: Implement** the pages, plain CSS with tokens, and keyboard-reachable controls. Serve `dist/` from Axum with SPA fallback; `/api/*` never falls back.
- [ ] **Step 4: Run** `npm test && npm run check:contrast && npm run build && cargo test -p katwork-server --locked`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat: web app for login, reset, team and streaming chat`.

### Task 14: Compose smoke, backup before migrate, HTTPS profile

**Files:**
- Create: `scripts/smoke.sh`, `scripts/compose-backup.sh`, `Caddyfile`, `docker-compose.smoke.yml`, `crates/katwork-server/tests/e2e_compose.rs` (`#[ignore]`, run in a CI job)
- Modify: `docker-compose.yml`, `Dockerfile` (build argument `CARGO_FEATURES`), `crates/katwork/{Cargo.toml,src/models/fake.rs,src/models/registry.rs}` (the `smoke-fixtures` feature), `.env.example` (`KATWORK_HTTPS_HOST`), `.github/workflows/ci.yml` (`e2e` and `upgrade_from_previous_release` jobs), `README.md` (a "Try it" section, HTTPS, backups and upgrades)

**Interfaces:**
- Consumes: everything above.
- Compose start order. The runtime image is distroless with no shell, so each step is its own service:
  1. `backup`: the `postgres:17` image on the `backend` network, run once. `scripts/compose-backup.sh` runs `pg_dump --format=custom` into the `./backups` bind mount as `/backups/katwork-<UTC timestamp>.dump`, but only when the database already has KatWork's schema (`_sqlx_migrations` exists); on a fresh database it does nothing.
  2. `migrate`: the KatWork image with `command: ["migrate"]`, after `backup` completes successfully.
  3. `katwork`: `serve`, after `migrate` completes successfully.
- Produces, for the restart scenario, a deterministic in-flight run:
  - the `katwork` crate gets a Cargo feature `smoke-fixtures`, off by default and never enabled in release images. With it, `FakeProvider::demo_held(hold: Duration) -> FakeProvider` behaves like `demo()`, except that each run's first attempt waits `hold` before its first chunk; `Registry::from_config` selects it when `KATWORK_SMOKE_HOLD_SECS` is set. Without the feature the variable is ignored.
  - the `Dockerfile` takes a build argument `CARGO_FEATURES` (empty by default) and passes it to `cargo build --features`
  - `docker-compose.smoke.yml` builds with `CARGO_FEATURES=katwork/smoke-fixtures`, sets `KATWORK_SMOKE_HOLD_SECS=20` (under the 30 s first-chunk timeout) and `KATWORK_DEMO=1`, and sets `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` to empty strings, so demo mode selects the fake even when the host has provider keys
- An optional `https` profile adds a `caddy:2` service that publishes ports 80 and 443 and proxies `KATWORK_HTTPS_HOST` to `katwork:3000`. With it, `KATWORK_PUBLIC_URL=https://<host>`, so the session cookie is `Secure`.

- [ ] **Step 1: Write `scripts/smoke.sh`.** It must:
  1. Export `COMPOSE_FILE=docker-compose.yml:docker-compose.smoke.yml` once, at the top, so that every `docker compose` command in the script, restarts included, uses the override. Then run `docker compose up -d --build` with `KATWORK_DEMO_PASSWORD` set, and assert the effective configuration: `docker inspect` on the `katwork` container shows `KATWORK_SMOKE_HOLD_SECS=20`, `KATWORK_DEMO=1`, and empty `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`.
  2. Wait for `/healthz`.
  3. Run `seed-demo`.
  4. Log in as demo and post `"สวัสดี ช่วยแนะนำทีมหน่อย"` with `Origin: $KATWORK_PUBLIC_URL` and a `client_msg_id`.
  5. Read SSE until `katwork.job_status`.
  6. Assert status `answered`, and that the Dot reply from the messages API has `synthetic: true`.
  7. In a second scenario, post again, then poll Postgres (`docker compose exec -T postgres psql …`) until the task is `leased` and its `runs` row is `running`. Only then run `docker compose kill katwork` and `docker compose up -d katwork`, and assert the same effective configuration on the new container. After recovery, assert that:
     - a second `runs` row with `kind = 'recovery'` exists for the task, so a recovery attempt really happened
     - the job has exactly one Dot reply message
     - the job has exactly one `katwork.decision` event and one terminal `katwork.job_status`, and its status is `answered`
  8. In a third scenario, run `docker compose up -d` again on the existing volume and assert that a new dump appeared in `./backups`.
  9. Print `SMOKE OK`.
- [ ] **Step 2: Run** `scripts/smoke.sh`. Expected: FAIL before wiring, PASS after.
- [ ] **Step 3: Implement** the `backup` and `migrate` services, `scripts/compose-backup.sh`, the `https` profile and `Caddyfile`, and any glue the smoke test exposes. Document HTTPS, backups and restore in `README.md`.
- [ ] **Step 4: Add the CI jobs.** `e2e` runs `scripts/smoke.sh`. `upgrade_from_previous_release` is skipped while no release tag exists; after the first tag, it starts the previous release's image, seeds demo data, then runs this build's `migrate` and the smoke test.
- [ ] **Step 5: Run** `scripts/smoke.sh`. Expected: `SMOKE OK`.
- [ ] **Step 6: Commit** with `test: compose smoke, backup before migrate, https profile`.

---

## Dependency order and parallelism

```
1 → 2 → 3 → 4 → 5 → 6 → 9 → 8 → 10 → 11 → 12 → 13 → 14
    2 → 7 ─────────────────────↗ (7 must merge before 8)
```

- Task 7 (model gateway) depends only on Task 2: it takes `ModelTier` and `SpacePolicy` from `crate::types` and `Config` from Task 1. It can run in parallel with Tasks 3–6 and 9, and must merge before Task 8. Tasks 7 and 9 each add one field to `Core` in `runtime.rs`; whichever merges second rebases.
- Task 9 comes before Task 8 because the agent loop emits through `EventSink`.
- Task 13 can start its UI against mocked APIs after Task 6 and integrate after Task 12.
