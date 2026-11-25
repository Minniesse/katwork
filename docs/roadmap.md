# KatWork roadmap

This document lists every KatWork sub-project from the design spec (`docs/spec/katwork-design.md` §19), what each one covers, what it depends on, its rough size, the order of work, and the entry criteria for the public alpha. It is a map, not a plan. Each sub-project gets its own detailed plan, written and reviewed one wave ahead of its build, and the founder approves each plan before that build starts (spec §19.1).

Where this roadmap and the spec or an approved plan disagree, the spec and the plan win. Scope listed here comes only from the spec, the approved plans and the ADRs in `docs/adr/`. Anything nobody has ruled on is marked as an open question and collected at the end.

## Sizes

Sizes count the tasks in a sub-project's plan:

| Size | Plan tasks |
|---|---|
| S | 1 to 8 |
| M | 9 to 20 |
| L | more than 20 |

Two sizes are counted from approved plans. The sub-project 1 plan has 17 task headings (Tasks 1 to 14, plus 3b, 7b and 13a/13b), so it is M. The sub-project 2a plan has 44 tasks, so it is L. Every other size is an estimate from the spec's scope and carried requirements, and the sub-project's own plan replaces it. These letters are not the S/M sizes inside the sub-project 2a plan, which measure worker time for a single task.

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
| 6 | Views and shells | #135 | Not planned | M (estimate) | Not written |
| 7 | Craft integration beyond 2a | #136 | Not planned | M to L (estimate) | Not written |
| 8 | Federation, after the alpha | #137 | Not planned | L (estimate) | Not written |
| Alpha | Public alpha release | #138 | Not started | S to M (estimate) | Not written |

## Order of work

Two sources give the order, and they do not fully agree.

**Spec §19 (build order).** The sub-projects are listed in build order, and each depends only on the ones before it: 1, then 2 (2a, 2b, 2c), then 3, 4, 5, 6 and 7. Sub-project 8 comes after the alpha.

**Spec §17 (weekly milestones).** The week-by-week schedule runs several sub-projects side by side:

- agentOS computers and the browser supervisor (2b) sit in the week of 2026-11-09, beside 2a's document tools.
- The approval host, the policy function and statuses (3) sit in the week of 2026-11-16, before 2a ends.
- Laya in shadow mode and Stage 0 routing (4), the LINE and Slack adapters (5), and the tasks-and-files board (6) sit in the week of 2026-11-23.
- The memory stores and the RustShare connector (2c) sit last among the builds, in the week of 2026-12-28.
- The sub-project 2a plan has its own week table, which ends with OCR in the weeks of 2026-12-21 and 2026-12-28.

**How plans are written.** The plans for 2b, 2c and 3 are written and reviewed while 2a builds. Each later plan is written one wave ahead of its build.

Whether builds overlap as §17 shows or run strictly in §19 order is an open question (Q2 below).

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

**Status when this roadmap was written:** Tasks 1, 2, 3 and 8 had merged. Tasks 4 and 5 were in review.

**Outstanding founder actions** listed in the plan (they gate work but are not design questions):
- the legal read of the Typhoon OCR terms;
- the legal read of which public Thai documents are usable;
- the start of the two Thai reviewers, and approval of the annotator programme;
- permission to publish the Craft image and keep the forks public;
- the Craft licence notes.

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
- The rest of the carried requirements in spec §19 item 2, which cover the sidecar lifecycle, argv process APIs, quotas, package pinning and the real-Chromium CI lane.

**Entry gate, first:**
- The inventory of commands the alpha templates need inside agentOS.
- A CI conformance test proving guest code cannot reach host files, host processes, the network or another Kat, for every enabled runtime.

Both feed ADR-0009's kill criteria for switching the code tier to gVisor. KatsAgentOS, the in-house sandbox that replaces agentOS, is a separate track after the alpha. Sub-project 2b builds only the agentOS adapter and the `Computer` contract.

**Depends on:** sub-project 1. It also uses 2a's artifact store for browser downloads, through a migration that relaxes 2a's "inputs are uploaded by a person" check.

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
- Proposals with expiry, precondition digest, revision and states.
- An approve endpoint that executes once per tool call.
- Action types from KatWork's registry.
- Approval cards rendered by code.
- An audit log.
- The policy function and job statuses.
- Resume for `needs_input` jobs and for approvals in one change, replacing sub-project 1's follow-up jobs.
- Acceptance on top of 2a's evidence verifier: zero failed claims, and every claim in the required-evidence scope passed.
- A contract hash that stops the job when the contract changes.
- The same-Kat rule, which caps a job at `ready`.
- Replay and kill-switch tests.
- Templates and their acceptance contracts.
- Template filling, after a fill-engine bake-off; editing existing workbooks; lopdf's constrained PDF edits.
- The render check, advisory until it passes the §9.1 gate.
- Taint-aware gates, which use the trust flags 2a records on every tool call.
- Real-model injection runs, seeded from KatBench-Inject.
- The KatBench SAR and One Report worlds, with the templates.

The spec puts templates in this sub-project, but it does not say which of the six alpha templates (spec §8) are built here or are needed for the alpha (Q4).

**Depends on:** 2a (the evidence verifier, evidence reports and trust flags). Templates that read scans need 2a's OCR.

**Size:** L (estimate).

### 4. Routing (#133)

**Scope** (spec §6.2 to §6.4, §19 item 4):
- Stage 0 in code: approval callbacks, `@mentions`, the closed set of eligible Kats, Thai script detection, an `addressed` flag, per-channel trigger modes, dropping bot messages, and a hop cap on `KatEnvelope`.
- Laya in Rust from a checksum-pinned bundle, in shadow mode until each language passes the 50-case calibration gate.
- The 200-case calibration set and its preregistered protocol.
- Lead-Kat planning: a validated DAG of at most 5 tasks and 3 levels, with exactly one sink task.
- Delegation through `KatEnvelope`.
- Context and provider-quota handling.

The 2a plan notes that the minutes team (Writer, Scheduler, Reviewer) needs this delegation; the 2a demo runs one Writer.

**Depends on:** sub-project 1. The spec's list order places it after sub-project 3.

**Size:** M (estimate).

### 5. Channels (#134)

**Scope** (spec §11, §19 item 5):
- One adapter boundary.
- Seven adapters: LINE, Slack, Teams, RustChat/Mattermost, Telegram, Discord, and WhatsApp through the official Cloud API.
- The documented tunnel recipe for webhook channels behind NAT.
- Approval cards and clarifying questions in each channel.
- The channel tests in spec §15, including `two_bots_in_one_channel_do_not_loop`.

**Depends on:** sub-project 1. Approval cards need sub-project 3's approval host, and clarifying questions need sub-project 3's resume.

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

**Depends on:** sub-project 1's event stream; 2a's Craft previews and panel; 2b's browser screencast.

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

## Alpha (#138)

**Date:** the spec gives "by 2027-01-31" (§1.1), with the public alpha in the week of 2027-01-25 (§17). Issue #138 says the date follows the sub-projects it needs. Whether that date still holds is an open question (Q1).

**Work that belongs to the release itself.** No §19 item owns this work:
- Hardening: the kill-switch, replay and fidelity gates.
- Demo mode.
- Docs.
- The HA self-assessment rubric pack with its redaction check.
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
3. Laya's 50-case routing benchmark is the release gate (§15). A language that fails it stays in shadow mode, and the lead Kat routes (§6.3, §18).
4. No Kat runs code in agentOS until the CI conformance test passes for every enabled runtime (§13, ADR-0009).
5. Each Craft app or operation that fails its §9.1 gate is forbidden in that app's policy. A failing optional app never blocks a template, but a template whose Rust baseline fails its Thai gate is blocked (§9.1).
6. Demo mode runs with no keys, and its canned decisions are labelled synthetic. A live demo never depends on an external login (§15, §18).
7. The 2a plan's legal reads come before any customer deployment uses the parts they gate. Until the Typhoon OCR terms pass, Typhoon OCR stays off.

**Cut order if a week slips twice** (§18): WhatsApp and Teams first, then Capacitor, then 3D (the board stays).

**Not in the alpha** (spec §1.2): KatsAgentOS, a VM per Kat for code, voice and video, federation, native Linux binaries inside a Kat's computer, a streamed Craft desktop, high availability, a hosted cloud, unofficial WhatsApp Web, and GPL or AGPL code in the core.

## After the alpha

The spec names, without assigning them to sub-projects:
- federation (sub-project 8);
- KatsAgentOS, built by the lab (§14);
- the Enterprise edition features (§16);
- a hosted cloud (§16);
- the Phase 2 to 4 use cases (§8): PPTX output and Thai OCR at scale (Phase 2), a tracked-changes engine and scheduled browser research (Phase 3), and a creative Kat (Phase 4).

## Open questions for the founder

Nobody has ruled on these. This roadmap does not decide them.

1. **The alpha date.** The spec says "by 2027-01-31", with the public alpha in the week of 2027-01-25. The 2a plan put the meeting-minutes demo in the week of 2026-11-30. When this roadmap was written, 4 of 2a's 44 tasks had merged. Does the date hold, move, or hold with §18's cut order (or a wider cut) applied?
2. **Build overlap.** Spec §19 says each sub-project depends only on the ones before it, while §17 builds 2b, 3, 4, 5 and 6 work beside 2a and puts memory (2c) last. Do later sub-projects start building while 2a is still open, and if so, which ones?
3. **Sub-project 7 and the alpha.** §19 lists sub-project 7 before federation, and only federation is marked "after the alpha". §17 gives sub-project 7 no week, and §1.2 defers the streamed Craft desktop to it. Is sub-project 7 required for the alpha, partly required, or after it?
4. **Templates.** Are all six alpha templates (§8) required for the alpha, or may some be cut? §18's cut order names channels, Capacitor and 3D, not templates. Which sub-project builds the research brief, SAR, One Report, monthly pack and HA rubric pack: all in sub-project 3, or each with the sub-project it first needs?
5. **Design partners.** The dry runs (week of 2027-01-11) and any partner samples need a signed data annex (§15). Who are the partners, and when is the annex signed?
6. **After the alpha.** How do the Phase 2 to 4 use cases (§8), the Enterprise features (§16) and KatsAgentOS (§14) map onto sub-projects or plans, and in what order relative to federation?
