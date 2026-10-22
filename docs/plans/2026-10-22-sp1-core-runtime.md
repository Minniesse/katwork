# Sub-project 1 — Core Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repo, tasks run as Orca workers (Codex implements, Claude reviews; Task 13 is Claude-implemented, Codex reviews), per spec §19.1.

**Goal:** A self-hosted KatWork server you can start with `docker compose up`. A person logs in (with password reset and demo accounts), sees a workspace with a default AI team, sends a chat message to a Dot, and watches a streamed reply. That reply is produced by a leased, restart-safe job running the Dot's agent loop through a model gateway.

**Architecture:**
- One Rust library crate, `katwork`, holds the domain, persistence, auth, model gateway, agent loop and scheduler as focused modules.
- A binary crate, `katwork-server`, exposes them over Axum: a JSON API, AG-UI events over SSE, and the built React/Vite web app as static files.
- State lives in PostgreSQL. The jobs, tasks and leases tables use `FOR UPDATE SKIP LOCKED`.
- Every model call goes through a `ModelProvider` trait. Production uses a Rig adapter; tests and keyless demo mode use `FakeProvider`.

**Tech Stack:**
- Rust (edition 2024): tokio 1, axum 0.8, sqlx 0.8 (postgres, runtime-tokio, tls-rustls, uuid, chrono, json, migrate), serde/serde_json, uuid 1 (v7), argon2 0.5, lettre 0.11, reqwest 0.12, rig-core (latest release, pinned), tracing, thiserror 2.
- Web: React 19, Vite, TypeScript, Vitest.
- Infrastructure: PostgreSQL 17, Qdrant (container only in SP1), Docker Compose, GitHub Actions.

**Spec:** `docs/spec/katwork-design.md` (§2 decisions, §4 domain model and statuses, §5 architecture, §6 flow, §15 testing, §19 sub-project 1)

## Global Constraints

- All KatWork code is Rust; the UI is React + Vite in TypeScript, served by the Rust server (spec §2).
- Licence Apache-2.0. **No GPL/AGPL dependencies**: `cargo deny check licenses` runs in CI with those licences denied.
- Self-host via **Docker Compose** on one Linux host (spec §1.1 goal 1).
- **English and Thai together.** Every user-facing string comes from `web/src/i18n/{en,th}.json`. API error codes are stable identifiers, never prose.
- **Model output is never executed as shell. Tool ids are matched against a registry** (spec §5.1, §13).
- Every model call goes through `ModelProvider`; provider and Space data-residency rules are enforced in the gateway (spec §11).
- The job status set is exactly `accepted | ready | answered | stopped | needs_input | cancelled | error` (spec §4). In SP1 a job can end as `answered`, `stopped`, `needs_input`, `cancelled` or `error`.
- Agent loop limits, initial defaults:
  - tool timeout 60 s
  - tool results truncated to 8,000 characters
  - at most 24 steps per run
  - loop detection: the same tool with the same arguments 3 times in a row stops the run
- Secrets come only from environment variables; `.env.example` lists them and never contains values.
- **Demo mode with no keys must work.** Replies are visibly labelled `SYNTHETIC` (spec §15).
- Default team roles: Lead, Researcher, Writer, Analyst, Scheduler, Reviewer (spec §2).

## Review Focus

1. **Cross-tenant access.** A user of workspace A requests workspace B's Dots, threads or event stream by id. Expected: 404, never data. (Task 4, Task 9.)
2. **Lease double-run after a crash.** Two workers poll at the same time, and a worker dies mid-run. Expected: a task never runs twice concurrently, and an expired lease is re-queued exactly once. (Task 11.)
3. **SSE reconnect with `Last-Event-ID`.** The browser drops and reconnects mid-reply, including a Thai reply. Expected: no duplicate and no missing events, and multibyte Thai characters are never split across deltas. (Task 9.)
4. **Password reset misuse.**
   - A reset requested for an unknown email gets the same response as a known one.
   - A token used twice, or after 30 minutes, fails.
   - A reset ends all of that user's existing sessions.

   (Task 3.)
5. **Provider down or no keys.** The model provider errors, times out or isn't configured. Expected: the job ends `stopped` with reason `provider_unavailable` within the timeout, the thread shows a localized error, nothing hangs. In demo mode the reply is labelled `SYNTHETIC`. (Task 7, Task 12.)

---

## File Structure

```
Cargo.toml                      workspace: crates/katwork, crates/katwork-server
rust-toolchain.toml             channel = "stable"; workspace rust-version = "1.85"
deny.toml                       licence + advisory policy (GPL/AGPL denied)
.env.example                    DATABASE_URL, KATWORK_SECRET, SMTP_*, OPENAI_*, ANTHROPIC_*, KATWORK_DEMO
docker-compose.yml              postgres:17, qdrant, katwork (built image)
Dockerfile                      multi-stage: web build (node 20) → cargo build → distroless runtime
.github/workflows/ci.yml        fmt, clippy -D warnings, test (postgres service), deny, web test+build
crates/katwork/
  migrations/0001_init.sql      all SP1 tables (Task 2)
  src/lib.rs
  src/ids.rs                    typed UUIDv7 ids
  src/db.rs                     pool + migrate
  src/error.rs                  KatError + stable codes
  src/auth/{mod,password,session,reset,mailer}.rs
  src/tenancy/{mod,workspace,space}.rs
  src/team/{mod,dot,team,roles}.rs
  src/chat/{mod,thread,message}.rs
  src/events/{mod,agui,log}.rs
  src/models/{mod,provider,registry,fake,rig_adapter}.rs
  src/agent/{mod,tool,registry,run,guards}.rs
  src/jobs/{mod,job,task,lease,worker}.rs
  src/pipeline.rs               message → job → task → run (SP1 routing)
crates/katwork-server/
  src/main.rs                   `serve` | `migrate` | `seed-demo`
  src/config.rs  src/app.rs     router + state
  src/routes/{health,auth,workspaces,dots,teams,threads,events}.rs
  src/static_files.rs
  tests/*.rs                    HTTP-level tests (axum-test style via tower::ServiceExt)
web/
  package.json vite.config.ts tsconfig.json
  src/main.tsx src/app.tsx src/api.ts src/sse.ts
  src/i18n/{index.ts,en.json,th.json}
  src/pages/{Login,ResetRequest,ResetConfirm,Team,Chat}.tsx
  src/**/*.test.tsx
```

---

### Task 1: Workspace skeleton, CI and Compose

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `deny.toml`, `.env.example`, `crates/katwork/{Cargo.toml,src/lib.rs,src/error.rs}`, `crates/katwork-server/{Cargo.toml,src/main.rs,src/config.rs,src/app.rs,src/routes/health.rs}`, `crates/katwork-server/tests/health.rs`, `.github/workflows/ci.yml`, `docker-compose.yml`, `Dockerfile`

**Interfaces:**
- Produces:
  - `katwork_server::app::build_router(state: AppState) -> axum::Router`
  - `AppState { pool: sqlx::PgPool, config: Arc<Config> }`
  - `Config::from_env() -> Result<Config, KatError>`
  - `katwork::error::KatError` (thiserror) with a `code(&self) -> &'static str`

- [ ] **Step 1: Write the failing test** `health_returns_ok_and_version` in `crates/katwork-server/tests/health.rs`: `GET /healthz` returns 200 with JSON `{"status":"ok","version": env!("CARGO_PKG_VERSION")}`.
- [ ] **Step 2: Run** `cargo test -p katwork-server --test health`. Expected: FAIL (route missing).
- [ ] **Step 3: Implement** the workspace, `Config::from_env` (requires `DATABASE_URL` and `KATWORK_SECRET` of ≥ 32 bytes; `KATWORK_DEMO=1` enables demo mode), `build_router` with `/healthz`, and `main` subcommands `serve | migrate | seed-demo` (the last two stubbed, returning Ok).
- [ ] **Step 4: Add CI** in `.github/workflows/ci.yml` with these jobs:
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test` (with a `postgres:17` service)
  - `cargo deny check licenses advisories` (`deny.toml` denies GPL-2.0, GPL-3.0, AGPL-3.0)
  - web: `npm ci && npm test && npm run build`, skipped until `web/` exists
- [ ] **Step 5: Add `docker-compose.yml`** (services `postgres:17` with a volume, `qdrant/qdrant` with a volume, and `katwork` built from `Dockerfile` with `depends_on` postgres healthy) and a multi-stage `Dockerfile`.
- [ ] **Step 6: Run** `cargo test && docker compose config -q`. Expected: tests pass, compose valid.
- [ ] **Step 7: Commit** with `chore: workspace skeleton, CI, compose`.

### Task 2: Schema and typed ids

**Files:**
- Create: `crates/katwork/migrations/0001_init.sql`, `crates/katwork/src/{ids.rs,db.rs}`, `crates/katwork/tests/schema.rs`

**Interfaces:**
- Produces:
  - ids: `WorkspaceId, SpaceId, UserId, DotId, TeamId, ThreadId, MessageId, JobId, TaskId, RunId, EventSeq(i64)`, all UUIDv7 newtypes with `sqlx::Type`, `Serialize`, `Deserialize` and `Display`
  - `db::connect(url: &str) -> Result<PgPool, KatError>`
  - `db::migrate(pool: &PgPool) -> Result<(), KatError>`
- Tables:
  - `workspaces`, `users`, `memberships(workspace_id, user_id, role)`, `sessions`, `password_resets`
  - `spaces`, `dots`, `teams`, `team_members(team_id, dot_id, is_lead)`
  - `threads`, `messages`
  - `jobs(status, stop_reason)`, `tasks(state, attempts, max_attempts=3, idempotency_key UNIQUE)`, `task_leases(task_id PK, worker_id, expires_at)`, `runs`
  - `events(seq BIGSERIAL, workspace_id, thread_id, run_id NULL, kind, payload JSONB, created_at)`
- Every tenant-owned row carries `workspace_id NOT NULL` with an FK.
- `jobs.status` is nullable, where NULL means running, and has `CHECK (status IS NULL OR status IN ('accepted','ready','answered','stopped','needs_input','cancelled','error'))`.
- `tasks.state` has `CHECK (state IN ('queued','leased','done','failed'))`.

- [ ] **Step 1: Write the failing tests** with `#[sqlx::test]`:
  - `migrations_apply_cleanly`
  - `job_status_rejects_unknown_value`: an insert with status `'done'` errors
  - `thai_text_round_trips`: insert and read back the Dot name `"นักวิจัย"` byte-equal
- [ ] **Step 2: Run** `cargo test -p katwork --test schema`. Expected: FAIL.
- [ ] **Step 3: Implement** the migration, the ids and `db.rs`. Wire `katwork-server migrate`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: initial schema and typed ids`.

### Task 3: Password auth, sessions, reset, demo accounts

**Files:**
- Create: `crates/katwork/src/auth/{mod,password,session,reset,mailer}.rs`, `crates/katwork-server/src/routes/auth.rs`, `crates/katwork-server/tests/auth.rs`

**Interfaces:**
- Consumes: Task 2 tables and ids.
- Produces:
  - `password::hash(&str) -> Result<String, KatError>` and `password::verify(&str, &str) -> bool` (argon2id)
  - `session::create(pool, UserId) -> Result<SessionToken, KatError>` and `session::authenticate(pool, &str) -> Result<UserId, KatError>`, where `SessionToken(String)` is the raw token returned once and only its SHA-256 is stored
  - `reset::request(pool, &dyn Mailer, email: &str) -> Result<(), KatError>` and `reset::confirm(pool, token: &str, new_password: &str) -> Result<(), KatError>`
  - `trait Mailer { async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), KatError> }`, with `ConsoleMailer` (logs) and `SmtpMailer` (lettre, from `SMTP_*`)
  - Axum extractor `AuthUser(UserId)`
- Routes: `POST /api/auth/login`, `POST /api/auth/logout`, `POST /api/auth/reset/request`, `POST /api/auth/reset/confirm`, `GET /api/auth/me`.
- Session cookie `kw_session` is `HttpOnly; Secure; SameSite=Lax`, expiring after 14 days. Session tokens are stored hashed (SHA-256).

- [ ] **Step 1: Write the failing tests:**
  - `login_with_correct_password_sets_cookie`
  - `login_with_wrong_password_is_401_without_detail`
  - `reset_request_same_response_for_unknown_email`: both return 202 with an identical body
  - `reset_token_single_use`: the second confirm returns 400 `reset_token_invalid`
  - `reset_token_expires_after_30_minutes`: the clock is injected
  - `reset_revokes_existing_sessions`
  - `seed_demo_creates_demo_user_and_workspace`: `seed-demo` is idempotent and creates `demo@katwork.local` with a printed random password
- [ ] **Step 2: Run** `cargo test -p katwork-server --test auth`. Expected: FAIL.
- [ ] **Step 3: Implement** the modules and routes. Reset tokens are 32 random bytes in URL-safe base64, stored hashed, with `expires_at = now + 30 min` and `used_at`. Login is rate-limited to 10 attempts per 15 minutes per email+IP in memory.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: password auth, sessions, reset, demo seed`.

### Task 4: Workspaces, Spaces and tenant scoping

**Files:**
- Create: `crates/katwork/src/tenancy/{mod,workspace,space}.rs`, `crates/katwork-server/src/routes/workspaces.rs`, `crates/katwork-server/tests/tenancy.rs`

**Interfaces:**
- Consumes: `AuthUser`.
- Produces:
  - `Tenant { workspace_id: WorkspaceId, user_id: UserId, role: Role }`, an extractor that reads `:workspace_id` from the path and checks membership, otherwise **404**
  - `Role::{Owner, Admin, Member}`
  - `workspace::create(pool, name, owner) -> Result<WorkspaceId, KatError>`
  - `space::create(pool, &Tenant, name) -> Result<SpaceId, KatError>`
  - `space::list(pool, &Tenant) -> Result<Vec<Space>, KatError>`
  - `Space { id: SpaceId, name: String, policy: SpacePolicy }`
  - `SpacePolicy { allow_external: bool }`, default `true`; stored as a `spaces.allow_external` column
- Routes: `GET/POST /api/workspaces`, `GET/POST /api/workspaces/:workspace_id/spaces`.

- [ ] **Step 1: Write the failing tests:**
  - `member_lists_own_spaces`
  - `non_member_gets_404_not_403`: Review Focus 1
  - `member_cannot_create_space_admin_can`
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** Every repository function takes `&Tenant` and filters by `workspace_id` in SQL.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: workspaces, spaces, tenant scoping`.

### Task 5: Dots, Teams and the default roster

**Files:**
- Create: `crates/katwork/src/team/{mod,dot,team,roles}.rs`, `crates/katwork-server/src/routes/{dots,teams}.rs`, `crates/katwork-server/tests/team.rs`

**Interfaces:**
- Consumes: `Tenant`, `SpaceId`.
- Produces:
  - `Dot { id, space_id, name, role: DotRole, persona: String, instructions: String, tool_allowlist: Vec<String>, model_tier: ModelTier, autonomy: Autonomy }`
  - `DotRole::{Lead, Researcher, Writer, Analyst, Scheduler, Reviewer, Custom(String)}`
  - `ModelTier::{Fast, Standard, Strong}`, `Autonomy::{AskFirst, Standard}`, `Locale::{En, Th}`
  - `TeamView { team: Team, members: Vec<Dot> }`, with the lead first
  - `roles::default_roster(locale: Locale) -> Vec<NewDot>`: 6 Dots with English and Thai display names, for example Lead/หัวหน้าทีม, Researcher/นักวิจัย, Writer/นักเขียน, Analyst/นักวิเคราะห์, Scheduler/เลขา, Reviewer/ผู้ตรวจ
  - `team::create_default(pool, &Tenant, SpaceId, Locale) -> Result<TeamId, KatError>`
  - `team::get(pool, &Tenant, TeamId) -> Result<TeamView, KatError>`
- Routes: CRUD under `/api/workspaces/:w/spaces/:s/dots` and `/teams`. "Hire" is a `POST` to `dots` with role, name and persona.

- [ ] **Step 1: Write the failing tests:**
  - `default_team_has_one_lead_and_five_specialists`
  - `team_rejects_second_lead`: 422 `team_lead_conflict`
  - `hire_custom_dot_with_thai_name`
  - `team_persisted_server_side_and_returned_in_order`: the lead comes first
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** `seed-demo` also creates the default team in the demo Space.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: dots, teams, default roster`.

### Task 6: Threads and messages (web channel)

**Files:**
- Create: `crates/katwork/src/chat/{mod,thread,message}.rs`, `crates/katwork-server/src/routes/threads.rs`, `crates/katwork-server/tests/chat.rs`

**Interfaces:**
- Consumes: `Tenant`, `DotId`, `TeamId`.
- Produces:
  - `Thread { id, space_id, team_id, title }`
  - `Message { id, thread_id, author: Author, body: String, created_at }`
  - `Author::{User(UserId), Dot(DotId), System}`
  - `thread::create`, `message::append(pool, &Tenant, ThreadId, Author, body) -> Result<MessageId, KatError>`
  - `message::list(pool, &Tenant, ThreadId, before: Option<MessageId>, limit: u16)`
  - `mentions::parse(body: &str) -> Vec<String>`: `@name` tokens, Thai-safe
- Routes:
  - `POST /api/workspaces/:w/threads`
  - `GET /api/workspaces/:w/threads/:t/messages`
  - `POST /api/workspaces/:w/threads/:t/messages`, which returns `{message_id, job_id}` (`job_id` is filled in by Task 12)

- [ ] **Step 1: Write the failing tests:**
  - `post_and_list_messages_in_order`
  - `mentions_parse_english_and_thai`: `"@Writer ช่วยสรุป"` → `["Writer"]`; `"@นักวิเคราะห์ ดูไฟล์"` → `["นักวิเคราะห์"]`
  - `message_body_limit_32kb`: a larger body returns 413 `message_too_large`
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: threads and messages`.

### Task 7: Model gateway, FakeProvider and Rig adapter

**Files:**
- Create: `crates/katwork/src/models/{mod,provider,registry,fake,rig_adapter}.rs`, `crates/katwork/tests/models.rs`

**Interfaces:**
- Produces:
  - `#[async_trait] trait ModelProvider: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> Capabilities; async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError>; }`
  - `ChatRequest { system: String, messages: Vec<ChatMsg>, tools: Vec<ToolSpec>, max_output_tokens: u32 }`
  - `ChatMsg { role: ChatRole, content: String, tool_call_id: Option<String> }`, with `ChatRole::{System, User, Assistant, Tool}`
  - `ToolSpec { name: String, description: String, parameters: serde_json::Value }`, where `parameters` is JSON Schema
  - `ModelError::{ProviderUnavailable, ResidencyDenied, Timeout, Other(String)}`
  - `ChatChunk::{Text(String), ToolCall { id: String, name: String, args_json: String }, Usage { input: u32, output: u32 }, Done}`
  - `Capabilities { tools: bool, residency: Residency }` where `Residency::{Local, External}`
  - `Registry::from_config(&Config) -> Registry`
  - `Registry::for_tier(&self, tier: ModelTier, space_policy: &SpacePolicy) -> Result<Arc<dyn ModelProvider>, ModelError>`
  - `SpacePolicy { allow_external: bool }`
  - `FakeProvider::scripted(Vec<Vec<ChatChunk>>)` for tests
  - `FakeProvider::demo()`, whose replies are prefixed `[SYNTHETIC]`
  - `RigProvider` wrapping rig-core's OpenAI-compatible and Anthropic clients

- [ ] **Step 1: Write the failing tests:**
  - `registry_refuses_external_provider_for_local_only_space`: returns `ModelError::ResidencyDenied`
  - `demo_mode_uses_synthetic_provider_without_keys`: no API keys plus `KATWORK_DEMO=1` gives `FakeProvider::demo`
  - `no_keys_and_no_demo_is_provider_unavailable`
  - `stream_timeout_maps_to_provider_unavailable`: a scripted provider that never yields hits a 30 s timeout, injected small in the test
- [ ] **Step 2: Run** `cargo test -p katwork --test models`. Expected: FAIL.
- [ ] **Step 3: Implement.** For `RigProvider`, add `rig-core` at its latest release and pin it in `Cargo.lock`. Map Rig's streaming text and tool-call items onto `ChatChunk`. Keep every Rig type inside `rig_adapter.rs`.
- [ ] **Step 4: Run.** Expected: PASS. Then run `cargo test -p katwork rig_adapter -- --ignored`, a live smoke test that runs only when `OPENAI_API_KEY` is set and is skipped in CI.
- [ ] **Step 5: Commit** with `feat: model gateway with residency policy, fake and rig providers`.

### Task 8: Tool registry and agent loop

**Files:**
- Create: `crates/katwork/src/agent/{mod,tool,registry,run,guards}.rs`, `crates/katwork/tests/agent.rs`

**Interfaces:**
- Consumes: `ModelProvider`, `Dot`, `Message`, and `EventSink` and `AgUiEvent` from Task 9.
- Produces:
  - `#[async_trait] trait Tool: Send + Sync { fn spec(&self) -> ToolSpec; fn side_effect(&self) -> SideEffect; async fn call(&self, ctx: &ToolCtx, args: serde_json::Value) -> Result<String, ToolError>; }`
  - `ToolCtx { tenant: Tenant, thread_id: ThreadId, dot_id: DotId, pool: PgPool }`
  - `ToolError::{NeedsApproval, InvalidArgs(String), Failed(String), Timeout}`
  - `RunInput { thread_id: ThreadId, messages: Vec<Message>, cancel: tokio_util::sync::CancellationToken }`
  - `SideEffect::{Read, WorkspaceWrite, Outward}`. Outward tools are refused in SP1 with `ToolError::NeedsApproval`; the approval host arrives in SP3.
  - `ToolRegistry::get(&self, id: &str) -> Option<Arc<dyn Tool>>`, where an unknown id is a tool error result, never executed
  - Built-in tools: `read_thread` (Read) and `post_reply` (WorkspaceWrite)
  - `run::execute(dot: &Dot, input: RunInput, provider: Arc<dyn ModelProvider>, tools: &ToolRegistry, sink: &dyn EventSink, limits: Limits) -> RunOutcome`
  - `RunOutcome::{Answered { reply: String }, Stopped { reason: StopReason }, NeedsInput { question: String }}`
  - `StopReason::{ProviderUnavailable, StepLimit, LoopDetected, NeedsApproval, Cancelled}`
  - `Limits { max_steps: 24, tool_timeout: 60s, result_max_chars: 8000, repeat_limit: 3 }`

- [ ] **Step 1: Write the failing tests** (FakeProvider scripted):
  - `plain_text_reply_is_answered`
  - `tool_call_then_reply`
  - `unknown_tool_id_returns_error_result_and_continues`
  - `same_tool_same_args_three_times_stops_loop_detected`
  - `step_limit_24_stops`
  - `tool_result_truncated_to_8000_chars`
  - `outward_tool_refused_with_needs_approval`
  - `tool_timeout_60s_returns_error_result`: the clock is injected
  - `cancellation_token_stops_with_cancelled`
- [ ] **Step 2: Run** `cargo test -p katwork --test agent`. Expected: FAIL.
- [ ] **Step 3: Implement.** The loop alternates model stream → tool calls → tool results until the model stops calling tools or a guard fires. Each step emits AG-UI events through `sink`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: tool registry and guarded agent loop`.

### Task 9: Event log and AG-UI SSE

**Files:**
- Create: `crates/katwork/src/events/{mod,agui,log}.rs`, `crates/katwork-server/src/routes/events.rs`, `crates/katwork-server/tests/events.rs`

**Interfaces:**
- Produces:
  - `enum AgUiEvent`, serialized with `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]`. Variants: `RunStarted{threadId,runId}`, `TextMessageStart{messageId,role}`, `TextMessageContent{messageId,delta}`, `TextMessageEnd{messageId}`, `ToolCallStart{toolCallId,toolCallName,parentMessageId}`, `ToolCallArgs{toolCallId,delta}`, `ToolCallEnd{toolCallId}`, `ToolCallResult{messageId,toolCallId,content}`, `RunFinished{threadId,runId}`, `RunError{message,code}`, `Custom{name,value}`.
  - KatWork-specific events use `Custom` with name `katwork.dot_state` (`{dotId, state: idle|working|waiting|error}`) or `katwork.job_status` (`{jobId, status, reason?}`).
  - `log::append(pool, workspace_id, thread_id, run_id, &AgUiEvent) -> Result<EventSeq, KatError>`
  - `log::since(pool, &Tenant, ThreadId, after: EventSeq) -> Result<Vec<(EventSeq, AgUiEvent)>, KatError>`
  - `trait EventSink: Send + Sync { fn emit(&self, e: AgUiEvent); }`
  - `PgEventSink`, which implements `EventSink` and is backed by `log::append` plus a `tokio::sync::broadcast` per thread
  - Route: `GET /api/workspaces/:w/threads/:t/events`, SSE with `id:` set to `EventSeq`. On connect it replays `since(Last-Event-ID or 0)`, then streams live with a keep-alive every 15 s.

- [ ] **Step 1: Write the failing tests:**
  - `events_serialize_as_agui_json`: `TextMessageContent` → `{"type":"TEXT_MESSAGE_CONTENT","messageId":"…","delta":"…"}`
  - `reconnect_with_last_event_id_has_no_gap_or_duplicate`: Review Focus 3
  - `thai_delta_never_splits_a_character`: the sink buffers deltas at char boundaries, so a scripted chunking of `"สวัสดีครับ"` reassembles byte-equal and every delta is valid UTF-8
  - `other_workspace_cannot_subscribe`: 404
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: event log and AG-UI SSE with resume`.

### Task 10: Jobs and status transitions

**Files:**
- Create: `crates/katwork/src/jobs/{mod,job,task}.rs`, `crates/katwork/tests/jobs.rs`

**Interfaces:**
- Produces:
  - `JobStatus::{Accepted, Ready, Answered, Stopped, NeedsInput, Cancelled, Error}`; the policy-owned subset excludes `Cancelled` and `Error`
  - `job::create(pool, &Tenant, ThreadId, MessageId) -> Result<JobId, KatError>`, which creates a running job: `status` NULL until it is finished
  - `job::finish(pool, JobId, JobStatus, reason: Option<String>) -> Result<(), KatError>`, where finishing a finished job errors with `job_already_finished`
  - `task::enqueue(pool, JobId, DotId, input: serde_json::Value, idempotency_key: &str) -> Result<TaskId, KatError>`, idempotent on the key
- Every change emits `katwork.job_status`.

- [ ] **Step 1: Write the failing tests:**
  - `finish_sets_status_and_emits_event`
  - `cannot_finish_twice`
  - `enqueue_is_idempotent_on_key`
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: jobs, tasks, status transitions`.

### Task 11: Leases, worker pool and restart recovery

**Files:**
- Create: `crates/katwork/src/jobs/{lease,worker}.rs`, `crates/katwork/tests/leases.rs`

**Interfaces:**
- Consumes: `task::enqueue`, `run::execute`, `PgEventSink`, `Registry`.
- Produces:
  - `lease::claim(pool, worker_id: &str, ttl: Duration) -> Result<Option<ClaimedTask>, KatError>`, using `SELECT … FOR UPDATE SKIP LOCKED` on queued tasks plus tasks whose lease has expired
  - `ClaimedTask { task_id: TaskId, job_id: JobId, dot_id: DotId, input: serde_json::Value, attempts: i32 }`
  - `lease::heartbeat(pool, TaskId, worker_id, ttl)`
  - `lease::complete(pool, TaskId, worker_id, outcome)`
  - `worker::spawn_pool(state, n: usize, ttl: Duration) -> JoinSet<()>`. TTL defaults to 60 s with a heartbeat every 20 s. `attempts` increments on claim, and at `attempts > max_attempts (3)` the job finishes `error` with `retries_exhausted`.

- [ ] **Step 1: Write the failing tests:**
  - `two_workers_never_claim_same_task`: 50 tasks, 2 workers in parallel, each task claimed once
  - `expired_lease_is_reclaimed_once`: worker A claims and stops heartbeating; after the TTL worker B claims it; worker A's late `complete` is rejected with `lease_lost`
  - `retries_exhausted_finishes_job_error`
- [ ] **Step 2: Run** `cargo test -p katwork --test leases`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: leased worker pool with restart recovery`.

### Task 12: Message → job pipeline (SP1 routing)

**Files:**
- Create: `crates/katwork/src/pipeline.rs`, `crates/katwork-server/tests/pipeline.rs`
- Modify: `crates/katwork-server/src/routes/threads.rs` (`POST` messages now returns `job_id`), `crates/katwork-server/src/main.rs` (`serve` spawns the worker pool)

**Interfaces:**
- Consumes: Tasks 5–11.
- Produces:
  - `pipeline::on_user_message(state, &Tenant, ThreadId, MessageId) -> Result<JobId, KatError>`
  - SP1 routing (spec §6.2 Stage 0 subset; Laya arrives in SP4): `@mention` of a Dot in the thread's team → that Dot; otherwise the team's Lead.
  - The task input is the thread's last 20 messages.
  - Outcome mapping:
    - `Answered` → reply message from the Dot, job `answered`
    - `Stopped` → system message with the localized reason code, job `stopped`
    - `NeedsInput` → Dot question message, job `needs_input`

- [ ] **Step 1: Write the failing tests** (FakeProvider):
  - `message_without_mention_goes_to_lead_and_job_answered`
  - `mention_routes_to_named_dot_including_thai_name`
  - `provider_unavailable_ends_job_stopped_with_reason`: Review Focus 5; the system message code is `provider_unavailable` and the job finishes within the test timeout
  - `demo_mode_reply_is_labelled_synthetic`
  - `full_event_sequence`: in order, `RUN_STARTED`, `katwork.dot_state working`, `TEXT_MESSAGE_*`, `RUN_FINISHED`, `katwork.job_status answered`, `katwork.dot_state idle`
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat: message to job pipeline with mention routing`.

### Task 13: Web app (login, reset, team, chat) — Claude implements, Codex reviews

**Files:**
- Create: `web/` (Vite React TS), `web/src/{main.tsx,app.tsx,api.ts,sse.ts}`, `web/src/i18n/{index.ts,en.json,th.json}`, `web/src/pages/{Login,ResetRequest,ResetConfirm,Team,Chat}.tsx` with `*.test.tsx`, `crates/katwork-server/src/static_files.rs`
- Modify: `crates/katwork-server/src/app.rs` (serve `web/dist` at `/`, SPA fallback), `Dockerfile` (web build stage)

**Interfaces:**
- Consumes: the auth, workspaces, teams, threads and events routes.
- Produces:
  - `sse.ts`: `subscribe(url, onEvent): () => void`, which uses `EventSource`, relies on the native `Last-Event-ID` for resume, and folds `TEXT_MESSAGE_CONTENT` deltas by `messageId`
  - `i18n.t(key, vars?)` with locales `en` and `th`; the locale switch persists in `localStorage`, and the locale is read with try/catch

- [ ] **Step 1: Write the failing tests** (Vitest + Testing Library):
  - `login_shows_localized_error_on_401`, in both `en` and `th`
  - `reset_request_shows_same_message_regardless_of_email`
  - `team_page_lists_lead_first_with_thai_names`
  - `chat_streams_deltas_into_one_bubble`: a mocked EventSource with 3 deltas gives 1 bubble with the concatenated text
  - `synthetic_reply_shows_badge`: a `[SYNTHETIC]` prefix renders a visible "Demo / ตัวอย่าง" badge
  - `every_key_exists_in_both_locales`: `en.json` and `th.json` have identical key sets
- [ ] **Step 2: Run** `cd web && npm test`. Expected: FAIL.
- [ ] **Step 3: Implement** the pages, plain CSS with tokens, and keyboard-reachable controls. Serve `dist/` from Axum with SPA fallback; `/api/*` never falls back.
- [ ] **Step 4: Run** `npm test && npm run build && cargo test -p katwork-server`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat: web app for login, reset, team and streaming chat`.

### Task 14: End-to-end compose smoke and demo mode

**Files:**
- Create: `scripts/smoke.sh`, `crates/katwork-server/tests/e2e_compose.rs` (`#[ignore]`, run in a CI job)
- Modify: `.github/workflows/ci.yml` (add an `e2e` job), `README.md` (a "Try it" section)

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write `scripts/smoke.sh`.** It must:
  1. Run `docker compose up -d --build` with `KATWORK_DEMO=1`.
  2. Wait for `/healthz`.
  3. Run `seed-demo`.
  4. Log in as demo, post `"สวัสดี ช่วยแนะนำทีมหน่อย"`.
  5. Read SSE until `katwork.job_status`.
  6. Assert status `answered` and a reply containing `[SYNTHETIC]`.
  7. Run `docker compose restart katwork` mid-run in a second scenario and assert the job still finishes exactly once.
- [ ] **Step 2: Run** `scripts/smoke.sh`. Expected: FAIL before wiring, PASS after.
- [ ] **Step 3: Implement** any glue the smoke test exposes (image entrypoint runs `migrate` then `serve`).
- [ ] **Step 4: Run** `scripts/smoke.sh`. Expected: `SMOKE OK`.
- [ ] **Step 5: Commit** with `test: compose smoke for demo mode and restart recovery`.

---

## Dependency order and parallelism

`1 → 2 → {3, 7} → 4 → 5 → 6 → 9 → 8 → 10 → 11 → 12 → 13 → 14`

Task 9 comes before Task 8 because the agent loop emits through `EventSink`.

- Task 7 (model gateway) can run in parallel with Tasks 3–6 after Task 2.
- Task 13 can start its UI against mocked APIs after Task 6 and integrate after Task 12.
- At most 3 Codex and 2 Claude workers at once (spec §19.1).
