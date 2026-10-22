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
- Rust (edition 2024): tokio 1, tokio-util (`CancellationToken`), axum 0.8, sqlx (exact release pinned at Task 2 start after a crates.io check; postgres, runtime-tokio, tls-rustls, uuid, chrono, json, migrate), serde/serde_json, uuid 1 (v7), chrono, argon2 0.5, lettre 0.11, rig-core (exact release checked and pinned at Task 7 start; used directly, with explicit reqwest and rustls features), reqwest (pinned at Task 7 start to the release rig-core uses), async-trait, futures (`BoxStream`), unicode-segmentation, tracing, tracing-subscriber, thiserror 2.
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
  - run wall-clock 90 s; token budget per run 200,000 input+output tokens summed over attempts; task deadline 5 minutes; job deadline 10 minutes. Every model or tool call's timeout is `min(its own, the run's remaining time)`
  - tool timeout 60 s; a model stream gets 30 s to its first chunk and at most 30 s between chunks; at most 1 retry per model call, only before the first chunk
  - loop detection: the same tool with the same canonical arguments 3 times in a row, **or** 5 times within the last 10 calls, stops the run before that call runs
  - tool results capped at 8,000 Unicode scalar values, the truncation marker included
  - the history passed to a run is the newest messages up to 24,000 characters
- **Secrets.** Install secrets come only from environment variables; `.env.example` lists them and never holds values. `Config` never prints a secret (its `Debug` redacts them). Per-Dot credentials (SP2) live in an encrypted vault, never in env. Every child process gets an env allowlist, never the server's environment.
- **Demo mode with no keys must work.** Synthetic replies carry `messages.synthetic = true`, set only on the `FakeProvider::demo` path; the UI badge reads the flag (spec §15).
- **Reply vs send.** A *reply* goes to the thread the request came from; the harness fills the target and it is not gated. A *send* goes to any other target, or is a proactive post; it is `Outward` and produces a Proposal (SP3). In SP1 the only write is the reply, written once by `job::conclude` (Task 10).
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
6. **Tool gate bypass.** A tool outside the Dot's allowlist, a destructive or outward tool, a tool call from a failed or `MaxTokens` turn, or a call after the allowlist changed mid-run. Expected: never dispatched; an error result, `needs_input/approval_pending` or `stopped/policy_changed`. (Task 8.)
7. **Cross-site and wrong-host requests.** A POST from another origin, or any request with an unknown `Host`. Expected: 403 before a handler runs. (Task 3.)
8. **Duplicate or stranded messages.** A client retries a post, two retries race, or the server crashes mid-intake. Expected: one message, one job and one task, or none at all; never a message without its job. (Task 12.)

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
- Create: `crates/katwork/migrations/0001_init.sql`, `crates/katwork/src/{ids.rs,types.rs,db.rs,runtime.rs}`, `crates/katwork/tests/schema.rs`, `crates/katwork-server/src/error.rs`
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
  - `types::SideEffect::{Read, Navigate, WorkspaceRead, Draft, WorkspaceWrite, Exec, CredentialUse, Destructive, Outward(OutwardKind)}` and `types::OutwardKind::{Send, Publish, Pay, Share, ExternalApi}`, with `SideEffect::wire(&self) -> &'static str` giving `read, navigate, workspace_read, draft, workspace_write, exec, credential_use, destructive, outward_send, outward_publish, outward_pay, outward_share, outward_external_api`
  - `types::ReasonCode` with `wire(&self) -> &'static str` and `ReasonCode::ALL`: `provider_unavailable, provider_rate_limited, provider_quota, provider_auth_failed, model_protocol_error, time_limit, quota_exceeded, workspace_paused, step_limit, token_budget, output_truncated, loop_detected, policy_changed, approval_pending, question_pending, interrupted_review_required, dot_paused, retries_exhausted, cancelled, lease_lost`. Each value is a job `reason`, a notice body or a `RUN_ERROR` code, and has a message in both locales (Task 13).
  - `db::connect(url: &str) -> Result<PgPool, KatError>` and `db::migrate(pool: &PgPool) -> Result<(), KatError>`
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
  - `messages(id, workspace_id, thread_id, author_kind, author_user_id NULL, author_dot_id NULL, body, client_msg_id TEXT NULL CHECK (char_length(client_msg_id) <= 64), idempotency_key TEXT NULL UNIQUE, synthetic BOOLEAN NOT NULL DEFAULT false, job_id UUID NULL, created_at, UNIQUE (thread_id, author_user_id, client_msg_id))`; the `job_id` FK is added once `jobs` exists
  - `jobs(id, workspace_id, thread_id, request_message_id UUID NOT NULL UNIQUE REFERENCES messages(id), requested_by UUID NOT NULL REFERENCES users(id), parent_job_id UUID NULL REFERENCES jobs(id), status NULL, reason TEXT NULL, decided_by TEXT NULL CHECK (decided_by IN ('policy','harness')), decision_seq BIGINT NULL, contract_sha256 TEXT NULL, created_at, finished_at NULL)`
    - `status` NULL means running, with `CHECK (status IS NULL OR status IN ('accepted','ready','answered','stopped','needs_input','cancelled','error'))`
    - `reason` holds a `ReasonCode` wire value; `decision_seq` is the per-thread seq of the job's `katwork.decision` event; `contract_sha256` is reserved and always NULL in SP1
    - one job per request message: the unique `request_message_id` means a retried or concurrent intake can never create a second job
  - `tasks(id, workspace_id, job_id, dot_id, input JSONB, state, attempts INT NOT NULL DEFAULT 0, max_attempts INT NOT NULL DEFAULT 3, idempotency_key TEXT UNIQUE, effects_started_at TIMESTAMPTZ NULL, hold_reason TEXT NULL, lease_epoch BIGINT NOT NULL DEFAULT 0, pending_tool_call JSONB NULL, created_at)`
    - `CHECK (state IN ('queued','leased','done','failed','interrupted','cancelled'))` and `CHECK (hold_reason IN ('dot_busy','provider_quota','workspace_paused'))`
    - terminal states: `done` (concluded by the policy after a run), `failed` (the harness gave up: deadline or retries exhausted), `interrupted` (stopped after a side effect, or by a workspace pause), `cancelled` (its job was cancelled). Every move to a terminal state deletes the task's `task_leases` row in the same transaction.
    - `pending_tool_call` is `{tool_call_id, name, args_sha256}`, kept when a run stops on `NeedsApproval`
  - `task_leases(task_id PRIMARY KEY, worker_id, lease_epoch BIGINT NOT NULL, expires_at)`; `expires_at` is always computed with Postgres `now()`, never the worker clock
  - `runs(id, workspace_id, job_id, task_id, dot_id, kind, lease_epoch BIGINT NOT NULL, dot_config_hash TEXT NOT NULL, model TEXT NULL, state, stop_reason TEXT NULL, started_at, ended_at NULL)` with `kind IN ('initial','repair','recovery')` and `state IN ('running','finished','interrupted','cancelled','error')`. Repairs (at most 2, set by the policy from SP3) and recoveries (owned by the scheduler) are counted separately through `kind`.
  - `tool_calls(id, workspace_id, run_id, step INT, tool_call_id TEXT, tool_name TEXT, side_effect TEXT, args_sha256 TEXT, state TEXT, result TEXT NULL, started_at, ended_at NULL, UNIQUE (run_id, step, tool_call_id))`, with `side_effect` checked against the 13 `SideEffect::wire` values and `state IN ('started','succeeded','failed','timed_out','outcome_unknown','refused')`. `result` holds the exact text passed to the next model call.
  - `model_calls(id, workspace_id, space_id, job_id, task_id, run_id, dot_id, provider_id, model, residency, attempt INT, input_tokens BIGINT NULL, cache_read_tokens BIGINT NULL, cache_write_tokens BIGINT NULL, output_tokens BIGINT NULL, usage_complete BOOLEAN NOT NULL, cost_micros BIGINT NULL, latency_ms INT, outcome TEXT, created_at)`: one row per model attempt, retries included; a missing count is NULL, never 0
  - `events(thread_id, seq BIGINT, workspace_id, run_id NULL, kind, payload JSONB, created_at, PRIMARY KEY (thread_id, seq))`

- [ ] **Step 1: Pin versions and check the MSRV.** Check crates.io for the latest released sqlx and pin it exactly (`=x.y.z`) in `[workspace.dependencies]` with the features above; add `uuid` (v7, serde), `chrono` (serde) and `serde`. Add the CI job `msrv`: `cargo +<rust-version> check --workspace --locked`. If the locked graph needs a newer compiler than 1.85 (let-chains under edition 2024 need 1.88), raise `rust-version` in `Cargo.toml` to the lowest version that passes and use the same value in the job.
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
  - `missing_usage_is_stored_as_null`
  - `space_allow_external_has_no_default`: an insert without `allow_external` errors
  - `side_effect_wire_values_pass_the_schema_check`: every `SideEffect::wire` value inserts into `tool_calls.side_effect`, and `'shell'` is rejected
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
  - `#[async_trait] trait Mailer: Send + Sync { async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), KatError>; }`, with `ConsoleMailer` (logs) and `SmtpMailer` (lettre, from `Config::smtp`)
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
  - `workspace::set_paused(pool, &Tenant, paused: bool) -> Result<(), KatError>`: owner or admin, otherwise `forbidden`
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
  - `Dot { id: DotId, default_space_id: SpaceId, spaces: Vec<SpaceId>, name: String, role: DotRole, persona: String, instructions: String, tool_allowlist: Vec<String>, model_tier: ModelTier, autonomy: Autonomy, status: DotStatus, appearance: Appearance, max_concurrency: u16, policy_version: i64 }`
  - `Dot::config_hash(&self) -> String`: hex SHA-256 of the canonical JSON of `persona`, `instructions`, `tool_allowlist`, `model_tier`, `autonomy` and `policy_version`
  - `DotRole::{Lead, Researcher, Writer, Analyst, Scheduler, Reviewer, Custom(String)}` and `Autonomy::{AskFirst, Standard}`
  - `Appearance { avatar: String, accent: String }`, stored in `dots.appearance`
  - `NewDot { name, role, persona, instructions, tool_allowlist, model_tier, autonomy, appearance }`
  - `DotPatch { name, persona, instructions, tool_allowlist, model_tier, autonomy, status, appearance }`, every field an `Option`
  - `Team { id: TeamId, space_id: SpaceId, name: String }` and `TeamView { team: Team, members: Vec<Dot> }`, with the lead first
  - `roles::default_roster(locale: Locale) -> Vec<NewDot>`: 6 Dots with English and Thai display names, for example Lead/หัวหน้าทีม, Researcher/นักวิจัย, Writer/นักเขียน, Analyst/นักวิเคราะห์, Scheduler/เลขา, Reviewer/ผู้ตรวจ. Each role gets a distinct avatar, and every Dot gets `tool_allowlist = ["read_thread", "ask_user"]`.
  - `dot::hire(pool, &Tenant, SpaceId, NewDot) -> Result<DotId, KatError>`: the Space becomes the Dot's default and its first grant
  - `dot::get(pool, &Tenant, DotId) -> Result<Dot, KatError>`
  - `dot::update(pool, &Tenant, DotId, DotPatch) -> Result<Dot, KatError>`
  - `dot::grant_space(pool, &Tenant, DotId, SpaceId) -> Result<(), KatError>` and `dot::revoke_space(pool, &Tenant, DotId, SpaceId) -> Result<(), KatError>`; revoking the default Space is 422 `default_space_required`
  - Any change to `tool_allowlist`, `status` or grants increments `policy_version` in the same statement.
  - `team::create_default(pool, &Tenant, SpaceId, Locale) -> Result<TeamId, KatError>` and `team::get(pool, &Tenant, TeamId) -> Result<TeamView, KatError>`
- Routes:
  - `GET/POST /api/workspaces/{workspace_id}/spaces/{space_id}/dots`. "Hire" is the `POST`, with role, name and persona.
  - `GET/PATCH /api/workspaces/{workspace_id}/dots/{dot_id}`
  - `PUT/DELETE /api/workspaces/{workspace_id}/dots/{dot_id}/spaces/{space_id}` (admin): grant and revoke
  - `GET/POST /api/workspaces/{workspace_id}/spaces/{space_id}/teams` and `GET /api/workspaces/{workspace_id}/teams/{team_id}`

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/team.rs`:
  - `default_team_has_one_lead_and_five_specialists`
  - `team_rejects_second_lead`: 422 `team_lead_conflict`
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
- Consumes: `Tenant`, `SpaceId`, `TeamId`, `DotId`, `JobId`.
- Produces:
  - `Thread { id: ThreadId, space_id: SpaceId, team_id: TeamId, title: String }`
  - `thread::create(pool, &Tenant, SpaceId, TeamId, title: &str) -> Result<ThreadId, KatError>` and `thread::get(pool, &Tenant, ThreadId) -> Result<Thread, KatError>`; another workspace's thread is `NotFound`
  - `Author::{User(UserId), Dot(DotId), System}` (`Eq`, `Hash`)
  - `Message { id: MessageId, thread_id: ThreadId, author: Author, body: String, synthetic: bool, job_id: Option<JobId>, created_at: DateTime<Utc> }`
  - `NewMessage { id: Option<MessageId>, author: Author, body: String, client_msg_id: Option<String>, idempotency_key: Option<String>, synthetic: bool }`; `id: None` generates a new id
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
- Modify: `Cargo.toml`, `Cargo.lock`, `crates/katwork/src/runtime.rs`, `crates/katwork-server/src/main.rs`

**Interfaces:**
- Consumes: `Config`, `ProviderEnv` (Task 1); `types::{ModelTier, SpacePolicy}` and `Core` (Task 2). Nothing from Tasks 3–6.
- Produces:
  - `#[async_trait] trait ModelProvider: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> Capabilities; async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError>; }`
  - `ChatRequest { system: String, messages: Vec<ChatMsg>, tools: Vec<ToolSpec>, max_output_tokens: u32, temperature: Option<f32> }`. System text lives only in `system`; there is no system role.
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
  - `ModelEntry { provider_id: String, model: String, tier: ModelTier, capabilities: Capabilities, price: Option<TokenPrice> }`. Capabilities and residency come from the entry, never from the provider name or base URL. SP1 configures no prices, so `model_calls.cost_micros` stays NULL; cost is only ever computed from usage the provider reported.
  - `Selected { provider: Arc<dyn ModelProvider>, entry: ModelEntry }`
  - `Registry::from_config(&Config) -> Registry` and `Registry::from_entries(Vec<(ModelEntry, Arc<dyn ModelProvider>)>) -> Registry`
  - `Registry::for_tier(&self, tier: ModelTier, space_policy: &SpacePolicy) -> Result<Selected, ModelError>`
  - `FakeTurn::{Chunks(Vec<ChatChunk>), Error(ModelError), Hang, ChunksThenHang(Vec<ChatChunk>)}`
  - `FakeProvider::scripted(Vec<FakeTurn>) -> FakeProvider` (`Clone`; each `stream` call takes the next turn) and `FakeProvider::received(&self) -> Vec<ChatRequest>`, the requests it got
  - `FakeProvider::demo() -> FakeProvider`: canned English and Thai replies, `Local`, with `capabilities().synthetic == true`
  - `OpenAiApi::{Chat, Responses}`, `RigProvider::openai(env: &ProviderEnv, model: &str, api: OpenAiApi) -> Result<RigProvider, ModelError>` and `RigProvider::anthropic(env: &ProviderEnv, model: &str) -> Result<RigProvider, ModelError>`
  - `Core` gains `registry: Arc<Registry>`, built in `Core::new` with `Registry::from_config`, and `Core::with_registry(self, registry: Registry) -> Core` for tests
- Registry rules:
  - `from_config` registers each configured provider as `External`: OpenAI through `OpenAiApi::Chat` (chosen explicitly, never detected) and Anthropic, with one model id per tier from a constant table in `registry.rs` (initial defaults, checked at Task 7 start).
  - With no provider keys and `demo`, every tier maps to `FakeProvider::demo()`. With no keys and no demo, `for_tier` returns `ProviderUnavailable`.
  - An `External` entry for a Space with `allow_external = false` is `ResidencyDenied`.
- Rig adapter rules (ADR-0001), all inside `rig_adapter.rs`; no Rig type leaves that file:
  - only Rig's completion and streaming clients; no `AgentRunner`, `ToolServer`, hooks or typed-output retries
  - tools advertised as `ToolDefinition` only
  - hosted tools, server-side fallbacks, raw `additional_params` and hidden continuation stay off
  - a reqwest client with a 10 s connect deadline, a 30 s read deadline, caps of 16 MiB per response, 64 KiB per error body and 1 MiB per SSE frame, and no redirects (initial defaults)
  - provider bytes are decoded with an incremental UTF-8 decoder, never `String::from_utf8` per network chunk
  - Anthropic's opening and closing usage snapshots are merged, never summed
  - a provider tool call with no id gets `kw_call_{index}`; a duplicate id within one turn is `Protocol`

- [ ] **Step 1: Pin dependencies.** Check crates.io for the latest rig-core release; pin it exactly (`=x.y.z`) with `default-features = false` and explicit reqwest and rustls features. Pin `reqwest` to the release that rig-core uses. Add `async-trait`, `futures` and `tracing-subscriber`. In `katwork-server`, initialize tracing from `RUST_LOG`, defaulting to `info,rig=info`, because Rig's `rig::completions` TRACE level logs full prompts.
- [ ] **Step 2: Write the failing tests** in `crates/katwork/tests/models.rs`:
  - `registry_refuses_external_provider_for_local_only_space`: returns `ModelError::ResidencyDenied`
  - `demo_mode_uses_synthetic_provider_without_keys`: no API keys plus `KATWORK_DEMO=1` gives `FakeProvider::demo`
  - `no_keys_and_no_demo_is_provider_unavailable`
  - `stream_stall_returns_model_timeout`: `FakeTurn::Hang` gives `Timeout { phase: FirstChunk }` and `ChunksThenHang` gives `Timeout { phase: Idle }`, with the timeouts injected small
  - `stream_without_done_is_protocol_error`
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
- Consumes: `Tenant`, `thread::get`, ids, `types::{SideEffect, ReasonCode}`, `Core`.
- Produces:
  - `enum AgUiEvent`: KatWork's own serde types for AG-UI protocol 1.0, covering only the events KatWork emits (ADR-0003). Serialized with `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]` and camelCase fields. Variants: `RunStarted{threadId,runId}`, `TextMessageStart{messageId,role}`, `TextMessageContent{messageId,delta}`, `TextMessageEnd{messageId}`, `ToolCallStart{toolCallId,toolCallName,parentMessageId}`, `ToolCallArgs{toolCallId,delta}`, `ToolCallEnd{toolCallId}`, `ToolCallResult{messageId,toolCallId,content}`, `RunFinished{threadId,runId}`, `RunError{message,code}`, `Custom{name,value}`.
  - Ids: `threadId` and `runId` are KatWork's. `toolCallId` is the `ToolCallRecordId`, so a provider id reused in a later step stays distinct. `TextMessageStart.messageId` equals the stored reply's `MessageId`. `TOOL_CALL_END` means the arguments are complete; `TOOL_CALL_RESULT` means execution is complete.
  - `KatworkEvent`, serialized as `Custom{name,value}` through `From<KatworkEvent> for AgUiEvent`:
    - `katwork.dot_state {dotId, state}` with `DotState::{Idle, Working, Researching, Waiting, NeedsApproval, Error, Paused}`
    - `katwork.job_status {jobId, status, reason?, holdReason?}`; `status` is null while the job runs
    - `katwork.decision {jobId, inputs, verdict, policyVersion, thresholds}`
    - `katwork.tool_status {toolCallId, ok, code}`
    - `katwork.usage {runId, dotId, modelCallId, input, output, complete}`
  - `DotState::for_tool(SideEffect) -> DotState`: `Read`, `WorkspaceRead` and `Navigate` give `Researching`; every other effect gives `Working`. Only the harness emits `dot_state`: `Working` while the model streams, `NeedsApproval` on an approval stop, `Waiting` on a question, `Error` on a run error, `Paused` for a paused Dot, `Idle` otherwise.
  - `log::append(tx: &mut Transaction<'_, Postgres>, workspace_id: WorkspaceId, thread_id: ThreadId, run_id: Option<RunId>, e: &AgUiEvent) -> Result<EventSeq, KatError>`: runs `UPDATE threads SET last_event_seq = last_event_seq + 1 WHERE id = $1 RETURNING last_event_seq` and then the insert, inside the caller's transaction, so appenders serialize per thread
  - `log::since(pool, &Tenant, ThreadId, after: EventSeq) -> Result<Vec<(EventSeq, AgUiEvent)>, KatError>`
  - `EventHub` (`Clone`): `EventHub::new() -> EventHub`, `subscribe(&self, ThreadId) -> tokio::sync::broadcast::Receiver<EventSeq>` and `notify(&self, ThreadId, EventSeq)`. The broadcast is only a wake-up; the log is the truth.
  - `#[async_trait] trait EventSink: Send + Sync { async fn emit(&self, e: AgUiEvent) -> Result<EventSeq, KatError>; }`
  - `PgEventSink::new(pool: PgPool, hub: EventHub, workspace_id: WorkspaceId, thread_id: ThreadId, run_id: Option<RunId>) -> PgEventSink`: each `emit` commits `log::append` in its own transaction and only then calls `hub.notify`
  - `DeltaCoalescer::new() -> DeltaCoalescer`, `push(&mut self, delta: &str, now: Instant) -> Option<String>` and `finish(&mut self) -> Option<String>`: it buffers text deltas and releases them at ≥100 ms or ≥512 bytes, cut only at a grapheme-cluster boundary (`unicode-segmentation`)
  - `Core` gains `events: EventHub`, built in `Core::new`
  - Route `GET /api/workspaces/{workspace_id}/threads/{thread_id}/events`: SSE with `id:` set to the per-thread `EventSeq`. It subscribes to the hub first, then replays `since(Last-Event-ID or 0)`, then on each wake-up reads `since(last sent)` from the log. It drops any event with `seq ≤` the last one sent, re-reads from the log when the receiver lags, and sends a `:` comment keep-alive every 15 s.

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/events.rs`:
  - `events_serialize_as_agui_json`: `TextMessageContent` → `{"type":"TEXT_MESSAGE_CONTENT","messageId":"…","delta":"…"}`
  - `agui_fixtures_match_protocol_1_0`: every emitted variant round-trips against the protocol JSON in `tests/fixtures/agui/`
  - `concurrent_appenders_never_skip_on_resume`: two tasks append to one thread at once, and a reader resuming at any seq sees every event
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
- Consumes: `Registry`, `Selected`, `ModelProvider`, `ChatRequest`, `ChatMsg`, `ChatChunk`, `ToolSpec`, `ModelError`, `StreamTimeouts`, `guard_stream` (Task 7); `EventSink`, `AgUiEvent`, `KatworkEvent`, `DotState`, `DeltaCoalescer` (Task 9); `Dot`, `Dot::config_hash` (Task 5); `Tenant` (Task 4); ids and `types::{SideEffect, SpacePolicy}` (Task 2).
- Produces:
  - `#[async_trait] trait Tool: Send + Sync { fn spec(&self) -> ToolSpec; fn side_effect(&self) -> SideEffect; async fn call(&self, ctx: &ToolCtx, args: serde_json::Value) -> Result<String, ToolError>; }`
  - `ToolCtx { tenant: Tenant, thread_id: ThreadId, dot_id: DotId, job_id: JobId, task_id: TaskId, run_id: RunId, step: u32, tool_call_id: String, idempotency_key: String, lease_epoch: LeaseEpoch, actor: UserId, initiator: Initiator, computer: Option<ComputerHandle>, pool: PgPool, cancel: CancellationToken }`
    - `idempotency_key` is `"{run_id}:{step}:{tool_call_id}"`
    - `computer` is always `None` in SP1; `ComputerHandle` is an empty placeholder struct
    - tenant authority and the pool never appear in model-visible arguments
  - `Initiator::{Person, Routine, Handoff, System}` (`Serialize`, `Deserialize`)
  - `ToolError::{NotAllowed, NeedsApproval, InvalidArgs(String), Failed(String), Timeout, OutcomeUnknown}`
  - `ToolRegistry::new() -> ToolRegistry`, `register(&mut self, tool: Arc<dyn Tool>)`, `get(&self, id: &str) -> Option<Arc<dyn Tool>>` and `ToolRegistry::builtin() -> ToolRegistry`
  - Built-in tools: `read_thread` (`Read`) and `ask_user { question: String, options: Vec<String> }` (`Read`), which ends the run with `pending_question` set. There is no `post_reply`: the final assistant text is the reply, written once by `job::conclude` (Task 10).
  - `RunKind::{Initial, Repair, Recovery}`
  - `RunInput { tenant: Tenant, run_id: RunId, thread_id: ThreadId, space_id: SpaceId, job_id: JobId, task_id: TaskId, lease_epoch: LeaseEpoch, kind: RunKind, space_policy: SpacePolicy, history: Vec<ChatMsg>, reply_message_id: MessageId, actor: UserId, initiator: Initiator, deadline: tokio::time::Instant, cancel: CancellationToken }`
  - `RunEnv<'a> { pool: &'a PgPool, models: &'a Registry, tools: &'a ToolRegistry, sink: &'a dyn EventSink }`
  - `run::execute(env: &RunEnv<'_>, dot: &Dot, input: RunInput, limits: &Limits) -> RunFacts`. It inserts the `runs` row first and closes it at the end.
  - `RunFacts { run_id: Option<RunId>, reply_message_id: MessageId, final_text: Option<String>, stop: Option<StopReason>, tool_calls: Vec<ToolCallRecord>, artifacts_created: u32, pending_tool_call: Option<PendingCall>, pending_question: Option<PendingQuestion>, synthetic: bool, executor_unavailable: bool, usage: Vec<ModelCallId> }`. The loop returns facts only; the policy (Task 10) turns them into a job status.
  - `RunFacts::not_run(stop: StopReason) -> RunFacts`: no run id, a fresh `reply_message_id`, everything else empty; used when the harness decides without a run (Tasks 11 and 12)
  - `ToolCallRecord { id: ToolCallRecordId, step: u32, tool_call_id: String, name: String, side_effect: SideEffect, state: ToolCallState }` and `ToolCallState::{Started, Succeeded, Failed, TimedOut, OutcomeUnknown, Refused}`
  - `PendingCall { tool_call_id: String, name: String, args_sha256: String }` and `PendingQuestion { question: String, options: Vec<String> }`
  - `StopReason::{ProviderUnavailable, ProviderRateLimited, ProviderQuota, ProviderAuthFailed, ModelProtocol, StepLimit, LoopDetected, NeedsApproval, TokenBudget, OutputTruncated, TimeLimit, PolicyChanged, QuotaExceeded, WorkspacePaused, DotPaused, Interrupted, Cancelled}`
  - `Limits { max_steps: u32, tool_timeout: Duration, result_max_chars: usize, repeat_limit: u32, repeat_window: (u32, u32), max_total_tokens: u64, run_wall_clock: Duration, task_deadline: Duration, job_deadline: Duration, model_retries: u32, stream: StreamTimeouts }`. `Limits::default()` holds the initial defaults: 24, 60 s, 8,000, 3, (5, 10), 200,000, 90 s, 5 min, 10 min, 1, and 30 s / 30 s. The run deadline is separate from the lease TTL.
  - `guards::canonical_args(args: &serde_json::Value) -> String`: object keys sorted and whitespace normalized; array order and values kept
  - `guards::RepeatKey { tool_name: String, canonical_args: String, target_revision: Option<String> }`, where `target_revision` is always `None` in SP1 (SP2 fills it so a render after each edit is not a loop), and `guards::RepeatGuard::new(limit: u32, window: (u32, u32))` with `check(&mut self, key: &RepeatKey) -> bool`, false meaning stop before this call. The provider's call id is never part of the key.
  - `guards::truncate_result(text: &str, max_scalars: usize) -> String`: at most `max_scalars` Unicode scalar values, the marker `"…[truncated]"` included. A JSON result over the cap becomes a plain-string preview prefixed `"[JSON truncated] "`, never cut inside its structure.
  - `ledger::start_tool_call(pool, ctx: &ToolCtx, name: &str, side_effect: SideEffect, args_sha256: &str) -> Result<ToolCallRecordId, KatError>`: inserts a `started` row. The first row whose `side_effect` is not `read` also sets `tasks.effects_started_at`, in the same transaction. A stale `lease_epoch` is `lease_lost`.
  - `ledger::end_tool_call(pool, id: ToolCallRecordId, state: ToolCallState, result: Option<&str>) -> Result<(), KatError>`
  - `ledger::check_epoch(conn: &mut PgConnection, TaskId, LeaseEpoch) -> Result<(), KatError>`: every tool that writes runs it inside its own write transaction, so a stale epoch is rejected
  - `ledger::record_model_call(pool, row: &ModelCallRow) -> Result<ModelCallId, KatError>`, where `ModelCallRow` mirrors the `model_calls` columns
- Loop rules:
  - **Steps.** A step is one model attempt, retries included. An answer on attempt 24 is accepted; a tool batch that would need attempt 25 is not executed (`StepLimit`).
  - **Model attempts.** Each attempt calls `models.for_tier(dot.model_tier, &input.space_policy)`, so a provider change re-checks residency, and wraps the stream in `guard_stream`, raced against `input.deadline` (`TimeLimit`). It retries only before the first chunk, at most `model_retries` times. Every attempt gets one `model_calls` row and one `katwork.usage` event.
  - **Provider errors.** `ProviderUnavailable` or a `Timeout` still failing after retries stops `ProviderUnavailable` and sets `executor_unavailable`. `RateLimited` stops `ProviderRateLimited`; `QuotaExceeded` stops `ProviderQuota`; `AuthFailed` stops `ProviderAuthFailed`; `Protocol` stops `ModelProtocol`.
  - **Token budget.** Input plus output tokens are summed over attempts; once the sum passes `max_total_tokens`, the run stops `TokenBudget` before the next attempt.
  - **Turn validation before dispatch.** Only an `EndTurn` or `ToolUse` turn can dispatch tools. Errored, aborted, filtered or unknown turn endings, malformed arguments and unfinished calls never execute. A `MaxTokens` turn keeps its partial text as `final_text`, stops `OutputTruncated`, and runs none of its tool calls.
  - **Allowlist at call time.** Each call resolves the tool from `dot.tool_allowlist ∩ ToolRegistry`; anything else gets a `ToolError::NotAllowed` error result. Before each tool call the loop re-reads `dots.policy_version`; if it changed, the run stops `PolicyChanged`.
  - **Effect gate before dispatch.** `Read`, `WorkspaceRead`, `Draft` and `WorkspaceWrite` tools run. `Navigate`, `Exec` and `CredentialUse` return `ToolError::NotAllowed`, since SP1 has no computer. `Destructive` and `Outward(_)` return `ToolError::NeedsApproval` before the tool is called: the run stops `NeedsApproval` and records `pending_tool_call` with the provider's `tool_call_id`.
  - **Repeat guard before dispatch.** The run stops `LoopDetected` before executing the 3rd consecutive identical call, or the 5th identical call within the last 10.
  - **Timeouts and cancellation.** Each call's future is owned and raced against `min(tool_timeout, run remaining)` and the run's `CancellationToken`, and dropped on either. Cancellation is checked before dispatch and between sibling calls. No task is ever detached. A call that times out or loses its reply after dispatch is `OutcomeUnknown`. A mutating tool is never retried automatically; a `Read` tool may retry once.
  - **Ledger.** Every dispatched call records `started`, then `succeeded`, `failed`, `timed_out` or `outcome_unknown`; a refused call records `refused`.
  - **Result pairing.** An unknown tool name, a validation failure, a timeout and a refusal each become exactly one `ChatMsg::Tool { is_error: true }` with the original call id and name, and the loop continues unless a stop rule above applies. The text given to the model is `truncate_result(…, result_max_chars)`, and that exact text is stored in `tool_calls.result`.
  - **Prompt hygiene.** The system text is the Dot's persona and instructions plus a capability preamble generated from the effective tool set, never hand-written into `instructions`. Tool results, history and retrieved text never use the system role. Prompts never say "never ask".
  - **Events.** `RUN_STARTED`, `dot_state working`, then `TEXT_MESSAGE_START` with `messageId = reply_message_id`, the text through `DeltaCoalescer` as `TEXT_MESSAGE_CONTENT`, and `TEXT_MESSAGE_END`. Tool-call events follow only once the turn is accepted; `TOOL_CALL_RESULT` and `katwork.tool_status` follow execution. On cancel, the loop emits `TOOL_CALL_RESULT` (content: `cancelled`) and `katwork.tool_status{ok: false, code: "cancelled"}` for every open call, then `RUN_ERROR{code: "cancelled"}`, and returns with `stop = Cancelled`. The loop never emits `RUN_FINISHED`; `job::conclude` does (Task 10).

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/agent.rs` (FakeProvider scripted; test-only tools registered per test):
  - `plain_text_reply_sets_final_text`
  - `tool_call_then_reply`
  - `unknown_tool_id_returns_error_result_and_continues`
  - `registered_tool_outside_dot_allowlist_is_refused`
  - `allowlist_revoked_mid_run_stops_before_next_tool`
  - `destructive_and_outward_tools_refused_before_dispatch`: the tool's `call` never runs; `stop = NeedsApproval`; `pending_tool_call` keeps the call id
  - `navigate_exec_and_credential_tools_are_not_allowed`
  - `no_tool_runs_from_a_max_tokens_turn`: the partial text is kept and `stop = OutputTruncated`
  - `no_tool_runs_from_a_failed_turn`
  - `same_tool_same_args_three_times_stops_loop_detected`: the third call is not executed
  - `alternating_two_calls_five_times_in_ten_stops`
  - `call_id_and_key_order_do_not_defeat_repeat_guard`
  - `answer_on_step_24_accepted_tool_batch_needing_25_stops`
  - `token_budget_200k_stops_before_next_attempt`
  - `run_wall_clock_90s_stops_time_limit`: paused tokio clock
  - `tool_result_truncated_to_8000_scalars_including_marker_thai`: includes a long single-line Thai string
  - `tool_timeout_60s_returns_error_result`: paused tokio clock
  - `timed_out_tool_cannot_write_afterwards`
  - `cancellation_token_stops_with_cancelled`
  - `no_done_after_cancellation`: no `RUN_FINISHED` and no text event after the cancel
  - `ask_user_ends_run_needs_input`: `pending_question` is set and `final_text` is `None`
  - `tool_output_never_enters_system_role`
  - `capability_preamble_lists_effective_tools`
  - `usage_recorded_once_per_attempt`
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
- Consumes: `RunFacts`, `StopReason`, `Initiator` (Task 8); `ChatMsg` (Task 7); `log::append`, `EventHub`, `AgUiEvent`, `KatworkEvent`, `DotState` (Task 9); `message::append`, `NewMessage` (Task 6); `Tenant`; `types::ReasonCode`.
- Produces:
  - `JobStatus::{Accepted, Ready, Answered, Stopped, NeedsInput, Cancelled, Error}`
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
    | `ProviderAuthFailed` / `DotPaused` | `stopped` | `provider_auth_failed` / `dot_paused` | `AskUser` (an admin) |
    | `StepLimit` / `TokenBudget` / `OutputTruncated` | `stopped` | `step_limit` / `token_budget` / `output_truncated` | `SaveDraftWithReport` |
    | `LoopDetected` | `stopped` | `loop_detected` | `HandToLead` |
    | `PolicyChanged` | `stopped` | `policy_changed` | `RestartJob` |
    | `NeedsApproval` | `needs_input` | `approval_pending` | `AskUser` |
    | `Interrupted` (lease reclaimed after a side effect) | `stopped` | `interrupted_review_required` | `Retry` after review |
    | `Cancelled` | `stopped` | `cancelled` | `Retry`; never reached in practice, because cancelled jobs end through the harness |

  - `policy::decision_event(job_id: JobId, facts: &RunFacts, v: &Verdict, t: &Thresholds) -> KatworkEvent`: `katwork.decision`, with a summary of the facts as `inputs`
  - `job::create(conn: &mut PgConnection, &Tenant, ThreadId, MessageId) -> Result<JobId, KatError>`: a running job (`status` NULL) with `requested_by = tenant.user_id`
  - `job::create_follow_up(conn: &mut PgConnection, &Tenant, parent: JobId, MessageId) -> Result<JobId, KatError>`: `parent` must be `needs_input` in the same thread, otherwise `job_not_awaiting_input`; sets `parent_job_id` (ADR-0002)
  - `JobSummary { id: JobId, status: Option<JobStatus> }` and `job::latest_in_thread(conn: &mut PgConnection, &Tenant, ThreadId) -> Result<Option<JobSummary>, KatError>`
  - `job::finish_by_policy(tx: &mut Transaction<'_, Postgres>, job_id: JobId, v: &Verdict, decision_seq: EventSeq) -> Result<EventSeq, KatError>`: sets `status`, `reason`, `decided_by = 'policy'` and `decision_seq`, then appends `katwork.job_status` and returns its seq. The caller has already appended `katwork.decision`.
  - `HarnessEnd::{Cancelled, Error { code: ReasonCode }}` and `job::finish_by_harness(tx: &mut Transaction<'_, Postgres>, job_id: JobId, end: HarnessEnd) -> Result<EventSeq, KatError>`: sets `status`, `reason` and `decided_by = 'harness'`, then appends `katwork.job_status` and returns its seq, inside the caller's transaction (claim and cancel use it); the caller commits and notifies the hub
  - Finishing a finished job is `job_already_finished`.
  - `job::conclude(tx: &mut Transaction<'_, Postgres>, tenant: &Tenant, job_id: JobId, thread_id: ThreadId, dot_id: Option<DotId>, idem_key: &str, facts: &RunFacts) -> Result<EventSeq, KatError>`: the single place that writes a job's outcome. In the caller's transaction it:
    1. runs `policy::decide_job(facts, &Thresholds::SP1)`
    2. appends `katwork.decision`
    3. when `dot_id` is set and there is `final_text` or a `pending_question`, appends the Dot's message with `id = facts.reply_message_id`, `idempotency_key = "reply:{idem_key}"` and `synthetic = facts.synthetic`
    4. for a `stopped` verdict, appends a `System` message whose body is the reason code, with `idempotency_key = "notice:{idem_key}"`
    5. calls `job::finish_by_policy`
    6. when `facts.run_id` is set, appends `katwork.dot_state` (`needs_approval` for an approval stop, `waiting` for a question, `paused` for a workspace pause, `idle` otherwise), then `RUN_FINISHED` unless `facts.stop` is `Cancelled` or `WorkspacePaused` (those runs already ended with `RUN_ERROR`)

    It returns the last seq; the caller commits and notifies the hub.
  - `TaskInput { thread_id: ThreadId, history: Vec<ChatMsg>, history_cut: u32, actor: UserId, initiator: Initiator }` (`Serialize`, `Deserialize`; stored in `tasks.input`)
  - `task::enqueue(conn: &mut PgConnection, job_id: JobId, dot_id: DotId, input: &TaskInput, idempotency_key: &str) -> Result<TaskId, KatError>`: idempotent on the key
  - `job::create`, `job::create_follow_up`, `job::latest_in_thread` and `task::enqueue` take a connection so that intake (Task 12) runs them in one transaction.

- [ ] **Step 1: Write the failing tests:**
  - in `crates/katwork/tests/policy.rs`: `policy_same_facts_same_verdict`, `answered_only_without_artifacts_else_ready`, `executor_unavailable_is_stopped_provider_unavailable`, `needs_approval_is_needs_input_approval_pending`, `every_stop_reason_maps_to_its_reason_code` (table-driven over the table above), `policy_never_returns_cancelled_or_error`
  - in `crates/katwork/tests/jobs.rs`: `finish_by_policy_records_reason_and_decided_by`, `finish_by_harness_sets_cancelled_and_error`, `cannot_finish_twice`, `decision_event_precedes_job_status`, `conclude_twice_writes_one_reply`, `follow_up_job_links_parent_needs_input_job`, `enqueue_is_idempotent_on_key`
- [ ] **Step 2: Run** `cargo test -p katwork --test policy --test jobs`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: jobs, policy function, status transitions`.

### Task 11: Leases, worker pool and restart recovery

**Files:**
- Create: `crates/katwork/src/jobs/{lease,worker}.rs`, `crates/katwork/tests/leases.rs`

**Interfaces:**
- Consumes: `task::enqueue`, `TaskInput`, `job::{conclude, finish_by_harness}` (Task 10); `run::execute`, `RunEnv`, `RunInput`, `RunFacts`, `RunFacts::not_run`, `RunKind`, `Limits`, `StopReason`, `ToolRegistry::builtin` (Task 8); `PgEventSink`, `log::append` (Task 9); `dot::get` (Task 5); `thread::get` (Task 6); `space::get`, `tenancy::resolve` (Task 4); `Core`.
- Produces:
  - `lease::claim(core: &Core, worker_id: &str, ttl: Duration, limits: &Limits) -> Result<Option<ClaimedTask>, KatError>`. Candidates are tasks with `state = 'queued'`, or `state = 'leased'` with an expired lease, **whose job is still running** (`jobs.status IS NULL`), selected with `FOR UPDATE OF tasks, jobs SKIP LOCKED`; every time comes from `now()`. Each candidate is handled in its own transaction. A branch that ends without a run commits and claim moves on to the next candidate (at most 16 per call); the first candidate that gets a lease is returned. For each candidate, in order:
    - on an expired lease, first emits `TOOL_CALL_RESULT` and `katwork.tool_status{ok: false}` for each of the previous run's open calls (started in `tool_calls` with no end), marks those calls `outcome_unknown` and the run `interrupted`, and emits `RUN_ERROR{code: "lease_lost"}`, before any later `RUN_STARTED`
    - **deadline:** a task past `created_at + task_deadline`, or whose job is past `created_at + job_deadline`, becomes `failed`, its lease row is deleted, and the job ends with `job::conclude` on `RunFacts::not_run(StopReason::TimeLimit)`
    - **retries exhausted:** at `attempts >= max_attempts` (3), the task becomes `failed`, its lease row is deleted, and the job ends with `finish_by_harness(tx, Error { code: RetriesExhausted })`
    - **effects started:** an expired lease with `effects_started_at` set makes the task `interrupted`, deletes its lease row, and ends the job with `job::conclude` on `RunFacts::not_run(StopReason::Interrupted)`
    - skips a task of a paused workspace and sets `hold_reason = 'workspace_paused'`
    - skips a task whose Dot already runs `max_concurrency` tasks and sets `hold_reason = 'dot_busy'`
    - emits `katwork.job_status {status: null, holdReason}` when it first sets a hold reason
    - otherwise claims it: attempts + 1 (a `Recovery` run when the lease had expired), increments `tasks.lease_epoch`, writes the lease with the same epoch, and clears `hold_reason`

    Because each no-run branch moves the task to a terminal state and finishes its job in the same transaction, a repeated poll never finishes the same job twice.
  - `ClaimedTask { task_id: TaskId, job_id: JobId, dot_id: DotId, workspace_id: WorkspaceId, thread_id: ThreadId, requested_by: UserId, input: TaskInput, attempts: i32, lease_epoch: LeaseEpoch, kind: RunKind, deadline_at: DateTime<Utc> }`, where `deadline_at = min(task created_at + task_deadline, job created_at + job_deadline)`, computed in SQL
  - `HeartbeatStatus::{Continue, Stop(StopReason)}` and `lease::heartbeat(pool, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch, ttl: Duration) -> Result<HeartbeatStatus, KatError>`: `Stop(WorkspacePaused)` once the workspace is paused. A task that is no longer leased under this epoch (reclaimed, cancelled or finished by the harness) is `lease_lost`.
  - `lease::complete(core: &Core, task_id: TaskId, worker_id: &str, epoch: LeaseEpoch, facts: RunFacts) -> Result<(), KatError>`: a task that is no longer leased under this epoch is `lease_lost`, and the worker drops its facts, because whoever ended the lease (reclaim or cancel) already closed the run. Otherwise, in one transaction: `job::conclude` (with `idem_key` = the task id); the task state (`done`, or `interrupted` for `WorkspacePaused`) and `pending_tool_call`; the lease release. Then it commits and notifies the hub.
  - `worker::spawn_pool(core: Arc<Core>, n: usize, ttl: Duration) -> JoinSet<()>`. Each worker:
    - polls `claim` (every 1 s when idle)
    - builds the `Tenant` with `tenancy::resolve(&core.pool, requested_by, workspace_id)`, loads the Dot and the thread's Space policy, and creates a `PgEventSink` for a new `RunId`
    - runs `run::execute` with `Limits::default()`, `ToolRegistry::builtin()`, `core.registry`, `reply_message_id = MessageId::new()` and `deadline = min(now + run_wall_clock, deadline_at)`
    - heartbeats every `ttl / 3`; on `Stop(reason)` it cancels the run and sets `facts.stop = Some(reason)`; on `lease_lost` it cancels the run and drops its facts
    - finishes with `lease::complete`
  - TTL defaults to 60 s, with a heartbeat every 20 s.

- [ ] **Step 1: Write the failing tests** in `crates/katwork/tests/leases.rs`:
  - `two_workers_never_claim_same_task`: 50 tasks, 2 workers in parallel, each task claimed once
  - `expired_lease_is_reclaimed_once`: worker A claims and stops heartbeating; after the TTL, worker B claims it with a higher epoch; A's late `heartbeat` and `complete` are rejected with `lease_lost`
  - `expired_lease_after_side_effect_is_interrupted_not_rerun`
  - `reclaim_does_not_duplicate_reply`
  - `reclaimed_run_closes_open_calls_before_new_run_started`
  - `retries_exhausted_finishes_job_error`
  - `cancelled_job_task_is_never_claimed`: a job cancelled while its task is queued is never claimed, and no model call happens
  - `deadline_branch_finishes_once_across_repeated_polls`: the task ends `failed` with no lease row, and one job status is written however often workers poll
  - `retries_exhausted_branch_finishes_once_across_repeated_polls`
  - `one_dot_runs_one_task_at_a_time`
  - `paused_workspace_claims_nothing`
  - `pausing_workspace_interrupts_running_task`: the task ends `interrupted` and the job `stopped/workspace_paused`
  - `task_past_deadline_finishes_time_limit`
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
  - `pipeline::intake(tx: &mut Transaction<'_, Postgres>, core: &Core, tenant: &Tenant, thread_id: ThreadId, post: UserPost) -> Result<Submitted, KatError>`: the whole intake inside the caller's transaction, in this order:
    1. `message::append` for the user message; when it returns `created: false`, intake returns the stored `{message_id, job_id}` at once. Because a message, its job and its task commit together, a stored message always has its `job_id`.
    2. `job::latest_in_thread`; if that job is `needs_input`, `job::create_follow_up`, otherwise `job::create` (ADR-0002). The unique `jobs.request_message_id` rejects any second job for the message.
    3. routing, quotas and notices (below)
    4. `task::enqueue` and `message::set_job`, or `job::conclude` for a job that ends at intake
  - `pipeline::submit(core: &Core, tenant: &Tenant, thread_id: ThreadId, post: UserPost) -> Result<Submitted, KatError>`: begins a transaction, runs `intake`, commits, then notifies the hub. A crash at any point before the commit leaves no message, job or task, so the client's retry with the same `client_msg_id` starts clean; a concurrent retry waits on the message's unique index and returns the first post's ids. The route answers 201 when `created`, otherwise 200.
  - Intake rules inside `intake`:
    - SP1 routing (spec §6.2 Stage 0 subset; Laya arrives in SP4): the closed set is the thread team's Dots that are `active` and granted to the thread's Space. An `@mention` of a Dot in the set goes to that Dot; otherwise to the team's Lead.
    - a mentioned Dot that is paused leaves the set: the pipeline posts a `System` notice `dot_paused` and routes to the Lead. If no eligible Dot remains, the job ends with `job::conclude` on `RunFacts::not_run(StopReason::DotPaused)`.
    - quotas before enqueue: `quota::check`; when exceeded, the job ends with `job::conclude` on `RunFacts::not_run(StopReason::QuotaExceeded)`, which posts a localized notice. Nothing is dropped silently.
    - in a paused workspace the task is still enqueued and waits (Task 11), and the pipeline posts a `System` notice `workspace_paused`
    - every `job::conclude` call here uses the job id as `idem_key`, and each notice uses the idempotency key `"notice:{job_id}:{code}"`, so a retried post never doubles them
    - builds `TaskInput` with `build_history`, calls `task::enqueue` with key `"job:{job_id}:dot:{dot_id}"`, then `message::set_job`
  - `quota::check(conn: &mut PgConnection, dot: &Dot, sender: UserId) -> Result<Option<StopReason>, KatError>`: the Dot's rolling 24-hour token total from `model_calls` against `daily_token_budget` (NULL means no cap), and at most 6 jobs per minute per (Dot, sender), both counted in Postgres
  - `History { messages: Vec<ChatMsg>, cut: u32 }` and `pipeline::build_history(messages: &[Message], dot_id: DotId, names: &HashMap<Author, String>, budget_chars: usize) -> History`:
    - the newest messages up to `budget_chars` (24,000); the latest user message is always kept whole, and the number of cut messages is recorded
    - the Dot's own messages map to `Assistant`; other Dots and people map to `User` with their server-side `name`
    - system notes become plain `User` notes, never the system role
  - `job::cancel(pool, hub: &EventHub, tenant: &Tenant, job_id: JobId) -> Result<(), KatError>`: another workspace's job is `NotFound`; a finished job is `job_already_finished`. In one transaction it locks the job row, then its task rows (the same job → task order as `claim`). Each queued or leased task becomes `cancelled` and its lease row is deleted. For a running run it closes the open calls (`TOOL_CALL_RESULT`, `katwork.tool_status{ok: false}`, ledger state `outcome_unknown`), marks the run `cancelled`, and emits `RUN_ERROR{code: "cancelled"}` and `katwork.dot_state idle`. It then calls `finish_by_harness(tx, Cancelled)`, commits and notifies the hub. The old worker's next heartbeat or write finds the task no longer leased and gets `lease_lost`.
  - Route `POST /api/workspaces/{workspace_id}/jobs/{job_id}/cancel` returns 202, 404 across tenants, and 409 `job_already_finished`.
  - `serve` builds `Core` and runs `worker::spawn_pool(core, 4, Duration::from_secs(60))`.
  - Event order for an answered job: `RUN_STARTED`, `katwork.dot_state working`, `TEXT_MESSAGE_*`, `katwork.decision`, `katwork.job_status answered`, `katwork.dot_state idle`, `RUN_FINISHED`. `katwork.job_status` is the only success signal; cancelled, needs-input and interrupted outcomes are explicit job statuses, never an omitted one.

- [ ] **Step 1: Write the failing tests** in `crates/katwork-server/tests/pipeline.rs` (FakeProvider):
  - `message_without_mention_goes_to_lead_and_job_answered`
  - `mention_routes_to_named_dot_including_thai_name`
  - `paused_dot_leaves_routing_set`
  - `provider_unavailable_ends_job_stopped_with_reason`: Review Focus 5; the notice code is `provider_unavailable` and the job finishes within the test timeout
  - `provider_quota_and_rate_limit_end_stopped_with_named_reason`
  - `demo_mode_reply_is_flagged_synthetic`: the reply from the messages API has `synthetic: true`
  - `full_event_sequence`: the order above
  - `stream_message_id_equals_stored_reply_id`
  - `restart_mid_run_posts_exactly_one_reply`
  - `retry_with_same_client_msg_id_creates_one_job`
  - `concurrent_retries_with_same_client_msg_id_create_one_job`: two posts with the same `client_msg_id` race; one message, one job and one task exist, and both responses carry the same ids
  - `intake_rolled_back_leaves_no_message_job_or_task`: runs `pipeline::intake` to its last step and drops the transaction without committing (a crash between any two steps rolls back the same way); no message, job or task remains, and a retry then creates exactly one of each
  - `retry_after_intake_stop_returns_same_job`: a post whose job ends at intake (quota) is retried; the same job comes back and no second job or notice appears
  - `reply_to_needs_input_job_starts_linked_follow_up`
  - `history_budget_keeps_latest_user_message_whole`
  - `other_dots_messages_are_user_role_with_name`
  - `seventh_message_in_a_minute_is_stopped_quota_exceeded`
  - `cancel_ends_job_cancelled`
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
  - `sse.ts`: `subscribe(url, onEvent): () => void`, using the browser `EventSource` (no Rust or SDK client) and its native `Last-Event-ID` resume. It keeps the last seq and drops duplicates after a reconnect, and folds text by `messageId` and tool arguments by `toolCallId`, never into "the last message".
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
- Create: `scripts/smoke.sh`, `scripts/compose-backup.sh`, `Caddyfile`, `crates/katwork-server/tests/e2e_compose.rs` (`#[ignore]`, run in a CI job)
- Modify: `docker-compose.yml`, `.env.example` (`KATWORK_HTTPS_HOST`), `.github/workflows/ci.yml` (`e2e` and `upgrade_from_previous_release` jobs), `README.md` (a "Try it" section, HTTPS, backups and upgrades)

**Interfaces:**
- Consumes: everything above.
- Compose start order. The runtime image is distroless with no shell, so each step is its own service:
  1. `backup`: the `postgres:17` image on the `backend` network, run once. `scripts/compose-backup.sh` runs `pg_dump --format=custom` into the `./backups` bind mount as `/backups/katwork-<UTC timestamp>.dump`, but only when the database already has KatWork's schema (`_sqlx_migrations` exists); on a fresh database it does nothing.
  2. `migrate`: the KatWork image with `command: ["migrate"]`, after `backup` completes successfully.
  3. `katwork`: `serve`, after `migrate` completes successfully.
- An optional `https` profile adds a `caddy:2` service that publishes ports 80 and 443 and proxies `KATWORK_HTTPS_HOST` to `katwork:3000`. With it, `KATWORK_PUBLIC_URL=https://<host>`, so the session cookie is `Secure`.

- [ ] **Step 1: Write `scripts/smoke.sh`.** It must:
  1. Run `docker compose up -d --build` with `KATWORK_DEMO=1` and `KATWORK_DEMO_PASSWORD` set.
  2. Wait for `/healthz`.
  3. Run `seed-demo`.
  4. Log in as demo and post `"สวัสดี ช่วยแนะนำทีมหน่อย"` with `Origin: $KATWORK_PUBLIC_URL` and a `client_msg_id`.
  5. Read SSE until `katwork.job_status`.
  6. Assert status `answered`, and that the Dot reply from the messages API has `synthetic: true`.
  7. In a second scenario, run `docker compose restart katwork` mid-run. After recovery, assert exactly one job status **and** exactly one Dot reply message, or `stopped/interrupted_review_required`; the task never ran twice at once.
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
