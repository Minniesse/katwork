# KatWork roadmap

This document lists every KatWork sub-project from the design spec (`docs/spec/katwork-design.md` §19), plus the sub-projects proposed for the expanded scope (9 to 14), what each one covers, what it depends on, its rough size, the order of work, and the entry criteria for the public alpha. It is a map, not a plan. Each sub-project gets its own detailed plan, and the founder approves each plan before that build starts (spec §19.1). The founder decided (2026-11-25) that each plan is written and reviewed one wave ahead of its build; that is process, not scope.

Where this roadmap and the spec or an approved plan disagree, the spec and the plan win. Scope listed here comes from the spec, the approved plans, the ADRs in `docs/adr/`, and the founder's rulings and decisions of 2026-11-26 on the expanded scope (see "Expanded scope" and "Decisions" below). The spec changes the expanded scope needs are in `docs/spec/2026-11-26-scope-amendment.md`. The founder approved them with this re-plan, and they are still to be written into the spec; until then, each approved change takes precedence over the spec text it changes. The few questions the approval did not settle are listed at the end.

## Sizes

Sizes count the tasks in a sub-project's plan:

| Size | Plan tasks |
|---|---|
| S | 1 to 8 |
| M | 9 to 20 |
| L | more than 20 |

Two sizes are counted from approved plans. The sub-project 1 plan has 17 task headings (Tasks 1 to 14 except 13, plus 3b, 7b, 13a and 13b), so it is M. The sub-project 2a plan has 44 tasks, so it is L. Every other size is an estimate from the spec's scope and carried requirements, and the sub-project's own plan replaces it. These letters are not the S/M sizes inside the sub-project 2a plan, which measure worker time for a single task.

## Summary

| # | Sub-project | Tracking issue | State | Size | Plan |
|---|---|---|---|---|---|
| 1 | Core runtime | #128 | Done | M (17 tasks) | `docs/plans/2026-10-22-sp1-core-runtime.md` |
| 2a | Documents and evidence | #129 | Building | L (44 tasks) | `docs/plans/2026-10-24-sp2a-documents-evidence.md` |
| 2b | Kat computers | #130 | Not planned | L (estimate) | Not written |
| 2c | Memory | #131 | Not planned | M (estimate) | Not written |
| 3 | Gates and policy | #132 | Not planned | L (estimate) | Not written |
| 4 | Routing | #133 | Not planned | M (estimate) | Not written |
| 5 | Channels | #134 | Not planned | M (estimate) | Not written |
| 6 | Views and shells | #135 | Not planned | M (estimate, 15 tasks); L with the additions (23) | Not written |
| 7 | Craft integration beyond 2a | #136 | Not planned | M (estimate, 20 tasks); L with the addition (22) | Not written |
| 8 | Federation, after the alpha | #137 | Not planned | L (estimate) | Not written |
| 9 | Companion | None yet | Not planned | L (estimate) | Not written |
| 10 | Spend | None yet | Not planned | M (estimate) | Not written |
| 11 | Voice and meetings | None yet | Not planned | L (estimate) | Not written |
| 12 | Connectors and plugins | None yet | Not planned | L (estimate) | Not written |
| 13 | Routines | None yet | Not planned | M (estimate) | Not written |
| 14 | Team admin | None yet | Not planned | M (estimate) | Not written |
| Alpha | Public alpha release | #138 | Not started | S (estimate) | Not written |

Sub-projects 9 to 14 come from this re-plan, which the founder approved on 2026-11-26 (Q8). They get tracking issues next. The sizes in this table include the additions from the expanded scope, but not the differentiation proposals DP1 to DP14 (scope amendment), the reversible-choices rule (Q25) or the letter and memo template (Q26), which "Estimated work" counts separately. Alpha 1 builds sub-projects 1 to 7 without those additions; the additions come in later waves ("Re-plan: staged alphas" below).

## Order of work

Two sources give the order.

**Spec §19 (build order).** The sub-projects are listed in build order, and each depends only on the ones before it: 1, then 2 (2a, 2b, 2c), then 3, 4, 5, 6 and 7. Sub-project 8 comes after the alpha. This is a dependency rule; it does not forbid building sub-projects side by side.

**Spec §17 (weekly milestones).** The approved week-by-week schedule runs several sub-projects side by side:

- agentOS computers and the browser supervisor (2b) sit in the week of 2026-11-09, beside 2a's document tools.
- The approval host, the policy function and statuses (3) sit in the week of 2026-11-16, before 2a ends.
- Laya in shadow mode and Stage 0 routing (4), the LINE and Slack adapters (5), and the tasks-and-files board (6) sit in the week of 2026-11-23.
- The memory stores and the RustShare connector (2c) sit last among the builds, in the week of 2026-12-28.
- The sub-project 2a plan has its own week table, which ends with OCR in the weeks of 2026-12-21 and 2026-12-28.

**How plans are written.** The founder decided (2026-11-25) that the plans for 2b, 2c and 3 are written and reviewed while 2a builds, and that each later plan is written one wave ahead of its build. This is process, not scope.

**Where the two sources conflict.** Some dependencies run against the §19 order or the §17 weeks:
- Sub-project 4 carries a recall tool and a memory byte cap (§19 item 4), which need 2c's memory, but §17 runs routing in the weeks of 2026-11-23 and 2026-11-30, before 2c in the week of 2026-12-28. Decided with the re-plan: 4 waits for 2c (Q2).
- Sub-project 3's templates need teams (§8) and so sub-project 4's lead-Kat planning and delegation, and the research brief needs saved web spans, which arrive with 2b; §19 lists sub-project 3 before 4. Decided with the re-plan: 3's template tasks come after 4 (Q4).

Sub-project 4's Stage 0 also sends approval callbacks to sub-project 3's approval host and resumes `needs_input` jobs from it (§6.2). That fits both sources: §19 lists 3 before 4, and §17 puts the approval host in the week of 2026-11-16, before Stage 0 in the week of 2026-11-23.

The rescheduling after the slip in Q1 is in "Re-plan: staged alphas" below.

## Sub-projects

### 1. Core runtime (#128): done

**Scope** (spec §19 item 1):
- The Docker Compose skeleton.
- Auth with password reset and demo accounts, operator-created accounts and admin invites (spec §13, ADR-0004).
- Workspaces, Spaces, Kats and Teams.
- A local OpenAI-compatible model provider (ADR-0005).
- The scheduler with leases, KatWork's own agent loop over Rig model clients, and the model gateway (ADR-0001).
- The AG-UI event stream and web chat (ADR-0003).

In sub-project 1, `needs_input` is a final job status: the person's next message starts a follow-up job (ADR-0002). Resuming the same job comes with sub-project 3.

**Depends on:** nothing.

**Size:** M (17 tasks).

### 2a. Documents and evidence (#129): building

**Scope** (spec §19 item 2, the sub-project 2a plan):
- In-process document tools on pure-Rust libraries: DOCX, XLSX and PDF readers behind a package guard, plus DOCX and XLSX writers with read-back. PPTX is read for evidence only.
- The artifact store, with immutable originals and atomic versions.
- The evidence model (ADR-0007): source spans, `cite` and claim records.
- The deterministic verifier and the claim judge, which can lower a claim but never lift it.
- The `katwork-thai` crate: normalization, Thai numbers, and Buddhist-Era and fiscal-year dates.
- The `katwork-craft` sidecar with all 13 Craft apps (ADR-0011): the forks, the signed image, sessions behind the MCP policy proxy, the bridge and its web origin, the Kat Craft tools, evidence for Craft saves, and the control lease for watching and taking over.
- The meeting-minutes demo.
- File deletion (the PDPA purge).
- Typst PDF reports.
- KatBench and KatBench-Inject.
- Judge calibration and the synthetic claim gold sets.
- The reader challenge.
- Thai OCR, last (ADR-0008).
- Untrusted content follows ADR-0010: hidden text never reaches the model, and instruction-like text never supports a claim.

**Depends on:** sub-project 1.

**Size:** L (44 tasks). The plan gives the dependency graph, the critical path and the cut order if the demo week slips.

**Status on 2026-11-26:** Tasks 1, 2, 3, 6 and 8 have merged (5 of 44). Tasks 4, 5 and 13 are in review.

**Outstanding founder actions** listed in the plan (they gate work but are not design questions):
- the legal read of the Typhoon OCR terms;
- the legal read of which public Thai documents are usable;
- the start of the two Thai reviewers, and approval of the annotator programme;
- permission to publish the Craft image and keep the forks public;
- the Craft licence notes;
- approval of the Craft round, which the plan still lists as outstanding and ADR-0011 still records as Proposed.

### 2b. Kat computers (#130)

**Scope** (spec §9, §13, §14, §19 item 2; ADR-0009):
- agentOS workspaces as the light code tier only (JavaScript, Python through Pyodide, WASM and files), with one sidecar pool per workspace.
- The Rust browser supervisor, with one Chromium container per Kat on a runtime ladder: a microVM when the host has KVM, otherwise gVisor, otherwise hardened runc with a visible banner.
- One KatWork egress gateway outside the sandbox.
- Encrypted disk checkpoints, restored only into a quarantined computer.
- A per-Kat credential vault.
- The private sign-in card.
- The browser take-over lease and its permissions.
- Captured, hashed downloads that become Kat-owned inputs.
- The rest of the carried requirements in spec §19 item 2, including the GUI tier with a virtual display, the control-lease API, guest filesystem snapshots, CDP on a per-Kat network, Web Bot Auth signing, the env allowlist, the sidecar lifecycle, argv process APIs, quotas, package pinning and the real-Chromium CI lane.
- From the 2a plan's deferrals: browser reader and authenticated modes, and the lab brief for the in-house agentOS.

**Entry gate, first:**
- The inventory of commands the alpha templates need inside agentOS.
- A CI conformance test proving guest code cannot reach host files, host processes, the network or another Kat, for every enabled runtime.

Both feed ADR-0009's kill criteria for switching the code tier to gVisor. KatsAgentOS, the in-house sandbox that replaces agentOS, is a separate track after the alpha. Sub-project 2b builds only the agentOS adapter and the `Computer` contract.

**Depends on:** sub-project 1, and 2a's artifact store: browser downloads become Kat-owned inputs through a migration that relaxes 2a's check that "an input has `created_by_user` and no Kat".

**Size:** L (estimate). The spec carries about twenty requirements into it.

### 2c. Memory (#131)

**Scope** (spec §11, §19 item 2):
- Per-Kat private notes and a Qdrant collection.
- Per-team and per-Space shared pages, files and decisions.
- Tantivy with nlpO3 for exact Thai and English search, with a versioned Thai analyzer ADR (the 2a plan's seam).
- An optional RustShare connector.
- A `remember` tool that proposes entries with provenance. An entry becomes active only when a person accepts it or a Space policy auto-accepts it. Entries are listed, deletable, deduplicated and expiring. Per the 2a plan, `remember` from a run that read untrusted content needs a person.
- Memory ACLs evaluated at query time against the thread's Space and the requesting person.
- A byte cap on memory that is always in the prompt.

**Depends on:** sub-project 1, and 2a's `katwork_thai::normalize_th` and stored `match_key`.

**Size:** M (estimate).

### 3. Gates and policy (#132)

**Scope** (spec §6.5, §7, §19 item 3; ADR-0002; the 2a plan's SP3 seam):
- The approval host.
- Proposals with expiry, precondition digest, revision and states, with admin-set expiry per action type, re-checked right before acting.
- An approve endpoint that executes once per tool call.
- Action types from KatWork's registry.
- Approval cards rendered by code.
- An audit log.
- The policy function and job statuses.
- Resume for `needs_input` jobs and for approvals in one change, replacing sub-project 1's follow-up jobs.
- Acceptance on top of 2a's evidence verifier: zero failed claims, and every claim in the required-evidence scope passed.
- A contract hash that stops the job when the contract changes.
- The same-Kat rule, which caps a job at `ready`.
- Checks that pass only on a harness-owned completion record, where before/after drift makes a claim unverified and stale (§5.1).
- Replay and kill-switch tests. §17 runs these gates in the hardening week of 2027-01-04; sub-project 3 builds them.
- LLM-judged evaluations that use a different model or provider from the system under test.
- Every team DAG has exactly one sink task, owned by the lead or the Writer; an invalid plan goes back as a tool error within the repair cap (§6.4). Sub-project 4 builds the planner that must honour it.
- Templates and their acceptance contracts.
- Template filling, after a fill-engine bake-off; editing existing workbooks; lopdf's constrained PDF edits.
- The render check, advisory until it passes the §9.1 gate.
- Taint-aware gates, which use the trust flags 2a records on every tool call.
- Real-model injection runs, seeded from KatBench-Inject.
- The KatBench SAR and One Report worlds, with the templates.

This list is not complete; §19 item 3 has the full set of carried requirements.

All six templates in spec §8 are alpha goals (§1.1 goal 8), and §18's cut order names none of them. §19 item 3 gives templates to this sub-project, while the 2a plan builds the meeting-minutes template and §17 puts the HA rubric pack in the hardening week of 2027-01-04. Decided with the re-plan (Q4): sub-project 3 builds the other five, in template tasks that come after sub-project 4, and no template is cut.

**Depends on:** 2a (the evidence verifier, evidence reports and trust flags; a scanned Thai PDF goes through OCR in the §15 fidelity gates). Its templates also need sub-project 4's lead-Kat planning and delegation for their teams (§8), and the research brief needs saved web spans from 2b (Q4).

**Size:** L (estimate).

### 4. Routing (#133)

**Scope** (spec §6.2 to §6.4, §19 item 4):
- Stage 0 in code: approval callbacks, `@mentions`, the closed set of eligible Kats, Thai script detection, an `addressed` flag, per-channel trigger modes, dropping bot messages, and a hop cap on `KatEnvelope`.
- Laya in Rust from a checksum-pinned bundle, in shadow mode until each language passes the 50-case calibration gate.
- The 200-case calibration set and its preregistered protocol.
- Lead-Kat planning: a validated DAG of at most 5 tasks and 3 levels. The one-sink-task rule is carried in §19 item 3 (sub-project 3), and this planner must honour it.
- Delegation through `KatEnvelope`.
- Context and provider-quota handling: a compaction threshold, a bounded handoff on model change, a recall tool, a memory byte cap, and a queue hold until the provider limit resets.

Plan-item closure (§6.4) is decided by a lead-Kat structured check plus the registered checks, and its items drive the live graph and the office view. §19 does not assign it to a sub-project, and the approval did not settle this (Q7, still open).

The 2a plan notes that the minutes team (Writer, Scheduler, Reviewer) needs this delegation; the 2a demo runs one Writer.

**Depends on:** sub-project 1; sub-project 3 (Stage 0 sends approval callbacks to the approval host and resumes `needs_input` jobs, §6.2); 2c (the recall tool and the memory byte cap, Q2).

**Size:** M (estimate).

### 5. Channels (#134)

**Scope** (spec §11, §19 item 5):
- One adapter boundary.
- Seven adapters: LINE, Slack, Teams, RustChat/Mattermost, Telegram, Discord, and WhatsApp through the official Cloud API.
- The documented tunnel recipe for webhook channels behind NAT.
- Approval cards in each channel (§7.2), and clarifying questions through LINE quick-reply and Slack buttons where available (§6.4).
- The channel tests in spec §15, including `two_bots_in_one_channel_do_not_loop`.

**Depends on:** sub-project 1; sub-project 3 (approval cards need the approval host, and clarifying questions need resume); sub-project 4 (`two_bots_in_one_channel_do_not_loop` relies on Stage 0 dropping bot and self messages and on the `KatEnvelope` hop cap).

**Size:** M (estimate).

### 6. Views and shells (#135)

**Scope** (spec §10, §19 item 6):
- The tasks-and-files board, the accessible fallback for everything else.
- The live agent graph.
- The 3D office, including desk monitors that show a Kat's browser or a labelled Craft preview, and a Computer panel that reuses 2a's Craft panel.
- The Tauri desktop shell and the Capacitor mobile shell.
- One read-only screencast per Kat, with input only from the lease holder.
- Allowlisted client input for AG-UI.
- From the 2a plan's seam: inline evidence markers inside a document preview, and an agent cursor and follow mode.

All views read one event model.

**Depends on:** sub-project 1's event stream; 2a's Craft previews and panel; 2b's browser screencast; sub-project 3 (the board shows the approval queue); sub-project 4 (plan items drive the live graph and the office view, §6.4).

**Size:** M (estimate, 15 tasks); L with the additions from the expanded scope (23).

### 7. Craft integration beyond 2a (#136)

**Scope** (spec §19 item 7; the 2a plan's SP7 seam): what 2a defers.
- One `katwork-craft` container per workspace, and a Craft web origin per session.
- The UI plane: an app running windowed on a virtual display, with a screencast and an agent cursor.
- Citing content KatWork cannot read yet: CAD drawings, audio, video and image content beyond OCR's text. DWG open and save.
- Precise PPTX visibility, so that a claim citing a deck can pass, and Kats opening decks they did not make.
- Long renders and exports as background jobs.
- Per-role tiers of the app policies, and approvals for outward Craft actions.
- Following the forks' upstream PRs, and moving each fork back to its upstream.
- KatWork's own detection of font substitution.
- The Craft and annotation tests.

**Depends on:** 2a's Craft track; approvals for outward Craft actions need sub-project 3; the UI plane's virtual display relates to 2b's GUI tier.

**Size:** M (estimate, 20 tasks); L with the addition from the expanded scope (22).

Decided (Q3): this sub-project is in Alpha 1, including the streamed Craft desktop.

### 8. Federation, after the alpha (#137)

**Scope** (spec §12, §19 item 8): two KatWork installs can see each other's shared Kats and exchange typed work requests and approvals, without a central server. The approach is a selective fork of reason-network, with:
- an OIDC identity provider and each workspace's own keys;
- KatWork's own relay;
- admin-granted, revocable membership;
- tenant-scoped PostgreSQL transactions with a durable outbox.

A peer can never approve a local side effect.

**Depends on:** the sub-projects before it (spec §19). It moved to the first release after the alpha (founder decision, 2026-10-22), and nothing in the alpha depends on it.

**Size:** L (estimate). The spec calls the work "far more than the one week the alpha schedule held".

## Expanded scope (founder rulings, 2026-11-26)

The founder read a static teardown of a competing desktop agent and made three rulings:

- Adopt all of that product's features: every capability area becomes KatWork scope.
- Put all of it before the alpha: all of it is alpha scope, and the alpha date moves.
- Accept this re-plan's recommendation for staged alphas: everything adopted stays product scope. Alpha 1 ships the current spec scope, and the new areas follow as later alpha waves, each with its own plan approval (see "Re-plan: staged alphas").

This repo calls that product "a competing desktop agent" and does not name it. KatWork takes ideas and scope from the teardown only, never its code, prompts or text.

The capability areas are: agent tools and planning; computer use and the browser; personal data, voice, phone calls and meetings; commerce, payments and a vault; connectors, plugins and MCP; teams, sharing and admin; automations and the chat shell.

The rulings set scope and the wave structure. The founder then approved this re-plan with its recommended answers ("Decisions" at the end). Sub-projects 9 to 14 are the six new sub-projects (Q8). Nothing in them is built before its plan is approved.

Some parts of the competing product run against KatWork's spec or ADRs, for example a browser that hides that it is automated, or link previews in chat. The first ruling adopts every capability area. The decisions keep KatWork's design where they conflict (Q18 to Q21): no stealth browsing, no egress through a person's machine, telemetry off by default, no link previews, and no model that approves actions.

### 9. Companion

**Scope:**
- A person's own computer as an optional execution target, beside the Kats' server-side computers.
- Local commands and file access on that computer, within grants the person sets.
- Local computer use: the accessibility tree, screen actions, and grants per app.
- Personal-data tools on that computer: messages, mail, notes, reminders, calendar, contacts, photos and location, each behind its own grant and each outward send a Proposal (§6.5).
- Desktop notifications, a tray, a capture overlay and element picking, built into the Tauri shell from sub-project 6.

**Rules proposed for its plan and the companion trust-model ADR** (not decided):
- The companion checks every approval itself, for example against a signed proposal digest, instead of trusting a flag from the server.
- Its privileges sit in a separate helper with only the permissions each part needs.
- Every send on a person's behalf is checked after sending. When KatWork cannot tell whether a message went out, it says so, instead of reporting it as sent or as failed.

**Depends on:** sub-project 3 (approvals and the proposal digest), sub-project 6 (the Tauri shell), 2b (the `Computer` contract), and the companion trust-model ADR.

**Size:** L (estimate, 30 tasks).

### 10. Spend

**Scope:**
- Payment instruments for Kats, for example one-time or limited cards, through each install's own payment provider account. Installs without a provider fall back to checkouts a person pays (Q9).
- `pay` action parameters: amount, currency, merchant and recipient.
- Spend limits per Kat, per Space and per day, and an optional expected price that a payment may not exceed by more than a set margin. These are deterministic gates in code; no model decides a payment.
- Card storage in the per-Kat vault from 2b, as write-only fields.
- A second factor for approving payments, for example a passkey or a hardware security key on the person's own device, after a threat model (with sub-project 9).
- Adding cards by scanning them, and importing entries from a password manager.
- Order and receipt records. A payment whose result KatWork cannot confirm gets its own outcome, separate from success and failure.
- Usage limits. Credits and top-ups belong to the Cloud edition (Q10).

**Depends on:** sub-project 3 (action types and gates), 2b (the vault), sub-project 12 (merchant and travel connectors), and the payments ADR.

**Size:** M (estimate, 16 tasks with the payment second factor).

### 11. Voice and meetings

**Scope:**
- Realtime voice chat with a Kat, dictation and voice memos, in Thai and English.
- Outbound phone calls by a Kat, each call an approved Proposal.
- Meeting capture: a Kat joins or records a meeting with recorded consent, and the meeting-minutes template takes the audio.
- An STT and TTS engine qualified for Thai, as ADR-0008 did for OCR.
- Recordings and transcripts follow §13 deletion and a retention rule.

**Depends on:** sub-project 5 (channels, for calls and voice messages), sub-project 6 (voice in the views and shells), 2a (the meeting-minutes template), the voice ADR, the recording-retention ADR, and the legal read on calls and recording (Q11). Voice chat, dictation and minutes from an uploaded recording come first; calls and live meeting capture stay off until the read passes.

**Size:** L (estimate, 25 tasks).

### 12. Connectors and plugins

**Scope:**
- A connector catalog with the OAuth lifecycle: a sign-in card, typed refusal and sign-out reasons, and several accounts per connector.
- Connector credentials: per person, per Kat service account, or delegated, with write-only secrets.
- General MCP servers beyond Craft: tools, resources, auth and elicitation, behind the same policy posture as Craft's MCP proxy.
- Named connectors chosen by Thai-market demand (Q13), including mail and calendar, a password manager, travel, forge, social search and finance.
- Image and video generation as model connectors, opt-in, with cost caps (Phase 4's creative Kat capability, pulled forward).
- External coding agents launched as connectors.
- A plugin package format with strict ingestion checks, and a team marketplace with optional, default and required plugins.

**Depends on:** sub-project 3 (the tool allowlist and gates), 2b (the vault and the egress gateway), the MCP posture ADR, and a connector-credentials ADR.

**Size:** L (estimate, 30 tasks).

### 13. Routines

**Scope:**
- Scheduled routines on sub-project 1's scheduler.
- Triggers: webhooks, channel messages, mail, forge and incident tools, with encrypted webhook keys. In Alpha 2, mail triggers come only through sub-project 12's mail connector; triggers from the inbound mail adapter (additions to 5) come with it in Alpha 3.
- Chained prompts, retries, and validation when a routine is turned on.
- Scheduled browser research (Phase 3's capability, pulled forward).

**Rule proposed for its plan** (not decided): text a trigger delivers is untrusted (ADR-0010). A routine started by a webhook or a message never holds an outward tool without a person.

**Depends on:** sub-project 3, sub-project 5 (channel triggers) and sub-project 12 (connector triggers).

**Size:** M (estimate, 12 tasks).

### 14. Team admin

**Scope:**
- Access lists, groups and roles for Kats and teams (viewer, collaborator, manager), beyond today's workspace admin and member.
- Sharing a Kat by copy, with a disclosure card before sharing that lists what travels (files, skills, vault entries, connectors).
- Service accounts, and delegated logins with an acknowledged warning and an admin kill switch.
- An admin console: the command denylist, tool and connector allowlists, the egress dashboard with each Kat's applied status, and network diagnostics.
- Tracking which blocked connectors people ask for, after a privacy review. It is off by default, an admin turns it on, and its data stays on the install (§13 telemetry rule).

**Depends on:** sub-project 3, sub-project 6 and sub-project 12. SSO and SCIM stay with the Enterprise edition (§16). Service accounts and the admin console are Enterprise features (Q10).

**Size:** M (estimate, 15 tasks).

### Additions to existing sub-projects

- **2b Kat computers:** a general Linux desktop on the GUI tier, with take-over; teach recording (a person records a task and the Kat proposes steps, reviewed before use); screen recording; the egress gateway failing closed when its policy cannot refresh, and an applied status per Kat; card and contact entries in the vault as write-only fields, and typed secret requests; credential fill that clears the fields and hands over to the person when it fails; browser dialogs shown to the person, never accepted automatically; clipboard transfer that is explicit and logged; idle pause, then parking when nobody is present.
- **2c Memory:** a review queue for shared memory and for skills; Kat-authored skills with version history, review and revert; untrusted shared memory flagged as such.
- **3 Gates and policy:** new action types (`pay`, `message-on-behalf`, `record`); spend limits (the payment second factor is in sub-project 10, Alpha 3); typed denial reasons returned to the Kat; an admin command denylist; approval rules per person and per team, where a rule that requires asking a person always overrides a rule that allows an action; bulk approval only for identical requests and never for outward or destructive actions; harness hooks before and after tool calls; a `pending_external` state distinct from an error; data-use preconditions per connector; a line on every approval card that explains why the job stopped for approval. Also from the decisions: choices a Kat makes for you, only reversible ones inside a job, listed with their reason on the completion receipt and the approval card (Q25, 1 task); and an official letter and memo template with a register check (Q26, 4 tasks, counted separately).
- **4 Routing:** rooms where Kats talk to each other, with cost and loop rules; questions a Kat asks while it keeps working (this changes ADR-0002's resume model and needs an ADR note); version conflicts on tracked tasks; goal continuation.
- **5 Channels:** SMS and inbound mail adapters (a Kat's own number and address); notifications; a clear install state for each channel app.
- **6 Views and shells**, by wave:
  - Alpha 2: the review-queue view and admin pages (3 tasks).
  - Alpha 3: onboarding, Thai and English first with more locales by demand, and in-app notices for outages and updates; chat with several people and Kats in one thread, maths and chart rendering, and presentations in chat (within ADR-0010's plain-text rule); a shared canvas; Kat creation with avatars and template browsing; a sandboxed renderer with no network for tool-result cards, with an ADR-0010 amendment (Q20) (4 tasks).
  - Alpha 4: voice controls (1 task).
  - The companion's Tauri shell features are listed and counted once, in sub-project 9.
- **7 Craft beyond 2a** (Alpha 2): making the virtual display and UI plane reusable by local computer use, screen recording and the general desktop (2 tasks).

## Re-plan: staged alphas

On 2026-11-26 the founder accepted this re-plan's recommendation for staged alphas, then approved the re-plan with its recommended answers ("Decisions" below). Everything adopted stays product scope, but it ships in waves:
- **Alpha 1** is the first public alpha. It carries:
  - the current spec scope: sub-projects 1 to 7 as specified, without the additions above;
  - the six differentiation proposals the published claims rest on (Q27);
  - the streamed Craft desktop that sub-project 7 builds (Q3).

  Federation (sub-project 8) stays after the alpha.
- **Alpha waves 2 and later** deliver the new areas: sub-projects 9 to 14, the additions to existing sub-projects, and the other differentiation proposals. They are ordered by dependency, and each wave's plans need their own founder approval.

The dates below are proposed targets. The founder approved them as targets; they are re-forecast at the pace review.

### Pace

Facts from the repository on 2026-11-26:
- The sub-project 1 plan (17 tasks) merged in two days, 2026-10-22 and 2026-10-23, before the current review pipeline existed. That pipeline has two reviewers, a verifier and up to three fix rounds.
- The sub-project 2a plan merged on 2026-10-24. Since then 5 of its 44 tasks have merged: Tasks 8 (2026-10-25), 3 (2026-10-27), 1 (2026-10-28), 2 (2026-11-25) and 6 (2026-11-26). Tasks 4, 5 and 13 are in review; Tasks 4 and 5 have been open since 2026-10-30.
- Average pace over those 4.71 weeks: 1.06 merged tasks a week, written "1.1" below.
- Best week: 3 tasks in the first 5 days, about 4 a week. Then nothing merged for four weeks.

The build pipeline runs at most 4 builders at once.

**Assumption for the dates:**
- Each of the 4 builders merges 1 task a week, and at most 2 builders work on one sub-project.
- That is a nominal 4 tasks a week, SP2a's best measured week. It is about 3.8 times SP2a's 1.06 average, and the pipeline has not yet sustained it.
- Because sub-projects wait for each other, the schedule below delivers less than 4 a week in practice:
  - 3.75 a week in Alpha 1 (199 tasks in 53 weeks);
  - 3.45 a week over all four waves (400 tasks in 116 weeks).
- A pace review in the week of 2027-01-25 re-forecasts every date from eight weeks of measured pace.

### Estimated work

| Wave | Contents (tasks, estimate) | Tasks |
|---|---|---|
| Alpha 1 | 2a (39 left of 44); 2b (31: 30 plus DP6); 2c (15); 3 (39: 25 for gates and policy, including DP4; 14 for the templates, including DP1, DP7 and DP11, after sub-project 4); 4 (15); 5 (15); 6 (17: 15 plus DP2); 7 (20); release work (8) | 199 |
| Alpha 2 | 12 Connectors and plugins (30); 13 Routines (12); 14 Team admin (15); the additions to 2b (8), 2c (4), 3 (10, including the reversible-choices rule from Q25) and 4 (4); the Alpha 2 additions to 6 (3: the review-queue view and admin pages); the addition to 7 (2); the official letter and memo template (4, Q26); the other differentiation proposals DP3, DP5, DP8, DP9, DP10, DP12, DP13 and DP14 (17); release work (4) | 113 |
| Alpha 3 | 9 Companion (30, including its Tauri shell features); 10 Spend (16, including the payment second factor); the additions to 5 (4); the Alpha 3 additions to 6 (4); release work (4) | 58 |
| Alpha 4 | 11 Voice and meetings (25); voice controls in the views (1, the Alpha 4 addition to 6); release work (4) | 30 |
| **Total** | | **400** |

How to read these counts:
- Every number except 2a's is an estimate, and each sub-project's plan replaces it.
- A second browser automation runtime (Q28) is not counted. It adds tasks to Alpha 2 only if 2b's plan finds a need for it.
- Each later wave carries its own release work, so four waves need 12 more release tasks than one alpha would.

### Why this order

- **Alpha 1** is the spec as written, plus the claim-carrying differentiation proposals. Its sub-projects depend only on each other:
  - 2b needs 2a's artifact store (Task 2, merged), and 2c needs `normalize_th` (Task 1, merged).
  - 4 needs 3 and 2c (Q2). 5 needs 3 and 4. 6 needs 3, 4 and 2b. 7 needs 2a's Craft track, 3 and 2b.
  - Sub-project 3's templates need sub-project 4's planning and delegation, and the research brief needs 2b's saved web spans. So sub-project 3 is split (Q4): its gates and policy come before 4, and its template tasks come after 4.
- **Alpha 2** is the platform layer the other new areas use.
  - Connectors (12) need 3 and 2b. Routines (13) need 12. Team admin (14) needs 12 and the admin pages from the additions to 6.
  - The additions to 2b, 2c, 3 and 4, the review-queue view and admin pages, and 7's reusable virtual display are here, because sub-projects 9, 10 and 14 build on them.
  - In Alpha 2, routines take mail triggers only through sub-project 12's mail connector. Triggers from the inbound mail adapter come with it in Alpha 3.
- **Alpha 3** acts for a person and pays.
  - Companion (9) needs 3, 6 and 2b. Spend (10) needs 3, 2b and 12.
  - Both need the legal read on acting on a person's behalf (Q12) and their ADRs.
- **Alpha 4** is voice and meetings (11).
  - It needs 5, 6 and 2a, the Thai speech engine gate and the recording-retention ADR.
  - Calls and live meeting capture also need the Thai legal read (Q11). Alpha 4 comes last because it carries the most legal and quality risk.

### Schedule

The schedule follows these rules:
- 4 builders, at most 2 on one sub-project, 1 merged task per builder per week. No week uses more than 4 builders.
- A sub-project starts once each of its dependencies was three-quarters merged at the start of that week, as stacked PRs. It finishes only after its dependencies finish.
- Where a sub-project depends on sub-project 3, it depends on 3's gates-and-policy part (25 tasks). Only 3's 14 template tasks depend on sub-project 4 (and on 2a and 2b).
- Each wave's release work (hardening, demo mode, docs) starts only after all the other work in that wave is done.
- Inside Alpha 2, routines (13) and team admin (14) wait for sub-project 12 to be three-quarters merged, and 14 also waits for the admin pages. That is why Alpha 2's 113 tasks take 31 weeks, not 29.
- 2b, 2c and 3 cannot start before their plans are approved (target week of 2026-12-14).
- Plans and ADRs are written one wave ahead of their builds and do not use builder time.
- A later wave starts building when the previous wave's builds end, while that wave's dry runs and release candidate run.

The order inside Alpha 1 is the one that finishes earliest among all orders of its nine work items, under these rules.

| Week of (proposed) | Milestone |
|---|---|
| 2026-11-30 | 2a continues (Tasks 4, 5 and 13 in review). Plans for 2b, 2c and 3 in writing. Legal reads requested (Q11, Q12). |
| 2026-12-14 | Plans for 2b, 2c and 3 approved (target). 2b starts beside 2a. |
| 2027-01-25 | Pace review: eight weeks of measured pace against the assumption; every date below is re-forecast (Q1). This was the old public-alpha week. |
| 2027-03-29 | Sub-project 3 (gates and policy) starts. |
| 2027-04-05 | 2b done: Kat computers, the browser supervisor, the vault, the offline-install test. |
| 2027-04-12 | 2c starts. |
| 2027-04-19 | 2a done: documents and evidence, all 13 Craft apps, the meeting-minutes demo, OCR. |
| 2027-06-07 | 2c done: memory. Sub-project 4 starts (routing, Laya in shadow mode, delegation). |
| 2027-06-28 | Sub-project 3's gates and policy done, with the completion receipt. Sub-project 7 starts. |
| 2027-07-19 | Sub-project 7 pauses, and its builders move to sub-project 3's template tasks. |
| 2027-07-26 | Sub-project 5 starts. |
| 2027-08-02 | Sub-project 4 done. |
| 2027-09-06 | Sub-project 3 done, with the templates and their seeded-defect worlds. Sub-project 6 starts (views and shells). |
| 2027-09-20 | Sub-project 5 done. Sub-project 7 resumes. |
| 2027-10-11 | Plans for Alpha 2 approved (target): 12, 13 and 14, the Alpha 2 additions, the letter and memo template, and the Alpha 2 differentiation proposals. The MCP posture, connector-credentials and recording-retention ADRs reviewed, and the PDPA legal read for screen and teach recordings back. |
| 2027-11-08 | Sub-projects 6 and 7 done. Alpha 1 release work: hardening (kill-switch, replay and fidelity gates), demo mode, docs. |
| 2027-12-06 | Alpha 1 builds end. Design-partner dry runs. Alpha 2 builds start. |
| 2027-12-13 | Fixes from the partners. |
| 2027-12-20 | Licence review. Release candidate. |
| 2027-12-27 | **Alpha 1, public.** |
| 2028-03-20 | Sub-project 12 done. |
| 2028-04-10 | Plans for Alpha 3 approved (target): 9, 10 and the Alpha 3 additions. Companion trust-model and payment ADRs reviewed; the Q12 legal read back. |
| 2028-07-10 | Alpha 2 builds end (13 and 14 done). Alpha 3 builds start. |
| 2028-07-24 | **Alpha 2.** |
| 2028-08-07 | Plans for Alpha 4 approved (target): 11. Voice ADR reviewed; the Q11 legal read back. |
| 2028-11-06 | Alpha 3 builds end. Alpha 4 builds start. |
| 2028-11-20 | **Alpha 3.** |
| 2029-02-19 | Alpha 4 builds end. |
| 2029-03-05 | **Alpha 4.** |

Why sub-project 7 spans 19 weeks for 20 tasks:
- Its two builders work 3 weeks (2027-06-28 to 2027-07-12) on the parts that need only 2a, 2b and 3.
- They then move to sub-project 3's template tasks, which sit on the critical path.
- They come back from 2027-09-20.

Giving 7 its builders without a break would delay 3's templates, and Alpha 1 with them.

Alpha 4 runs longer than its 30 tasks suggest. Sub-project 11 is one sub-project, and at most 2 builders work on one sub-project.

### Range by pace

One method gives every row. Take the schedule above, built at a nominal 4 tasks a week. Stretch its build spans from 2026-11-30 by 4 divided by the pace: 53 weeks to the end of Alpha 1 builds, and 116 weeks to the end of Alpha 4 builds. Then add the same release weeks: 3 for Alpha 1 and 2 for Alpha 4.

| Nominal pace (tasks a week) | Alpha 1 public | Alpha 4 public |
|---|---|---|
| 1.1 (SP2a's average) | week of 2030-09-02 | week of 2035-01-15 |
| 2 | week of 2029-01-01 | week of 2031-05-26 |
| 4 (SP2a's best week; the approved target) | week of 2027-12-27 | week of 2029-03-05 |

**What else moves the dates:**
- Founder gates: plan approvals, the legal reads (Q11, Q12), and the design-partner annex (Q5, not yet settled).
- Alpha 3's outward parts (sends as a person, payments) and Alpha 4's calls and live capture cannot ship before their legal reads.
- No early slice of a later wave comes before Alpha 1 (Q22).

**Cut order.** Inside Alpha 1, §18's cut order (WhatsApp and Teams, then Capacitor, then 3D) still stands as written. No cut is planned across the waves.

## Differentiation

On 2026-11-26 the founder asked for KatWork to outperform other products, through research into what people want and need, across many fields. A first research wave did four things:
- read published studies on office work;
- read studies on trust and delegation with AI;
- read studies on the Thai work context;
- coded public complaints about AI tools in English and Thai.

Its findings feed each sub-project's plan.

**Status of this section.** The founder accepted every response below (Q27).
- Where a response is already in the spec, an ADR or an approved plan, the table names that source.
- Every other response is a differentiation proposal, DP1 to DP14 in the scope amendment, with an owner, a task estimate and a wave. The founder accepted all fourteen (Q27): DP1, DP2, DP4, DP6, DP7 and DP11 are in Alpha 1, and the rest in Alpha 2.
- Alpha 1 counts the 12 tasks of its six proposals, and Alpha 2 the other 17.

**Limits of the evidence.**
- Every count comes from one coder.
- The Thai first-person voice is thin: a few dozen public forum posts, with no replies.
- No source measures willingness to pay.
- Satisfied users rarely post.

The ranking is ordinal, so needs that sit next to each other should be read as ties.

### Unmet needs, ranked

The order follows three things: how many independent sources support each need, how intense the complaints are, and how close the need is to KatWork's mission.

| # | What people need | Proposed response | Already in the spec or a plan | Proposals (owner) |
|---|---|---|---|---|
| 1 | **No silent wrong numbers.** Wrong output that looks right is the most damaging failure, and the least noticed. People want every figure checked against a source they can open. | Every number, date and quote in a draft is marked passed, failed or unverified. The missing-evidence list is shown. A failed or unverified claim blocks acceptance of a section that needs evidence. Seeded-defect test worlds cover every alpha template. | Claim statuses and the verifier (§7.1, ADR-0007); the acceptance rule (§19 item 3); KatBench seeded worlds (2a plan) | DP1 more trap kinds and a world per template (3) |
| 2 | **Data stays in the building.** People want a plain answer to "where does my file go". This is a top fear in organisations, and PDPA enforcement is real. | Residency per Space and local-only Spaces. A screen per Space showing which providers see it and what each model call sent. A personal-data flag on a folder. A redaction check before outward sends. An offline-install test. | Residency per Space, local models (§11, §13); the HA redaction check (§8); the egress gateway (ADR-0009) | DP5 data screen, folder flag, redaction on every outward send (3, with 6); DP6 offline-install CI test (2b) |
| 3 | **Review that costs less than redoing the work.** Checking AI output often eats the time it saved. | One click opens the source at the exact span. Claims ordered by risk, failed first. Reviewer minutes logged from use, not surveys. | Source spans (ADR-0007); the evidence card (2a plan) | DP2 risk ordering and the span-open timing test (6); DP3 reviewer minutes (6) |
| 4 | **Right in the person's own file and template, including Thai print.** Spreadsheets and slides come out worst, and Thai numerals, tone marks and fonts break. | The deliverable opens in the office's own template and reads back. Each template declares its date era and digit form. Thai print is checked on two renderers. | Template writers with read-back (2a plan); template fill and the render check (§19 item 3); fidelity gates (§15) | DP7 date-era and digit declaration (3); DP8 two-renderer Thai print check (3) |
| 5 | **Approvals that catch errors.** Approval prompts decay into rubber stamps, and blame lands on the nearest person. | Each approval card stores exactly what it showed, and there are more ways to answer. | Proposals, approval cards and the audit log (§6.5, §7.2) | DP9 approve with a comment, unsure, forced span opening for high-risk claims, a card budget per job, the card content in the audit row, seal-and-initial approval, a fast-approval alarm (3, with 6) |
| 6 | **An honest "done".** Agents claim completion they did not achieve. | Completion comes from a harness-owned record, never from the model. Reports are generated by code. | The harness-owned completion record (§19 item 3); reports generated by code (§7.1); stops name a reason (§6.6) | DP4 a completion receipt from the job log, where a skipped file or a zero-match run fails (3) |
| 7 | **Staying in control.** People want partnership, not full automation. | Clarifying questions, Proposals for outward actions, pausing, and take-over. | Clarifying questions (§6.4); Proposals (§6.5); pause all Kats (§6.6); take-over (§9) | DP10 redo one section without a restart, and a stop-latency target (4) |
| 8 | **Thai official-document correctness.** This covers Buddhist-Era dates, numerals, register, the official letter form, and scanned input. | Thai normalisation and Buddhist-Era dates. OCR lines below confidence are flagged. | `katwork-thai` and OCR flags (2a plan, ADR-0007) | An official letter and memo template with a register check, in Alpha 2 (Q26; 3, 4 tasks) |
| 9 | **Reconcile across files, show only the exceptions, and keep the same figure everywhere.** | In a pack, every output draws each figure from one verified fact, and cross-file conflicts become an approval card. | The monthly pack (§8) | DP11 a matched-and-exceptions output (3) |
| 10 | **Predictable cost.** People want an estimate first, no silent burn, and no silent switch to a weaker model. | Run budgets, and usage per Kat and job. A provider limit stops the job with a named reason. | Run budgets (§6.6); usage tracking and limits (§11) | DP12 a pre-run estimate, a workspace cap, an "unpriced" state and no silent downgrade (4, with 6) |
| 11 | **Memory of facts and decisions, with their reasons, across a long job.** | Shared decisions per Space, and `remember` with provenance. | §11 memory (2c) | DP13 a decision log with reasons and rejected options (2c) |
| 12 | **An easy start for non-technical staff.** | Chat as the mobile default, and the board as the accessible fallback. | §10 | DP14 a first run with one visible next action, and a glossary (6) |

### Five claims to test and publish

These are targets, not measurements. KatWork will test each one, and publish both the method and the result. Each claim holds once its proposals are built; all of them are in Alpha 1 (Q27).

| # | Claim | Owner | Wave | Where it is tested |
|---|---|---|---|---|
| 1 | On KatBench's seeded-defect worlds, no planted wrong figure, unit mix or date-era swap is ever marked as passed. | 3 (with 2a's verifier) | Alpha 1 | KatBench runs in CI |
| 2 | A reviewer opens the exact source span behind any claim in under 10 seconds. | 6 (with 2a's evidence card) | Alpha 1 | A timed internal run on the alpha templates |
| 3 | A job's completion receipt is generated from its log, and no statement in it is contradicted by that log. Skipped files and zero-match runs fail. | 3 | Alpha 1 | CI on seeded skipped-file and zero-match runs, plus an audit of 50 jobs |
| 4 | An offline install makes zero outbound connections. No outward action runs unless a person approved it: in Alpha 1, that exact action. In later waves, where bulk approval of identical requests or a capped standing grant applies (Q21), it is the exact batch or grant that covers it. | 2b (offline test), 3 (approval gate) | Alpha 1 | CI: an offline-install test and the approval-gate replay suite (§15) |
| 5 | Every figure in a monthly pack is identical across all its outputs, and Thai dates and numerals follow the template's declared form with zero violations. | 3 (DP7, DP11) | Alpha 1 | CI on generated monthly packs |

Claims about how reviewers behave need real reviewers, for example their catch rate with and without the proof panel. They are design-partner measurements in the dry runs, not launch claims.

### Adopted areas the evidence does not yet support

The research found no supporting evidence for these areas. It found some evidence of risk: privacy fears, reluctance to hand over credentials, and third-party consent. They are **kept per the founder's ruling; evidence to gather**. None is dropped, and each already sits in a later wave behind its legal read.

| Area | Wave | Evidence to gather |
|---|---|---|
| Payments (sub-project 10) | Alpha 3 | Whether offices want Kats to pay, and for what; the rails question (Q9) |
| Outbound phone calls (sub-project 11) | Alpha 4 | Office demand; the Thai legal read (Q11) |
| Local desktop control (sub-project 9) | Alpha 3 | Which tasks people would hand over on their own computer |
| Personal data from apps on a person's own computer (sub-project 9) | Alpha 3 | Demand among LINE-first and Windows users (Q13) |
| Teach recording (additions to 2b) | Alpha 2 | Whether people would record tasks, and the recording rules |

## Alpha (#138)

**Date:** the spec's §1.1 heading reads "Goals for the alpha (2027-01-31)", with the public alpha in the week of 2027-01-25 (§17). That date cannot hold. Under the staged alphas, this section describes **Alpha 1**, the first public alpha, with the current spec scope. With the claim-carrying differentiation proposals (Q27) and the streamed Craft desktop (Q3), "Re-plan: staged alphas" gives the week of 2027-12-27 for Alpha 1 at a nominal 4 merged tasks a week, and dates for Alphas 2 to 4. The founder approved these dates as targets; they stay proposed and are re-forecast at the pace review (Q1). Each later wave repeats the release work and entry criteria below for its own scope, plus the gates of its ADRs and legal reads.

**Work that belongs to the release itself.** No §19 item owns this work:
- Running the hardening gates (kill-switch, replay and fidelity). Sub-project 3 builds the kill-switch and replay tests; §17 runs the gates in the week of 2027-01-04, the same week as the HA rubric pack, which is a template (Q4).
- Demo mode.
- Docs.
- Design-partner dry runs and the fixes from them.
- The licence review.
- A release candidate.
- The launch: GitHub, `docker compose up`, a landing page and launch posts.

These come from §17's last four weeks.

**Entry criteria.** Each comes from the spec or the 2a plan.

1. Every goal in spec §1.1 is met, unless §18's cut order removed it.
   - `docker compose up` on one Linux server.
   - The default team.
   - Messages from every listed channel.
   - Real work on real computers.
   - Proof on every deliverable.
   - Deterministic approvals.
   - The four views.
   - The six showcase templates.
   - Login with password reset and demo accounts.
2. The §15 gates pass:
   - Kill switch: with every model off, no job reaches `accepted`.
   - Policy replay over saved jobs.
   - The document fidelity gates.
   - The approval gate tests, including every channel's callback.
   - The channel tests, including a behind-NAT install.
   - The view tests (browser, Tauri and Capacitor builds, Thai input, screen reader, reconnecting).
   - A licence review of sources, models, fonts and assets.
3. The 50-case routing and delegation benchmark is the release gate (§15). A language that fails it stays in shadow mode, and the lead Kat routes (§6.3, §18).
4. No Kat runs code in agentOS until the CI conformance test passes for every enabled runtime (§13, ADR-0009).
5. Each Craft app or operation that fails its §9.1 gate is forbidden in that app's policy. A failing optional app never blocks a template, but a template whose Rust baseline fails its Thai gate is blocked (§9.1). GridCraft's check of XLSX outputs is not optional in this sense: it runs on every XLSX save, its outage marks the version "not checked by GridCraft", and a file it reports Excel would repair is refused.
6. Demo mode runs with no keys, and its canned decisions are labelled synthetic. A live demo never depends on an external login (§15, §18).
7. The 2a plan's legal reads come before any customer deployment uses the parts they gate. Until the Typhoon OCR terms pass, Typhoon OCR stays off.
8. The human-labelled Thai claim set from real documents, which the 2a plan defers, is built before the design-partner dry runs (proposed week of 2027-12-06, "Re-plan: staged alphas"). No sub-project owns it yet.

**Fallbacks in §18:** for Thai document fidelity, a narrower supported-feature list; if the Craft track slips, the forks the demo does not use merge after it, and their apps stay watchable through previews.

**Cut order if a week slips twice** (§18): WhatsApp and Teams first, then Capacitor, then 3D (the board stays).

**Not in the alpha** (spec §1.2 as written): KatsAgentOS, a VM per Kat for code, voice and video, federation, native Linux binaries inside a Kat's computer, high availability, a hosted cloud, unofficial WhatsApp Web, and GPL or AGPL code in the core.

The streamed Craft desktop is no longer on this list: it comes into Alpha 1 with sub-project 7 (Q3). The list holds for Alpha 1. The decisions keep native binaries, the hosted cloud and multi-host scheduling out of every alpha wave (Q17, Q9, Q23). "Voice and video calls" moves into Alpha 4 (sub-project 11), with calls and live capture off until the legal read passes (Q11). The spec changes are applied from the scope amendment.

## After the alpha

The spec names, without assigning them to sub-projects:
- federation (sub-project 8);
- KatsAgentOS, built by the lab (§14);
- the Enterprise edition features (§16);
- a hosted cloud (§16);
- the Phase 2 to 4 use cases (§8): PPTX output and Thai OCR at scale (Phase 2), a tracked-changes engine and scheduled browser research (Phase 3), and a creative Kat (Phase 4).

## Decisions (founder, 2026-11-26)

On 2026-11-26 the founder approved this re-plan and took the recommended answer to every question that offered one. The answers below are paraphrased. The text above already applies them. The dates stay proposed targets, to be re-forecast at the pace review in the week of 2027-01-25.

Four questions offered no recommended answer (Q2, Q4, Q5, Q7), and one did only in part (Q6). Where the approved schedule already embodies an answer, it is recorded as taken with the plan. The rest are listed after the table, because approving the re-plan does not settle them.

| Q | Question | Answer taken |
|---|---|---|
| 1 | The alpha dates | The staged-alpha dates in "Re-plan: staged alphas" are approved as targets, at a nominal 4 merged tasks a week, and re-forecast at the pace review (week of 2027-01-25). |
| 2 | Routing before memory | Taken with the approved schedule: sub-project 4 waits for 2c, so the recall tool and the memory byte cap build on 2c's memory. |
| 3 | Sub-project 7 and the alpha | Sub-project 7 is in Alpha 1. The "streamed Craft desktop" non-goal is removed for Alpha 1. |
| 4 | Templates | Taken with the approved schedule: all five remaining templates are built in sub-project 3, by its template tasks, which come after sub-project 4. No template is cut. |
| 6 | After the alpha (in part) | The new scope is placed in Alphas 2 to 4. Scheduled browser research (Phase 3) arrives with sub-project 13, and image and video generation (Phase 4) with sub-project 12, both in Alpha 2. |
| 8 | Sub-project split | Six new sub-projects, 9 to 14. |
| 9 | Payment rails | Each install brings its own payment provider account and keys, in Alpha 3. Installs without a provider fall back to checkouts that a person pays. |
| 10 | Editions | Approvals, gates and spend limits stay in the free core. Approval rules, the marketplace, service accounts and the admin console go to Enterprise. Credits and top-ups go to Cloud. |
| 11 | Legal reads on calls and recording | Both reads are requested now. Voice chat with your own Kat, dictation and minutes from an uploaded recording are built first. Calls and live meeting capture stay off until the reads pass. |
| 12 | Legal read on acting for a person | Requested with Q11. Every send as a person and every payment stays a Proposal a person approves until the read and the ADRs land. |
| 13 | Apple-only features against LINE-first users | The companion targets Windows and macOS through the Tauri shell. Personal data comes first through cross-platform connectors (mail and calendar, LINE). Apple-only tools come last in sub-project 9's plan. |
| 14 | Connector priorities | Mail and calendar first, then file stores and LINE OA, then the rest by design-partner demand. |
| 15 | Voice languages and engine | Thai and English both. The engine is qualified by a gate on Thai office speech, with a local engine option for local-only Spaces. |
| 16 | Companion platforms | Windows and macOS desktop in Alpha 3. Mobile gets notifications only. |
| 17 | Shell inside a Kat's computer | The argv-only agentOS exec stays in every alpha wave. Stdio MCP servers and plugin commands run as pinned containers. A full shell waits for KatsAgentOS. |
| 18 | Stealth browsing and egress through a person's machine | Neither is adopted. Both are recorded as rejected alternatives in ADR-0009. |
| 19 | Telemetry | §13 stays: no telemetry unless an admin turns it on. |
| 20 | Link previews and tool-result cards | No link previews. Tool-result cards only in a sandboxed renderer with no network, with an ADR-0010 amendment, in sub-project 6 (Alpha 3). |
| 21 | Unattended actions | Standing grants only bound to one recipient and content class, with caps. Hiding a Kat's screen keeps the activity log and the take-over path. A classifier may narrow permissions but never approve. |
| 22 | An early look at the new scope | No early slice before Alpha 1. |
| 23 | Private worker pools | The alpha waves stay single-host. Private pools come after the last alpha wave. |
| 24 | A phone as a tool host | Mobile is notifications only in the alpha waves (Q16), so a phone as a tool host comes after the last alpha wave. |
| 25 | Choices a Kat makes for you | Allowed only for reversible choices inside a job, listed with their reason on the completion receipt and the approval card. A template may forbid them. They are never allowed for a clarifying question, for anything outward or destructive, or for anything that changes a claim's source. This needs a §6.4 amendment, built in sub-project 3 in Alpha 2 (1 task). |
| 26 | An official letter and memo template | It moves into Alpha 2, in sub-project 3 (4 tasks, estimate). |
| 27 | The differentiation proposals | All fourteen are accepted. DP1, DP2, DP4, DP6, DP7 and DP11 (12 tasks) are in Alpha 1. The other eight (17 tasks) are in Alpha 2. |
| 28 | A second browser automation runtime | 2b's plan considers one only if it finds a need that chromiumoxide cannot meet, in Alpha 2. It is not counted in the estimates. |

### Not settled by the approval

These offered no recommended answer, so approving the re-plan does not decide them. Each needs its own founder answer before the plan it affects.

- **Q5, design partners.** Who the partners are, and when the data annex (§15) is signed. This is needed before the Alpha 1 dry runs (proposed week of 2027-12-06).
- **Q6, after the alpha (the rest).** How the Phase 2 to 4 use cases (§8), the Enterprise features (§16) and KatsAgentOS (§14) map onto sub-projects, and in what order relative to federation.
- **Q7, plan-item closure.** Whether §6.4's plan-item closure belongs to sub-project 4 or sub-project 3. This is needed before either plan is approved.
