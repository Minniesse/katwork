# KatWork roadmap

This document lists every KatWork sub-project from the design spec (`docs/spec/katwork-design.md` §19), plus the sub-projects proposed for the expanded scope (9 to 14), what each one covers, what it depends on, its rough size, the order of work, and the entry criteria for the public alpha. It is a map, not a plan. Each sub-project gets its own detailed plan, and the founder approves each plan before that build starts (spec §19.1). The founder decided (2026-11-25) that each plan is written and reviewed one wave ahead of its build; that is process, not scope.

Where this roadmap and the spec or an approved plan disagree, the spec and the plan win. Scope listed here comes from the spec, the approved plans, the ADRs in `docs/adr/`, and the founder's two rulings of 2026-11-26 on the expanded scope (see "Expanded scope" below). The spec changes that the expanded scope needs are proposals in `docs/spec/2026-11-26-scope-amendment.md`; until the founder approves them, the spec as written still wins. Anything nobody has ruled on is marked as an open question and collected at the end.

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
| 6 | Views and shells | #135 | Not planned | L (estimate, with the additions) | Not written |
| 7 | Craft integration beyond 2a | #136 | Not planned | L (estimate, with the additions) | Not written |
| 8 | Federation, after the alpha | #137 | Not planned | L (estimate) | Not written |
| 9 | Companion (proposed) | None yet | Not planned | L (estimate) | Not written |
| 10 | Spend (proposed) | None yet | Not planned | M (estimate) | Not written |
| 11 | Voice and meetings (proposed) | None yet | Not planned | L (estimate) | Not written |
| 12 | Connectors and plugins (proposed) | None yet | Not planned | L (estimate) | Not written |
| 13 | Routines (proposed) | None yet | Not planned | M (estimate) | Not written |
| 14 | Team admin (proposed) | None yet | Not planned | M (estimate) | Not written |
| Alpha | Public alpha release | #138 | Not started | S (estimate) | Not written |

Sub-projects 9 to 14 are proposed by this re-plan. They get tracking issues once the founder approves them. The sizes in this table include the additions from the expanded scope. Alpha 1 builds sub-projects 1 to 7 without those additions; the additions come in later waves ("Re-plan: staged alphas" below).

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
- Sub-project 4 carries a recall tool and a memory byte cap (§19 item 4), which need 2c's memory, but §17 runs routing in the weeks of 2026-11-23 and 2026-11-30, before 2c in the week of 2026-12-28 (Q2).
- Sub-project 3's templates need teams (§8) and so sub-project 4's lead-Kat planning and delegation, and the research brief needs saved web spans, which arrive with 2b; §19 lists sub-project 3 before 4 (Q4).

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

All six templates in spec §8 are alpha goals (§1.1 goal 8), and §18's cut order names none of them. §19 item 3 gives templates to this sub-project, while the 2a plan builds the meeting-minutes template and §17 puts the HA rubric pack in the hardening week of 2027-01-04. Which sub-project builds each of the others, and how their dependencies on later sub-projects are handled, is open (Q4).

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

Plan-item closure (§6.4) is decided by a lead-Kat structured check plus the registered checks, and its items drive the live graph and the office view. §19 does not assign it to a sub-project (Q7).

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

**Size:** M (estimate).

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

**Size:** M to L (estimate).

Whether this sub-project is needed for the alpha is not stated (Q3).

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

- "[...] features: all": every capability area of that product becomes KatWork scope. (The quote is shortened here because it names the product.)
- "All before the alpha": all of it is alpha scope, and the alpha date moves.
- "just do it from ur recommended [...]": this accepted the staged alphas this re-plan recommended. Everything adopted stays product scope. Alpha 1 ships the current spec scope, and the new areas follow as later alpha waves, each with its own plan approval (see "Re-plan: staged alphas").

This repo calls that product "a competing desktop agent" and does not name it. KatWork takes ideas and scope from the teardown only, never its code, prompts or text.

The capability areas are: agent tools and planning; computer use and the browser; personal data, voice, phone calls and meetings; commerce, payments and a vault; connectors, plugins and MCP; teams, sharing and admin; automations and the chat shell.

The rulings settle scope and the wave structure. They do not settle how each area is built, which spec sections change, or the questions listed at the end (Q8 to Q26). Sub-projects 9 to 14 below are this re-plan's proposal for where the new scope goes; the founder may merge or split them (Q8). Nothing in them is built before its plan is approved.

Some parts of the competing product run against KatWork's spec or ADRs, for example a browser that hides that it is automated, or link previews in chat. "All" adopts every capability area, but whether those particular designs are adopted is not settled; each is an open question with a recommended option (Q18 to Q21).

### 9. Companion (proposed)

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

### 10. Spend (proposed)

**Scope:**
- Payment instruments for Kats, for example one-time or limited cards, on rails the founder picks (Q9).
- `pay` action parameters: amount, currency, merchant and recipient.
- Spend limits per Kat, per Space and per day, and an optional expected price that a payment may not exceed by more than a set margin. These are deterministic gates in code; no model decides a payment.
- A second factor for approving payments, for example a passkey.
- Card storage in the per-Kat vault from 2b, as write-only fields.
- A hardware security key or passkey on the person's own device for approving payments, after a threat model (with sub-project 9).
- Adding cards by scanning them, and importing entries from a password manager.
- Order and receipt records. A payment whose result KatWork cannot confirm gets its own outcome, separate from success and failure.
- Credits, top-ups and usage limits, if the founder places them in this edition (Q10).

**Depends on:** sub-project 3 (action types and gates), 2b (the vault), sub-project 12 (merchant and travel connectors), and the payments ADR.

**Size:** M (estimate, 15 tasks).

### 11. Voice and meetings (proposed)

**Scope:**
- Realtime voice chat with a Kat, dictation and voice memos, in Thai and English.
- Outbound phone calls by a Kat, each call an approved Proposal.
- Meeting capture: a Kat joins or records a meeting with recorded consent, and the meeting-minutes template takes the audio.
- An STT and TTS engine qualified for Thai, as ADR-0008 did for OCR.
- Recordings and transcripts follow §13 deletion and a retention rule.

**Depends on:** sub-project 5 (channels, for calls and voice messages), sub-project 6 (voice in the views and shells), 2a (the meeting-minutes template), the voice ADR, the recording-retention ADR, and the legal read on calls and recording (Q11).

**Size:** L (estimate, 25 tasks).

### 12. Connectors and plugins (proposed)

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

### 13. Routines (proposed)

**Scope:**
- Scheduled routines on sub-project 1's scheduler.
- Triggers: webhooks, channel messages, mail, forge and incident tools, with encrypted webhook keys.
- Chained prompts, retries, and validation when a routine is turned on.
- Scheduled browser research (Phase 3's capability, pulled forward).

**Rule proposed for its plan** (not decided): text a trigger delivers is untrusted (ADR-0010). A routine started by a webhook or a message never holds an outward tool without a person.

**Depends on:** sub-project 3, sub-project 5 (channel triggers) and sub-project 12 (connector triggers).

**Size:** M (estimate, 12 tasks).

### 14. Team admin (proposed)

**Scope:**
- Access lists, groups and roles for Kats and teams (viewer, collaborator, manager), beyond today's workspace admin and member.
- Sharing a Kat by copy, with a disclosure card before sharing that lists what travels (files, skills, vault entries, connectors).
- Service accounts, and delegated logins with an acknowledged warning and an admin kill switch.
- An admin console: the command denylist, tool and connector allowlists, the egress dashboard with each Kat's applied status, and network diagnostics.
- Tracking which blocked connectors people ask for, after a privacy review.

**Depends on:** sub-project 3, sub-project 6 and sub-project 12. SSO and SCIM stay with the Enterprise edition (§16) unless the founder rules otherwise (Q10).

**Size:** M (estimate, 15 tasks).

### Additions to existing sub-projects

- **2b Kat computers:** Playwright as an alternative browser runtime, if the plan finds it needed beside chromiumoxide; a general Linux desktop on the GUI tier, with take-over; teach recording (a person records a task and the Kat proposes steps, reviewed before use); screen recording; the egress gateway failing closed when its policy cannot refresh, and an applied status per Kat; card and contact entries in the vault as write-only fields, and typed secret requests; credential fill that clears the fields and hands over to the person when it fails; browser dialogs shown to the person, never accepted automatically; clipboard transfer that is explicit and logged; idle pause, then parking when nobody is present.
- **2c Memory:** a review queue for shared memory and for skills; Kat-authored skills with version history, review and revert; untrusted shared memory flagged as such.
- **3 Gates and policy:** new action types (`pay`, `message-on-behalf`, `record`); spend limits and the payment second factor (with sub-project 10); typed denial reasons returned to the Kat; an admin command denylist; approval rules per person and per team, where a rule that requires asking a person always overrides a rule that allows an action; bulk approval only for identical requests and never for outward or destructive actions; harness hooks before and after tool calls; a `pending_external` state distinct from an error; data-use preconditions per connector; a line on every approval card that explains why the job stopped for approval.
- **4 Routing:** rooms where Kats talk to each other, with cost and loop rules; questions a Kat asks while it keeps working (this changes ADR-0002's resume model and needs an ADR note); version conflicts on tracked tasks; goal continuation.
- **5 Channels:** SMS and inbound mail adapters (a Kat's own number and address); notifications; a clear install state for each channel app.
- **6 Views and shells:** onboarding, Thai and English first with more locales by demand, and in-app notices for outages and updates; chat with several people and Kats in one thread, maths and chart rendering, and presentations in chat (within ADR-0010's plain-text rule); the review-queue view; admin pages; the desktop shell features listed in sub-project 9; a shared canvas; voice controls; Kat creation with avatars and template browsing; a sandboxed renderer for tool-result cards, if the founder adopts them (Q20).
- **7 Craft beyond 2a:** making the virtual display and UI plane reusable by local computer use, screen recording and the general desktop.

## Re-plan: staged alphas

On 2026-11-26 the founder accepted this re-plan's recommendation for staged alphas. Everything adopted stays product scope, but it ships in waves:
- **Alpha 1** is the first public alpha. It carries the current spec scope: sub-projects 1 to 7 as specified, without the additions above. Federation (sub-project 8) stays after the alpha. Alpha 1 is re-dated from the measured pace.
- **Alpha waves 2 and later** deliver the new areas: sub-projects 9 to 14 and the additions to existing sub-projects. They are ordered by dependency, and each wave's plans need their own founder approval.

### Pace

Facts from the repository on 2026-11-26:
- The sub-project 1 plan (17 tasks) merged in two days, 2026-10-22 and 2026-10-23, before the current review pipeline (two reviewers, a verifier and up to three fix rounds) existed.
- The sub-project 2a plan merged on 2026-10-24. Since then 5 of its 44 tasks have merged: Tasks 8 (2026-10-25), 3 (2026-10-27), 1 (2026-10-28), 2 (2026-11-25) and 6 (2026-11-26). Tasks 4, 5 and 13 are in review; Tasks 4 and 5 have been open since 2026-10-30.
- Average pace over those 4.7 weeks: about 1.1 merged tasks a week.
- Best week: 3 tasks in the first 5 days, about 4 a week. Then nothing merged for four weeks.

The build pipeline runs at most 4 builders at once, one wave at a time.

**Assumption for the proposed dates:** 4 merged tasks a week. This is SP2a's best measured week, and one task per builder per week. It is about 3.8 times SP2a's average, and the pipeline has not yet sustained it. The range below gives the dates at 1.1, 2 and 4 tasks a week. A pace review in the week of 2027-01-25 re-forecasts every date from eight weeks of measured pace.

### Estimated work

| Wave | Sub-projects | Tasks left (estimate) |
|---|---|---|
| Alpha 1 | 2a (39: plan, 44 tasks, 5 merged), 2b (30, L), 2c (15, M), 3 (30, L), 4 (15, M), 5 (15, M), 6 (15, M), 7 (20, M to L), release work (8) | 187 |
| Alpha 2 | 12 Connectors and plugins (30), 13 Routines (12), 14 Team admin (15), the additions to 2b (8), 2c (4), 3 (10) and 4 (4), release work (4) | 87 |
| Alpha 3 | 9 Companion (30), 10 Spend (15), the additions to 5 (4), 6 (8) and 7 (2), release work (4) | 63 |
| Alpha 4 | 11 Voice and meetings (25), with the voice controls from the additions to 6, release work (4) | 29 |
| **Total** | | **366** |

Every number except 2a's is an estimate, and each sub-project's plan replaces it. The total is 12 more than a single alpha would need, because each later wave has its own release work.

### Why this order

- **Alpha 1** is the spec as written. Its sub-projects depend only on each other:
  - 2b needs 2a's artifact store (Task 2, merged), and 2c needs `normalize_th` (Task 1, merged).
  - 4 needs 3 and 2c. 5 needs 3 and 4. 6 needs 3, 4 and 2b. 7 needs 2a's Craft track, 3 and 2b.
  - Q3 asked whether sub-project 7 belongs to the alpha. The staged-alpha ruling places it in Alpha 1; Q3 closes when the founder confirms that reading.
- **Alpha 2** is the platform layer the other new areas use.
  - Connectors (12) need 3 and 2b. Routines (13) need 3, 5 and 12. Team admin (14) needs 3, 6 and 12.
  - The additions to 2b, 2c, 3 and 4 include the new action types, the vault fields and the review queue. Sub-projects 9 and 10 depend on them.
- **Alpha 3** acts for a person and pays.
  - Companion (9) needs 3, 6 and 2b. Spend (10) needs 3, 2b and 12.
  - Both need the legal read on acting on a person's behalf (Q12) and their ADRs.
- **Alpha 4** is voice and meetings (11).
  - It needs 5, 6 and 2a, the Thai speech engine gate and the recording-retention ADR.
  - Calls and live meeting capture also need the Thai legal read (Q11). It comes last because it carries the most legal and quality risk.

### Schedule

The schedule assumes:
- 4 builders, at most 2 on one sub-project.
- A sub-project starts once each dependency is three-quarters merged, as stacked PRs, and finishes only after its dependencies finish.
- Plans and ADRs are written one wave ahead of their builds and do not use builder time.
- Each later wave starts building when the previous wave's builds end, while that wave's dry runs and release candidate run.

| Week of | Milestone |
|---|---|
| 2026-11-30 | Founder review of this re-plan and the scope amendment. 2a continues (Tasks 4, 5 and 13 in review). Plans for 2b, 2c and 3 in writing. |
| 2026-12-14 | Plans for 2b, 2c and 3 approved (target). 2b starts beside 2a. |
| 2027-01-25 | Pace review: eight weeks of measured pace against the 4-a-week assumption; every date below is re-forecast (Q1). This was the old public-alpha week. |
| 2027-03-29 | 2b done: Kat computers, the browser supervisor, the vault. Sub-project 3 starts. |
| 2027-04-12 | 2c starts. |
| 2027-04-19 | 2a done: documents and evidence, all 13 Craft apps, the meeting-minutes demo, OCR. |
| 2027-06-07 | 2c done: memory. |
| 2027-06-14 | Sub-project 4 starts (routing, Laya in shadow mode, delegation). |
| 2027-07-12 | Sub-project 3 done: approvals, gates, templates. Sub-project 7 starts. |
| 2027-07-19 | Sub-project 5 starts (channels). |
| 2027-08-02 | Sub-project 6 starts (views and shells). |
| 2027-08-09 | Sub-project 4 done. |
| 2027-09-13 | Sub-project 5 done. |
| 2027-09-27 | Sub-project 6 done: board, live graph, 3D office, Tauri and Capacitor shells. |
| 2027-10-11 | Plans for Alpha 2 (12, 13, 14 and the additions to 2b, 2c, 3 and 4) approved (target). The MCP posture and connector-credentials ADRs reviewed. |
| 2027-11-15 | Sub-project 7 done. Alpha 1 release work: hardening (kill-switch, replay and fidelity gates), demo mode, docs. |
| 2027-12-13 | Alpha 1 builds end. Design-partner dry runs. Alpha 2 builds start. |
| 2027-12-20 | Fixes from the partners. |
| 2027-12-27 | Licence review. Release candidate. |
| 2028-01-03 | **Alpha 1, public: proposed, founder approval needed.** |
| 2028-03-27 | Sub-project 12 done. |
| 2028-04-10 | Plans for Alpha 3 (9, 10 and the additions to 5, 6 and 7) approved (target). Companion trust-model and payment ADRs reviewed; the Q12 legal read back. |
| 2028-06-05 | Alpha 2 builds end (13 and 14 done). Alpha 3 builds start. |
| 2028-06-19 | **Alpha 2: proposed, founder approval needed.** |
| 2028-08-07 | Plans for Alpha 4 (11) approved (target). Voice and recording-retention ADRs reviewed; the Q11 legal read back. |
| 2028-10-02 | Alpha 3 builds end. Alpha 4 builds start. |
| 2028-10-16 | **Alpha 3: proposed, founder approval needed.** |
| 2029-01-15 | Alpha 4 builds end. |
| 2029-01-29 | **Alpha 4: proposed, founder approval needed.** |

Alpha 4 runs longer than its 29 tasks suggest because sub-project 11 is one sub-project, and at most 2 builders work on one sub-project.

### Range by pace

| Pace (merged tasks a week) | Alpha 1 | Last wave (Alpha 4) |
|---|---|---|
| 1.1 (SP2a's average) | week of 2030-04-01 | about 2033-05 |
| 2 | week of 2028-10-09 | about 2030-06 |
| 4 (SP2a's best week; the proposal) | week of 2028-01-03 | week of 2029-01-29 |

The 1.1 and 2 rows use the same order. Their later-wave dates are the remaining 179 tasks at that pace, plus two release weeks.

**What else moves the dates:**
- Founder gates: plan approvals, the legal reads (Q11, Q12) and the design-partner annex (Q5).
- Alpha 3's outward parts (sends as a person, payments) and Alpha 4's calls and live capture cannot ship before their legal reads.
- If the founder wants an early slice of a later wave (Q22), it takes builder weeks from Alpha 1.

**Cut order.** Inside Alpha 1, §18's cut order (WhatsApp and Teams, then Capacitor, then 3D) still stands as written. This re-plan proposes no cut across the waves.

## Differentiation

The founder asked on 2026-11-26 for KatWork to outperform other products, through research into what people want and need, across many fields. A first research wave read published studies on office work, trust and delegation with AI, and the Thai work context, and coded public complaints about AI tools in English and Thai. Its findings feed each sub-project's plan. Nothing here changes the scope or the dates above. A finding that would change scope comes to the founder as a proposal.

**Limits of the evidence.**
- Every count comes from one coder.
- The Thai first-person voice is thin: a few dozen public forum posts, with no replies.
- No source measures willingness to pay.
- Satisfied users rarely post.

The ranking is therefore ordinal, and needs that sit next to each other should be read as ties.

### Unmet needs, ranked

The order follows how many independent sources support each need, how intense the complaints are, and how close the need is to KatWork's mission.

| # | What people need | KatWork's design response | Owner |
|---|---|---|---|
| 1 | **No silent wrong numbers.** Wrong output that looks right is the most damaging failure and the least noticed. People want every figure checked against a source they can open. | Every number, date and quote in a draft is marked passed, failed or unverified. A "check these first" list opens the report, and the missing-evidence list sits on the cover. A failed or unverified claim blocks acceptance of any section the template marks as required evidence. Seeded-defect test worlds cover every alpha template. | 2a (verifier), 3 (acceptance, test worlds), 6 |
| 2 | **Data stays in the building.** People want a plain answer to "where does my file go". This is a top fear in organisations, and PDPA enforcement is real. | One screen per Space showing which model providers see it, what each outbound model call sent, and what never leaves. A personal-data flag on a folder sends its jobs to a local model. A redaction check runs before any outward send. An offline-install test runs in CI. | 1, 2b (egress gateway), 3 (redaction), 6 |
| 3 | **Review that costs less than redoing the work.** Checking AI output often eats the time it saved. | Claims ordered by risk: failed first, then unverified, with passed claims collapsed to a count. One click opens the source at the exact span, highlighted. Reviewer minutes per deliverable are logged from use, not taken from surveys. | 2a (evidence card), 6 |
| 4 | **Right in the person's own file and template, including Thai print.** Spreadsheets and slides come out worst, and Thai numerals, tone marks and fonts break. | The deliverable opens in the office's own template, reads back, and prints the same on two renderers. Each template declares its date era (Buddhist or Common) and digit form (Thai or Arabic), and a check fails mixed forms. | 2a (writers), 3 (template fill, render check) |
| 5 | **Approvals that catch errors.** Approval prompts decay into rubber stamps, and blame lands on the nearest person. | Approval cards offer approve, approve with a comment, unsure, and decline. High-risk claims ask the reviewer to open the cited span first. Each job has a budget of cards. The audit row stores exactly what the card showed. Seal-and-initial is an approval type. An alarm fires when approvals become too fast and too uniform. | 3, 5 (cards per channel), 6 |
| 6 | **An honest "done".** Agents claim completion they did not achieve. | A completion receipt generated from the job log covers inputs read against the input list, checks run and not run, what was tried and rejected, stops and their reasons, and time and tokens from the model-call records. A skipped file or a run that matched zero records is a failed check. | 1 (records), 3 (completion record), 2a (reader coverage) |
| 7 | **Staying in control.** People want partnership, not full automation. | A clarifying card when the brief lacks audience, template or period. A stop that works within seconds. Take-over of a Kat's browser or app. Redo of one section without restarting. Every outward or destructive action waits as a Proposal. | 3, 4, 6, 2b |
| 8 | **Thai official-document correctness.** This covers Buddhist-Era dates, numerals, register, the official letter form, and scanned input. | Official letter and memo forms with a register check. OCR lines below confidence are flagged, not trusted. Pulling an official memo template from Phase 3 forward is a founder question (Q26). | 2a (`katwork-thai`, OCR), 3, 4 |
| 9 | **Reconcile across files and show only the exceptions; the same figure everywhere.** | Reconciliation results in two parts: matched (collapsed) and exceptions (both sources side by side). In a pack, every output draws each figure from one verified fact. | 3 (monthly pack), 2a (cell facts) |
| 10 | **Predictable cost.** People want an estimate first, no silent burn, and no silent switch to a weaker model. | An estimate of tokens and time before a team job, and the running cost during it. A per-workspace cap. A model without a price shows "unpriced", never zero. No silent model downgrade. | 1 (gateway), 4, 6 |
| 11 | **Memory of facts and decisions, with their reasons, across a long job.** | A fact register and a decision log (decision, reason, rejected options, who, when) that later tasks and jobs reuse. Space memory accepts an entry only through a person or a policy. | 2c, 2a, 4 |
| 12 | **An easy start for non-technical staff.** | A chat-first first run (LINE too), with one visible next action. The 3D office optional. A one-page glossary. | 6, 5 |

### Five claims to test and publish

These are targets, not measurements. KatWork will test each in its own CI or in a timed internal run, and publish both the method and the result.

1. On KatBench's seeded-defect worlds, no planted wrong figure, unit mix or date-era swap is ever marked as passed.
2. A reviewer opens the exact source span behind any claim in under 10 seconds.
3. A job's completion receipt is generated from its log, and no statement in it is contradicted by that log. Skipped files and zero-match runs fail.
4. An offline install makes zero outbound connections, and no outward action runs without a person approving that exact action.
5. Every figure in a monthly pack is identical across all its outputs, and Thai dates and numerals follow the template's declared form with zero violations.

Claims about how reviewers behave, for example their catch rate with and without the proof panel, need real reviewers. They are design-partner measurements in the dry runs, not launch claims.

### Adopted areas the evidence does not yet support

The research found no supporting evidence for these areas, and some evidence of risk: privacy fears, reluctance to hand over credentials, and third-party consent. They are **kept per the founder's ruling; evidence to gather**. None is dropped. Each already sits in a late wave and behind its legal read.

| Area | Wave | Evidence to gather |
|---|---|---|
| Payments (sub-project 10) | Alpha 3 | Whether offices want Kats to pay, and for what; the rails question (Q9) |
| Outbound phone calls (sub-project 11) | Alpha 4 | Office demand; the Thai legal read (Q11) |
| Local desktop control (sub-project 9) | Alpha 3 | Which tasks people would hand over on their own computer |
| Personal data from apps on a person's own computer (sub-project 9) | Alpha 3 | Demand among LINE-first and Windows users (Q13) |
| Teach recording (additions to 2b) | Alpha 2 | Whether people would record tasks, and the recording rules |

## Alpha (#138)

**Date:** the spec's §1.1 heading reads "Goals for the alpha (2027-01-31)", with the public alpha in the week of 2027-01-25 (§17). That date cannot hold. Under the staged alphas, this section describes **Alpha 1**, the first public alpha, with the current spec scope. "Re-plan: staged alphas" proposes the week of 2028-01-03 for Alpha 1 at 4 merged tasks a week, and dates for Alphas 2 to 4. Every date is proposed and needs the founder's approval (Q1). Each later wave repeats the release work and entry criteria below for its own scope, plus the gates of its ADRs and legal reads.

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
8. The human-labelled Thai claim set from real documents, which the 2a plan defers, is built before the design-partner dry runs (week of 2027-01-11). No sub-project owns it yet.

**Fallbacks in §18:** for Thai document fidelity, a narrower supported-feature list; if the Craft track slips, the forks the demo does not use merge after it, and their apps stay watchable through previews.

**Cut order if a week slips twice** (§18): WhatsApp and Teams first, then Capacitor, then 3D (the board stays).

**Not in the alpha** (spec §1.2 as written): KatsAgentOS, a VM per Kat for code, voice and video, federation, native Linux binaries inside a Kat's computer, a streamed Craft desktop, high availability, a hosted cloud, unofficial WhatsApp Web, and GPL or AGPL code in the core.

This list holds for Alpha 1. The expanded scope collides with some of these items in later waves. The scope amendment proposes moving "voice and video calls" into Alpha 4 (sub-project 11). It lists the other collisions as open questions: native binaries, the streamed Craft desktop, the hosted cloud and multi-host scheduling. Until the founder approves the amendment, this list stands.

## After the alpha

The spec names, without assigning them to sub-projects:
- federation (sub-project 8);
- KatsAgentOS, built by the lab (§14);
- the Enterprise edition features (§16);
- a hosted cloud (§16);
- the Phase 2 to 4 use cases (§8): PPTX output and Thai OCR at scale (Phase 2), a tracked-changes engine and scheduled browser research (Phase 3), and a creative Kat (Phase 4).

## Open questions for the founder

Nobody has ruled on these. This roadmap does not decide them.

1. **The alpha dates.** The spec's §1.1 heading gives 2027-01-31, with the public alpha in the week of 2027-01-25; that cannot hold. The founder accepted staged alphas on 2026-11-26. At 4 merged tasks a week (SP2a's best week), "Re-plan: staged alphas" proposes:
   - Alpha 1: week of 2028-01-03.
   - Alpha 2: week of 2028-06-19.
   - Alpha 3: week of 2028-10-16.
   - Alpha 4: week of 2029-01-29.

   At 2 a week, Alpha 1 is the week of 2028-10-09. At SP2a's 1.1 average, it is the week of 2030-04-01. Which dates and pace does the founder approve? Recommended: approve them as targets and re-forecast at the pace review in the week of 2027-01-25.
2. **Routing before memory.** Sub-project 4 carries a recall tool and a memory byte cap (§19 item 4), which need 2c's memory, but §17 runs routing in the weeks of 2026-11-23 and 2026-11-30 and memory in the week of 2026-12-28. Does sub-project 4 wait for 2c, build those two parts after 2c lands, or does 2c move earlier?
3. **Sub-project 7 and the alpha.** §19 lists sub-project 7 before federation, and only federation is marked "after the alpha". §17 gives sub-project 7 no week, and §1.2 defers the streamed Craft desktop to it. Is sub-project 7 required for the alpha, partly required, or after it? The staged-alpha ruling, as relayed, places sub-projects 1 to 7 in Alpha 1, so this re-plan counts sub-project 7 there. Recommended: confirm that reading, which closes this question.
4. **Templates.** All six templates are alpha goals (§1.1 goal 8), and §18's cut order names none, so cutting one would be a new founder decision. Two things are open. First, which sub-project builds each of the research brief, SAR, One Report, monthly pack and HA rubric pack: all in sub-project 3 (§19 item 3), or each with the sub-project it first needs? Second, how their dependencies on later sub-projects are handled: their teams need sub-project 4's planning and delegation, and the research brief needs 2b's saved web spans, while §19 lists sub-project 3 before both. Should any template be cut if the date in Q1 cannot hold?
5. **Design partners.** The dry runs (week of 2027-01-11) and any partner samples need a signed data annex (§15). Who are the partners, and when is the annex signed?
6. **After the alpha.** How do the Phase 2 to 4 use cases (§8), the Enterprise features (§16) and KatsAgentOS (§14) map onto sub-projects or plans, and in what order relative to federation? The expanded scope pulls two Phase capabilities into the alpha (scheduled browser research from Phase 3, through sub-project 13; image and video generation from Phase 4, through sub-project 12). The rest of this question is still open.
7. **Plan-item closure.** §6.4's plan-item closure (a lead-Kat structured check plus the registered checks, driving the live graph and the office view) is not assigned to a sub-project in §19. Does it belong to sub-project 4 (lead-Kat planning) or sub-project 3 (registered checks)?

### Questions from the expanded scope (2026-11-26)

The rulings settle that all of the new scope is product scope, delivered in alpha waves 2 to 4. They do not settle these. Each has a recommended option; none is decided here.

8. **Sub-project split.** Should the new scope be six new sub-projects (9 to 14), or should some fold into existing ones, for example team admin into 3 and 6? Recommended: six new sub-projects. Each has its own ADRs and legal gates, and separate plans keep each one reviewable.
9. **Payment rails.** A self-hosted install has no merchant relationship of its own. Which rails does it use? Options: (a) each install brings its own payment provider account and keys, like model keys today; (b) only a KatWork-run service issues cards, which needs the hosted cloud; (c) no card issuing, and Kats prepare checkouts that a person pays. Recommended: (a) for Alpha 3, with (c) as the fallback for installs without a provider.
10. **Editions.** Which new features are free core and which are Enterprise or Cloud (§16)? This covers credits and top-ups, approval rules, the payment second factor, the team marketplace, service accounts and the admin console. Recommended: approvals, gates and spend limits stay in the free core, because safety should not cost extra. Rules engines, the marketplace, service accounts and credits follow §16's existing split: policy rules and admin to Enterprise, credits to Cloud.
11. **Legal reads: phone calls and meeting recording in Thailand.** Outbound AI calls to third parties and recording other people need a legal read under Thai law (consent, PDPA, telecom rules) before any build of those parts. Recommended: request both reads now. Build the parts that need no read (voice chat with your own Kat, dictation, minutes from an uploaded recording) first, and keep calls and live meeting capture off until the reads pass, as Typhoon OCR is off until its terms pass.
12. **Legal read: acting on a person's behalf.** Sending messages and mail as a person, and paying, raise consent and liability questions. Recommended: request the read with Q11. Every such action stays a Proposal a person approves until the read and the ADRs land.
13. **Apple-only features against LINE-first Thai users.** The competing product's personal-data tools are mostly macOS-only (Messages, Notes, Reminders, Photos). Most Thai office users live in LINE and on Windows or Android. Recommended: the companion targets Windows and macOS through the Tauri shell. Personal data comes through cross-platform connectors first (Google and Microsoft mail and calendar, LINE through sub-project 5). Apple-only tools come last in sub-project 9's plan.
14. **Connector priorities.** Which named connectors come first? Recommended: those Thai offices use most. Mail and calendar (Google, Microsoft 365), file stores, LINE OA, then the rest by design-partner demand.
15. **Voice languages and engine.** Must voice work in Thai at alpha quality, or English first? Recommended: Thai and English both. The engine is qualified by a gate on Thai office speech, as ADR-0008 did for OCR, with a local engine option for local-only Spaces.
16. **Companion platforms.** Windows, macOS, Linux, Android, iOS: which does Alpha 3 cover? Recommended: Windows and macOS desktop first, mobile notifications only.
17. **Shell inside a Kat's computer.** The competing product gives its agent a full shell. §1.2 rules out native Linux binaries in a Kat's computer for the alpha. Recommended: keep the argv-only agentOS exec in every alpha wave. Stdio MCP servers and plugin commands run as pinned containers. A full shell waits for KatsAgentOS.
18. **Stealth browsing and egress through a person's machine.** The competing product disguises its browser as a person's and can route traffic out through the user's home connection. Both conflict with ADR-0009's single egress gateway and with Web Bot Auth signing. Recommended: do not adopt either. Record both as rejected alternatives in ADR-0009.
19. **Telemetry.** The competing product sends telemetry by default. §13 says no telemetry unless an admin turns it on. Recommended: keep §13 as written.
20. **Link previews and rich cards from tool results.** ADR-0010 renders model text as plain text, with no link previews and nothing fetched from a reply. Recommended: no link previews. Tool-result cards only in a sandboxed renderer with no network, with an ADR-0010 amendment, in sub-project 6.
21. **Unattended actions.** Standing grants to send as a person, an admin switch that hides a Kat's screen while it works, and a model classifier that approves actions on its own all appear in the competing product. Recommended: standing grants only bound to one recipient and content class, with caps. Hiding the screen keeps the activity log and the take-over path. A classifier may narrow permissions but never approve; code decides (§5.1, ADR-0010).
22. **An early look at the new scope.** In the proposed order, the new sub-projects come after Alpha 1 (builds from the week of 2027-12-13). Does the founder want an early demo slice of one of them, for example voice chat or one connector, before the core is done? Recommended: no. The core sub-projects are dependencies of all of them, and an early slice would take builder weeks from them.
23. **Private worker pools.** The competing product can run its agents on a customer's own pool of machines. For KatWork that means multi-host scheduling, a §1.2 non-goal. Recommended: keep the single-host alpha and place private pools after it, because they are a hosting mode, not a capability a person uses. This needs the founder's ruling, because the rulings make it product scope.
24. **A phone as a tool host.** The competing product can use a person's phone for photos, location and similar tools. Recommended: include it in sub-project 9 after the desktop companion, with photos and location behind PDPA grants (§13). If Q16 keeps mobile to notifications only, this moves after the last alpha wave, which needs the founder's ruling.
25. **Choices a Kat makes for you.** Another pattern from the research lets an agent pick a default when it would otherwise wait, record why, and let the person review it later. Spec §6.4 says "a timeout never picks defaults" for clarifying questions, so this conflicts with the spec as written. Recommended: allow it only for reversible choices inside a job, for example which of two sheet names is the budget. Each such choice is listed on the completion receipt and the approval card, with its reason, and a template may forbid it. Never allow it for a clarifying question (audience, period, template, recipient), for anything outward or destructive, or for anything that changes a claim's source. This is a proposal, not a decision.
26. **An official letter and memo template in an alpha.** Thai official-document correctness is a ranked need (see "Differentiation"), but the official memo factory is a Phase 3 use case (§8). Should a letter and memo template move into an alpha wave? Recommended: yes, into Alpha 2. It reuses the template engine and `katwork-thai`, and its date and numeral checks are claim 5.
