# KatWork: the self-hosted AI team office — design spec

| | |
|---|---|
| Date | 2026-10-22 |
| Status | Approved for the alpha (2026-10-22) |
| Maintainer | @Minniesse |
| Milestone | Public alpha by **2027-01-31** |
| Inputs | Design interviews and internal research (not published) |

## 1. Summary

KatWork lets anyone in an organization hire a team of named AI coworkers ("Dots") that do real office work on their own isolated computers and hand back **finished files with proof**. People talk to the team from LINE, Slack, Teams, RustChat/Mattermost, Telegram, Discord, WhatsApp, the web, desktop or mobile, and they watch the work in a cute 3D office and a live graph of every agent call. Anything risky waits for a human to approve. The whole system runs **on the organization's own servers**, with any model provider, in English and Thai.

**Positioning:** KatWork is *the self-hosted AI team office*, for organizations of any size. It turns folders of everyday files into finished, source-cited documents through a persistent team of named coworkers, with human approvals. It differs from:
- hosted coworker products such as OpenAI ChatGPT Dots, which run in the vendor's cloud;
- open-source coworker templates such as CopilotKit OpenDots, which leave hosting, approvals and proof to the adopter;
- chat assistants, which return text rather than finished, checked files.


### 1.1 Goals for the alpha (2027-01-31)

1. A self-hosted install that starts with `docker compose up` on one Linux server.
2. A default team (Lead, Researcher, Writer, Analyst, Scheduler, Reviewer) that people can rename, restyle and extend.
3. Messages from every listed channel, routed in milliseconds to one Dot or to a planned team job.
4. Real work on real computers: files read and written, web research, documents produced in the customer's template.
5. Proof on every deliverable: each claim *passed*, *failed* or *unverified*, plus a missing-evidence list.
6. Deterministic approvals for anything outward or destructive.
7. Four views: 3D office, live agent graph, chat, and a tasks-and-files board.
8. Six showcase templates (§8): four chosen, plus two from the use-case research.
9. Two KatWork installs linked by federation (§12). This is the first item cut if the schedule slips (§18).
10. Login with password reset and demo accounts, so a live demo never depends on an external login.

### 1.2 Non-goals for the alpha

- KatWork's own agentOS; the alpha uses rivet agentOS (§14 lists what the in-house agentOS must provide).
- A VM per Dot. Per-Dot microVMs come with the in-house agentOS.
- Voice and video calls.
- High availability or multi-host scheduling.
- A hosted cloud offering. The paid cloud comes after the alpha (§16).
- Unofficial WhatsApp Web, and any GPL or AGPL code in the core.

## 2. Decisions

| Area | Decision |
|---|---|
| Product shape | A general AI team for any office work. Templates showcase jobs; they do not limit them. |
| Team model | Persistent "Dots", like OpenAI ChatGPT Dots and CopilotKit OpenDots. Each has a name, role, personality, memory, its own computer, and a tool allowlist. |
| Edge | A team anyone can build · finished work with proof · a safe computer per agent · English and Thai together. |
| Beachheads | University report offices (SAR and annual reports) and listed-company One Report sustainability teams. KatWork itself stays horizontal. |
| Core language | Rust for all KatWork code. Agents may run non-Rust tools inside their sandbox. |
| UI | React and Vite in TypeScript, served by the Rust server. Tauri for desktop, Capacitor for mobile. |
| Platforms | Web, desktop and mobile, plus connectors for LINE, Slack, Teams, RustChat/Mattermost, Telegram, Discord and WhatsApp (official Cloud API). |
| Hosting | Self-hosted from day one, packaged with Docker Compose. |
| Licence | Open-core under Apache-2.0. No GPL or AGPL code in the core. |
| Models | Any provider; the admin chooses. Default to the strongest API model, with a local model for sensitive data. |
| Decision layer | Laya, multilingual checkpoint only, running inside Rust (ONNX via `ort`, or `laya-rust`), with separate calibration per language. Used for intake routing only. |
| Routing | A 4-stage routing design (§6) combined with the four JEVIS layers (§5). |
| Sandbox | rivet agentOS now, plus a shared headless-Chromium service. KatWork's own agentOS later, built with an infrastructure partner. |
| Office apps | The storytold Craft apps where they pass our gates (§9). docx-rs, umya-spreadsheet, pdf-extract, lopdf, Typst and OAR-OCR are the dependable base. |
| Memory | Built in: Postgres, Qdrant and Tantivy with nlpO3, per Dot and per team. Plus a RustShare connector. |
| Views | 3D office (react-three-fiber with Kenney and Quaternius CC0 assets), live agent graph, chat, tasks-and-files board. |
| Federation | In the alpha: a selective fork of reason-network with our own identity and relay (§12). |
| Revenue | Free core, paid Enterprise edition, and later a paid hosted cloud. |
| Name | KatWork, "the self-hosted AI team office". Repo `Minniesse/katwork`. |
| Team | A solo maintainer builds the app with AI coding agents orchestrated by Orca. An infrastructure partner owns the future in-house agentOS. |
| Development loop | Approved spec, then small GitHub Issues labelled `ready-for-agent`, then Orca workers in worktrees with TDD. The model that didn't write a PR reviews it (§19.1), the founder approves every PR, CI must be green, and there is a weekly demo. |
| Tracking | GitHub Issues for engineering; the Notion KatWork board for phases. |

## 3. Users and their jobs

- **Who uses it:** non-technical staff who own recurring document work. For example: planning and QA officers, sustainability and IR teams, hospital quality staff, school administrators, agency account managers and NGO program staff.
- **The job that recurs everywhere:** turn a folder of evidence into a finished, cited document by a deadline. Accuracy is the top fear, and "AI drafts, a human checks" is already a trusted pattern.
- **The trust line in the product copy:** "AI ร่างและตรวจสอบเบื้องต้น — คุณตรวจทานและอนุมัติ" ("AI drafts and does a first check — you review and approve").
- **Words to use:** "ผู้ช่วย AI" (AI assistant), "ทีม" (team), "ห้องทำงานทีมผู้ช่วย AI" (AI-assistant team office).
- **Word to avoid:** "พนักงาน AI" (AI employee), which carries a layoff connotation.

## 4. Domain model

| Concept | Meaning |
|---|---|
| **Workspace** | One self-hosted install's tenant: an organization or unit. It owns members, model settings, budgets, connectors and federation links. |
| **Space** | A shared area inside a workspace, such as a faculty office or an ESG team. It holds files, pages, memory and the Dots allowed to work there. |
| **Dot** | A persistent AI coworker with a name, role, personality, instructions, memory, tool allowlist, model tier, autonomy level and its own **Computer**. Created from a role template or "hired" with a short form. |
| **Team** | A lead Dot plus specialists. The team saved on the server **decides who does the work**; it is not decoration. |
| **Thread** | A conversation between people and Dots in any channel. Each thread is mapped to one KatWork thread id. |
| **Job** | One request, from intake to final status. It owns the plan, the runs, the evidence and the verdict. |
| **Task** | A bounded piece of a job assigned to one Dot: inputs, acceptance conditions, a lease and retry limits. A job's tasks form a DAG of at most 5 tasks and 3 levels. |
| **Run** | One Dot executing one task: steps, tool calls, tokens, cost and an event stream. |
| **Computer** | A Dot's sandbox: an agentOS workspace, an optional browser profile, and app access (§9). |
| **Artifact** | A versioned output file or page. Originals are immutable and every new version is written atomically. |
| **Evidence** | A source span (file, page, cell, URL, retrieval time, content hash) linked to a claim. |
| **Claim** | A statement in an artifact, such as a number, date or fact, with status *passed*, *failed* or *unverified*. |
| **Proposal** | An exact pending side effect (action, parameters, target, digest) waiting for approval. It is invalid if anything changes. |
| **Template** | A showcase job definition: the output format and template file, the team, the questions it asks, and its **acceptance contract** (§7.3). |
| **Peer** | Another KatWork install linked through federation (§12). |

**Job statuses** (from JEVIS):

| Status | Meaning |
|---|---|
| `accepted` | The template's preset acceptance contract passed against the current artifact version. |
| `ready` | A result exists, but there was no independent acceptance, for example because the checks were produced by the same Dot. |
| `answered` | A read-only answer. |
| `needs_input` | Waiting on a person. |
| `stopped` | A rule fired; the job carries a named reason and a way to carry on. |

`cancelled` and `error` are set by the harness, outside the policy.

## 5. Architecture

### 5.1 Four layers

KatWork follows the JEVIS layering: **the decision model advises, the harness executes and observes, and the policy decides.** No layer can skip the next one.

| Layer | In KatWork | Rules |
|---|---|---|
| Executor (replaceable) | A Dot's agent loop: Rig tool loop, its model, and its computer | Never decides how a job ends. Its "done" is only a claim. |
| Harness | The Rust core: scheduler, task leases, document snapshots, registered checks, approval host, evidence verifier, event log | Owns execution and evidence. Runs checks as argv arrays, never through a shell. Snapshots artifacts before and after each check. |
| Decision model | Laya, multilingual, inside Rust | Answers typed questions over closed option sets that code builds. Writes no text and no code. Its uncertainty narrows permissions; it never widens them. |
| Policy | A thresholds table plus one pure Rust function that turns recorded facts into the next action or a verdict | No I/O, clock or model calls. It can be replayed from the log. |

### 5.2 Services (Docker Compose)

```
┌──────────────────────── katwork (one Linux host, docker compose) ─────────────────────────┐
│ katwork-core (Rust)                                                                         │
│   axum API + AG-UI SSE · serves React/Vite build                                            │
│   scheduler (Tokio, task leases, retries) · policy fn · approval host · evidence verifier   │
│   decision worker (Laya multilingual ONNX via ort, warm, bounded concurrency)               │
│   model gateway (Rig providers; capability + data-residency rules; quotas)                  │
│   channel adapters (LINE, Slack, Teams, RustChat/Mattermost, Telegram, Discord, WhatsApp)   │
│   federation bridge (reason-network fork, OIDC identity, self-run relay)                    │
│ dot-workers (Rust) ── agentOS sidecars (one workspace per Dot) ── document tools            │
│ browser-supervisor (Rust, chromiumoxide) ── chromium containers (one profile per Dot)       │
│ craft-apps (web builds / MCP servers that pass the gates, §9)                               │
│ postgres (jobs, tasks, proposals, events, memory metadata) · qdrant (vectors)               │
│ artifact volume (local or S3-compatible) · optional: mistral.rs local LLM                   │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
```

Vendor picks (one primary pick per role):
- **Core:** Rig (agent tool loop) · Tokio · Axum · SQLx on PostgreSQL · rmcp (MCP) · a2a-rs (external peers only) · openidconnect.
- **Sandbox and browser:** rivet agentOS Rust client and sidecar · chromiumoxide.
- **Documents:** docx-rs · umya-spreadsheet · pdf-extract · lopdf · Typst · OAR-OCR.
- **Search and Thai text:** Tantivy with nlpO3 · Qdrant · ICU4X.
- **Inference:** ort (Laya) · mistral.rs (optional local LLM).
- **Front end:** AG-UI (`ag-ui-core`) with our own React UI, not CopilotKit Intelligence.
- **Shells and art:** Tauri · Capacitor · react-three-fiber with drei · Kenney Furniture Kit · Quaternius Universal Base Characters.

### 5.3 What must stay in-house

These cannot be bought or vendored:
- Durable team orchestration (leases, retries, restart recovery without repeating side effects).
- The approval host.
- Credential and computer isolation.
- The evidence model and the citation verifier.
- Safe document editing (immutable originals, atomic versions, read-back validation).
- The accessible office view.
- Local-model qualification.
- Decision calibration.
- Federation trust.

## 6. From message to finished work

### 6.1 Flow

```mermaid
sequenceDiagram
  participant U as Person (LINE/Slack/web…)
  participant C as Channel adapter
  participant S0 as Stage 0 (code)
  participant L as Laya (Stage 1)
  participant D as Dot / Lead Dot (Stage 2)
  participant H as Harness + policy (Stage 3)
  U->>C: message + files
  C->>S0: normalized event (tenant, thread, sender, attachments, reply target)
  S0->>S0: approval callback? @mention? paused thread? build closed Dot set from ACLs
  S0->>L: typed questions (which Dot, single vs team, slots present, complexity)
  L-->>S0: probabilities + confidence
  alt confident single-Dot job
    S0->>D: dispatch to that Dot
  else team job or "no opinion"
    S0->>D: Lead Dot gets full distribution, plans ≤5-task DAG
  end
  D->>H: tool calls, drafts, claims
  H->>H: deterministic approval gate · evidence verifier (passed/failed/unverified)
  H-->>U: approval card when an outward action is proposed
  U->>H: approve / edit / decline (bound to exact proposal digest)
  H-->>U: finished files + proof + missing-evidence list, job status
```

### 6.2 Stage 0: code (under 1 ms)

- Approval callbacks go to the approval host.
- Threads waiting on a person resume their task lease.
- `@mentions`, in English or Thai (`@นักวิจัย`, "researcher"), go straight to that Dot if the person may use it.
- Code builds the **closed set of eligible Dots** from Space ACLs, so the model can never pick a Dot the person may not use.
- Thai script detection picks the language calibration table.

### 6.3 Stage 1: Laya, intake only

One forward pass asks:
- `target_dot`: a choice over the closed set, plus `none_suitable`.
- `execution_mode`: single Dot vs team.
- Narrow slot questions about the text: does it mention a file that isn't attached? Is a recipient or date missing?
- `complexity_tier`: picks the model tier.

Rules:
- **Thresholds are set per language** after temperature fitting on labelled data, with a 10× resampling check. They are expressed as `p_max` plus a margin of at least 0.15 between the top two options.
- Low confidence means "no opinion" and goes to the lead Dot. A missing answer is never agreement.
- **Uncertainty narrows permissions.** If the request's mode (produce or change files / read-only / answer) is uncertain, the job runs read-only or asks the user.

What early evaluation suggests (to be confirmed by the calibration work in §15):
- Routing accuracy was similar in English and Thai.
- English confidence was badly calibrated.
- Approval and numeric-proof questions failed, so they are not Laya's job.

### 6.4 Stage 2: fast path or team

- **Single Dot.** The expected share is about 70–80% of messages. The task goes straight to that specialist without waking the lead.
- **Team.**
  - The lead Dot gets the authorized agent cards, Laya's full distribution, each Dot's availability, and the cost per model tier.
  - It returns a constrained JSON DAG: at most 5 subtasks and 3 levels.
  - Rust validates the plan: every assignee is in the closed set and the graph is acyclic. Only then are task leases created.
  - Dots hand work to each other through typed `DotEnvelope` messages on the Postgres-backed bus.
- **Clarifying.** If a slot is missing, the Dot asks one short question with sensible defaults, then the job continues.
- **Plan-item closure.** Each plan item gets its own "is this shown closed by the work and the checks?" question. Open items go to the next attempt **by name**. They also drive the live graph and the office view.

### 6.5 Stage 3: deterministic gates

- **Approval gate (Rust, at the tool boundary).**
  - The gate classifies each action from the tool's declared action type and the target or recipient: read, draft, write in workspace, send, publish, pay, delete, share, or an external API call. Laya never decides this.
  - Outward and destructive actions become a **Proposal** with exact parameters and a digest.
  - A person approves that exact proposal from any channel or the web. Changed parameters need a new approval.
  - Permissions are re-checked immediately before the side effect.
- **Evidence verifier (Rust).**
  - It extracts every number, date and named figure from the artifact and normalizes them: Thai and Arabic numerals, พัน and ล้าน (thousand and million), percentages, Buddhist-Era years.
  - It checks arithmetic and rounding against the cited source span.
  - Each claim is recorded as *passed*, *failed* or *unverified*. Unverified claims make up the **missing-evidence list**.
  - A "done" from a Dot is a claim, never evidence.
- **Acceptance.**
  - A job is `accepted` only when the template's contract passes on the current artifact version.
  - The contract and its checks are hashed when the job starts. If they change during the job, acceptance is forbidden.
  - Checks written by the same Dot that produced the work cap the job at `ready`.

### 6.6 Limits and carry-on

- Each run has a step and token budget.
- Repairs per task are capped at 2.
- Wall-clock limits apply per task and per job.
- Loop detection stops repeats.
- Each tool call has a timeout.
- Every stop names its reason and offers a next step: run all cheap checks, ask the user, hand to the lead, or save the draft with its evidence report.

## 7. Proof, approvals and templates

### 7.1 Evidence model

- **Source spans:** each source span stores the file id and version, the page, sheet or cell, or the URL and retrieval time, a content hash and the exact text.
- **Claims:** each claim in an artifact links to one or more spans.
- **Reports:** the report a person reads is generated by code from these records. The model never invents the explanation.

### 7.2 Approval experience

- **Where cards appear:** in the channel the person is using (LINE Flex message, Slack Block Kit, Teams card, Telegram and Discord buttons) and in the web approval queue.
- **What a card shows:** the exact action, the target, and a preview or diff. Options are Approve, Edit or Decline.
- **Audit:** every decision is logged. Enterprise adds policy rules on top, for example two approvers for external sends.

### 7.3 Templates and acceptance contracts

A template defines:
- the output type (DOCX, XLSX, PPTX, PDF or page) and the customer's own template file;
- the default team and its roles;
- the questions it asks at the start;
- the evidence rules, for example that every number in section 2 must cite a sheet and cell;
- the **acceptance contract**: the registered checks that must pass (template placeholders filled, required sections present, every numeric claim passed or listed as missing, a successful read-back and render).

Templates are data. New ones need no code changes.

## 8. Alpha templates

| Template | Output | Team | Acceptance contract highlights |
|---|---|---|---|
| University SAR / annual performance report | DOCX in the office's template; criteria-mapped evidence table | Lead, Researcher (collects evidence), Analyst (KPI tables), Writer, Reviewer | Every indicator section present; every number cited to a cell or page; missing-evidence list mapped to CHE QA, AUN-QA or EdPEx criteria |
| One Report sustainability section (56-1) | DOCX in Thai and English, with an XLSX data appendix | Lead, Analyst (reconciles branch data), Writer, Reviewer | Totals reconcile with source sheets; year-over-year changes recomputed; conflicting figures flagged; bilingual sections in parity |
| Meeting minutes | DOCX formal minutes plus an action-item list | Writer, Scheduler (owners and dates), Reviewer | Every action item has an owner and date or is flagged; decisions quoted from the source |
| Research brief | DOCX or PPTX brief with cited sources | Researcher, Analyst, Writer, Reviewer | Every factual claim links to a saved web or file span; no uncited figures |

| Monthly management / board commentary pack (added after early evaluation) | DOCX bilingual commentary plus an XLSX variance and lineage table. PPTX deck comes after the alpha | Lead, Analyst (variance bridge, cross-file reconciliation), Researcher (explains the drivers), Writer, Reviewer | Every figure is identical across all outputs because they draw on the same verified fact ids; cross-file conflicts become an approval card; actual vs budget, month-over-month and year-over-year recomputed in code |
| Hospital HA Standard v6 self-assessment (added after early evaluation): a **rubric pack on the SAR engine, no code** | DOCX in the HA official form (`FM-ACD-088-01`) | SAR team | As SAR, plus a **patient-identifier (HN) and personal-data redaction check** in the Reviewer before any approval |

An early evaluation scored candidate office use cases. These two were added because:
- The monthly pack runs **every month** in every company, while SAR and One Report are yearly.
- HA self-assessment has the same structure as a university SAR. Together they reuse the alpha engine for about 3–5 extra days of work.

The roadmap after the alpha, from the same early evaluation:

| Phase | Timing | Use cases | New capability |
|---|---|---|---|
| Phase 2 | Feb–Apr 2027 | Procurement TOR and reference-price matrix · grant proposal, NRIIS budget and pitch slides · audit PBC chasing and tie-out · school e-SAR · NGO donor report · ITA disclosure binder · survey synthesis | PPTX output, Thai OCR at scale |
| Phase 3 | May–Aug 2027 | Tender response · bilingual policy translation with tracked changes · agency client report · ISO management review · training-course kit · BOI pack · official memo factory · competitor digest · M&A diligence matrix | Tracked-changes engine, scheduled browser research |
| Phase 4 | Sep–Dec 2027 | Onboarding pack · events team · academic promotion dossier · bank-statement-to-tax pack · LINE OA replies with insights · PDPA RoPA · content calendar with visuals and video · feasibility memo · safeguarding portfolio · crisis fact pack | Creative Dot (PhotoCraft, DesignCraft, FilmCraft), opt-in with cost caps |

## 9. Dot computers and office apps

The alpha computer for each Dot has these parts:

- **agentOS workspace.**
  - Files: the original inputs (read-only) and the drafts (versioned).
  - Shell tools.
  - Python through Pyodide, for light data work.
- **Browser profile.** A separate Chromium container driven over CDP by the Rust browser supervisor. The profile and downloads persist per Dot, and **a person can take over** from the office view.
- **Document tools as Rust MCP tools.**
  - docx-rs fills templates and makes structured edits.
  - umya-spreadsheet edits existing workbooks without silently recalculating them.
  - pdf-extract extracts digital PDFs, keeping page and span provenance.
  - OAR-OCR reads scanned Thai documents.
  - Typst produces new cited PDF reports.
  - lopdf handles constrained PDF edits.
- **Network rules.** Egress is limited per Dot. Secrets stay outside the sandbox, and no control-plane files or provider keys are mounted.

### 9.1 Craft apps as visible office apps

**Policy.** A small, qualified set of Craft apps gives Dots visible office tools. **KatWork's document pipeline stays authoritative**: the Rust libraries above produce every deliverable. All 13 Craft repos are MIT or Apache-2.0, and all have browser builds, FilmCraft included. They are only 13–22 days old, though, so every operation is gated.

**The apps are displayed inside KatWork (founder decision).** Craft apps run as **web builds**, so a Dot's open app *is its screen*:
- In the 3D office, each Dot's monitor shows a live thumbnail of its current app (PdfCraft, WordCraft, PhotoCraft…).
- Clicking the Dot opens a full-size **Computer panel** with that same live web app. A person can watch, and can **take over** through the control lease.
- On mobile, the panel opens full-screen, read-only by default.
- Each embedded app is served from its own sandboxed origin per Dot session (below), so it can't reach KatWork's authenticated origin.

**Alpha set, in qualification order:**

| Order | App | Role in the alpha |
|---|---|---|
| 1 | **PdfCraft** | Opens source PDFs and generated report PDFs in the office, so people can see a Dot reading and checking citations. It has explicit Thai shaping. Its OCR cannot read Thai, so Thai scans use OAR-OCR. |
| 2 | **PhotoCraft** | Narrow image operations: crop, resize, composite and format conversion, for report figures and cover images. Also a browser workbench where a person can continue the work. Starts with PNG and JPEG. |
| 3 | **WordCraft** | Inspects drafts and previews the meeting-minutes template. Editing is enabled only for templates that pass independent DOCX round-trip tests. |
| Later | **GridCraft** | Stays behind umya-spreadsheet. Loading a file recalculates formulas, which must never replace the saved source values. |
| Deferred | Other nine apps | DeckCraft for PPTX (Phase 2), the creative apps (Phase 4), and the rest. |

**How Dots drive them:**
- **Sidecar CLI or MCP first** for bounded automation. These tools run as pinned images, isolated per tenant, exposed as a small allowlist of operations (inspect, render page, propose image edit, save draft).
- **The browser build** when a person needs to watch and take over. It runs from a separate origin per Dot or session, with a minimal host page that receives only a scoped artifact capability.
- **One active editor at a time.** A control lease belongs to either the Dot or the person. Takeover pauses automation, a save creates a new revision, and stale writes are rejected.
- **The office monitor streams the Dot's actual session.** Headless renders are labelled as previews.

**Rules:**
- A Craft "Save" means *draft saved*. Approval belongs to KatWork and binds the hashes of the report, preview, template, font bundle and evidence ledger. Any later edit invalidates it.
- Never install `craft-launcher` in sandboxes, or let it auto-update.
- `craft-ai-integrations` is not the Dot runtime. Providers go through KatWork's model gateway.
- `artcraft-services` stays excluded.

**Thai text:**
- Ship pinned Thai font files with their hashes and licences to both the workers and the browser builds. If a template requires TH Sarabun New, verify the rights to the supplied files.
- Show any font substitution explicitly.
- Line breaking needs ICU4X segmentation fed into the layout path. Adding the dependency alone does not change how text wraps.

**Branding and licences:**
- Record which licence branch we use (MIT or Apache) for each app.
- Keep the NOTICE files.
- **Remove ArtCraft marks from any modified build** we publish, and use KatWork names and icons instead.
- Review the MPL-2.0 dependencies (CADCraft, the audio and media apps) before adopting them.
- Ship no restricted model weights: no SAM or RawNIND, and no PdfCraft OCR weights by default.

**Gates per operation** (from early evaluation):
- Thai fidelity, on a shared corpus checked by two Thai reviewers.
- Export parity, checked in the office's reference Word, Excel and PDF tools.
- Crash and data safety: malformed files, worker kill, out-of-memory, interrupted saves.
- Evidence integrity: formulas and cached values, page locators.
- Browser handover and persistence.
- Licence and deployment.

An app or operation that fails is left out of the alpha. **It never blocks a template**, because the Rust library path still produces the output.

## 10. Office views

| View | What it shows | Notes |
|---|---|---|
| 3D office | An isometric room with each Dot as an avatar at a desk. State drives the animation: idle, working, researching, waiting, needs approval, error. **Each desk monitor shows the Dot's live Craft web app or browser.** Clicking an avatar opens the Dot's profile, its current task and its **Computer panel**, the full-size live app with take-over | React 19, react-three-fiber 9, drei, an orthographic camera. Kenney furniture and Quaternius bases restyled to KatWork's palette (Nakama-style warmth, blue and yellow). The pixel ratio is capped and rendering pauses when idle. |
| Live agent graph | Every Dot, tool and call animating in real time: who called whom, what is running, where it is blocked | Driven by the same event stream. Inspired by the Flo agent-graph post. |
| Chat | Talk to one Dot or the whole team; approval and plan cards inline | Also the mobile default. |
| Tasks and files board | Jobs by status, tasks, outputs, evidence reports, the approval queue | The accessible fallback: everything in the office view is also here. Keyboard paths, screen-reader labels, reduced motion. |

All four views read **one event model** from the core over AG-UI SSE: job and task lifecycle, tool calls, approvals, claims and Dot states. Animations never invent state.

## 11. Channels, memory, models

- **Channels.** There is one KatWork adapter boundary. It normalizes the tenant, account, sender, thread, event id, text, attachments, reply target, capabilities and approval proposal id. It deduplicates retries and keeps rich cards. Per-channel picks:
  - LINE: `line-bot-sdk-rust`.
  - Slack: `slack-morphism`.
  - Teams: our own Axum adapter, with Graph only for Graph calls.
  - RustChat/Mattermost: a narrow API-v4 adapter.
  - Telegram: `teloxide`.
  - Discord: `serenity`.
  - WhatsApp: the official Cloud API over `reqwest`.

  OAuth tokens and webhook secrets stay inside each adapter.
- **Memory.**
  - Per Dot: private notes plus a Qdrant collection.
  - Per team and Space: shared pages, files and decisions.
  - Tantivy with nlpO3 for exact Thai and English search.
  - Retrieval enforces ACLs in trusted code.
  - An optional RustShare connector imports or links company memory.
- **Models.**
  - The gateway keeps a registry of each model's capabilities and data-residency.
  - Each workspace sets which providers may see which Spaces. For example, university evidence goes to a local model only.
  - Quotas and usage are tracked per Dot and per job.
  - A local model (mistral.rs, or any OpenAI-compatible server) is qualified on Thai office tasks before it is offered.

## 12. Federation (alpha scope)

**Goal:** two KatWork installs, for example two units of one organization, can see each other's shared Dots and send typed work requests and approvals across sites, without a central server.

**Approach:** a selective fork of `reason-network` (MIT or Apache-2.0, Rust, iroh):
- Keep its directory, encrypted peer transport and `ask_user` pattern.
- Replace its hosted identity with an OIDC identity provider and each workspace's own keys.
- Run our own relay.

**Rules:**
- A peer request is untrusted input, with its own permissions.
- A peer can never approve a local side effect.
- Presence is a read-only projection of local state.
- Membership is granted and revoked explicitly by admins.

**Tests:** see §15.

**Risk:** this is the largest addition for a solo alpha. If it slips, it moves to the first post-alpha release; the rest of the alpha does not depend on it.

## 13. Security and privacy

- **Isolation.** Each Dot has its own sandbox and browser profile. Untrusted code never sees the control plane, and tool credentials are scoped per Dot.
- **Treating inputs as untrusted.** Model output is never executed as a shell command. Tool ids are matched against a registry. Content from web pages, files and peers is treated as untrusted (prompt injection).
- **Data residency.** Each Space declares which providers may see its data. Documents and evidence stay on the install.
- **Audit.** Logs record every proposal, approval, tool call and verdict. Enterprise adds export and retention policies.
- **Licences.** Third-party notices are kept, with no GPL or AGPL in the core. Craft apps follow their NOTICE and branding rules (§9).

## 14. agentOS requirements (for the in-house sandbox)

The alpha uses rivet agentOS behind a stable `Computer` contract. The in-house agentOS is planned as a VM-based sandbox; candidates are under evaluation. Whichever is chosen must provide the following so KatWork can switch to it without product changes:

1. **Exec and stream.** Run a command in a Dot's computer and stream stdout and stderr with exit codes.
2. **Files.** Put and get files and list directories, with content hashes.
3. **Snapshot, restore and fork** a Dot's computer (memory and disk). This supports "try two approaches" and rollback after a bad edit.
4. **Warm pool and fast start.** A ready computer in under 1 s.
5. **Per-Dot identity and delegated authority.** An agent acts for a person, recording both identities.
6. **Egress policy** per computer (allowlists).
7. **Idle pause and resume** with persistent state.
8. **A browser and GUI tier** for the Craft apps and the human-takeover browser, beside the light code sandbox (§9).
9. **Quotas and metering** per workspace and per Dot.

## 15. Testing and evaluation

- **TDD for all core code.** CI on every PR covers unit, integration and contract tests.
- **Policy replay.** Recompute every saved job's actions from its log, with no model calls, whenever the policy or thresholds change.
- **Kill-switch test.** With every model off, no job may reach `accepted`.
- **Laya calibration.**
  - A 200-case labelled Thai and English intake set from real requests.
  - Per-language temperature and abstention fitting.
  - 10× resampling.
  - A 50-case routing and delegation benchmark as the release gate.
- **Document fidelity gates.** Real Thai and English DOCX and XLSX samples: read, change and save, then compare text, values, formulas, structure and the rendered output. A scanned Thai PDF goes through OCR, checking names and numbers. Templates are filled with every placeholder and section checked.
- **Approval gate tests.**
  - Approve one action and reject another.
  - Change a proposed target after approval and prove the old approval cannot be reused.
  - Run every channel's approval callback.
- **Channel tests.** Inbound, reply, attachments, signature and token validation, and deduplication, for each channel.
- **Views.** Browser, Tauri and Capacitor builds; Thai input methods; the list-only and screen-reader paths; reconnecting.
- **Federation.** Two installs, our own relay, offline and replayed messages, member removal, expired approvals.
- **Licence review** at each release revision: sources, models, fonts and assets.
- **Demo mode** runs with no keys, using canned decisions clearly labelled as synthetic.

## 16. Open-core boundary

| Free core (Apache-2.0) | Enterprise edition (paid, per organization per year) | Cloud (paid, after the alpha) |
|---|---|---|
| Dots, teams, templates, all channels, the four views, approvals, proof, memory, Laya routing, single-host Compose, basic federation | SSO and SCIM, approval policies (multi-approver, rules), audit export and retention, federation admin (many peers, revocation workflows), priority support, template packs | Hosted KatWork (planned) |

## 17. Milestones to the alpha (founder hours are scarce; each week ends with a demo)

| Week of | Milestone |
|---|---|
| 2026-10-26 | Spec approved. `Minniesse/katwork` created. Skills setup switched to GitHub. Issue breakdown. Compose skeleton (core, Postgres, Qdrant). Auth with password reset. |
| 2026-11-02 | Dot model, team builder, scheduler with leases, Rig loop with one provider. Web chat. AG-UI events. |
| 2026-11-09 | agentOS computers and the browser supervisor. Document tools (docx, xlsx, pdf, OCR). Evidence model. |
| 2026-11-16 | Approval host. Evidence verifier. Policy function and statuses. Meeting-minutes template end to end. |
| 2026-11-23 | Laya in Rust, with Stage 0–1 routing. LINE and Slack adapters. Tasks-and-files board. |
| 2026-11-30 | Lead-Dot planning and delegation. Research-brief template. Live agent graph. |
| 2026-12-07 | 3D office v1. SAR template with university samples. Calibration set collected. |
| 2026-12-14 | Teams, Telegram, Discord and RustChat adapters. One Report template. Monthly board commentary pack. |
| 2026-12-21 | WhatsApp Cloud adapter. Tauri and Capacitor shells. Craft-app integrations that passed the gates. |
| 2026-12-28 | Federation bridge (two installs, own relay). |
| 2027-01-04 | Hardening: kill-switch, replay and fidelity gates. Demo mode. Docs. HA SAR rubric pack with the redaction check. |
| 2027-01-11 | Design-partner dry runs. |
| 2027-01-18 | Fixes from the partners. Licence review. Release candidate. |
| 2027-01-25 | **Public alpha** (GitHub, `docker compose up`, landing page and launch posts). |


## 18. Risks

| Risk | Mitigation / kill criterion |
|---|---|
| Scope for a solo builder: general team, 7 channels, 3 shells, federation, 3D | Strict weekly demos. If a week slips twice, cut in this order: federation → WhatsApp and Teams → Capacitor → 3D (the board stays). |
| Thai document fidelity (templates, OCR, numbers) | The fidelity gates in §15 run in weeks 3–4 with real samples. Fall back to a narrower supported-feature list. |
| Laya calibration fails in English or Thai | Route through the lead Dot (higher LLM cost); Laya becomes optional. |
| Craft apps immature | They are gated (§9); the docx-rs, umya and Typst base keeps working. |
| Free bundled AI at universities, and OpenAI Dots | Lead with finished, cited documents, approvals and self-hosting. Measure the rewrite rate with design partners. |
| Demo reliability: a live demo must never depend on an external login | Password reset and demo accounts in week 1; demo mode needs no keys. |

## 19. Implementation breakdown

This spec covers the whole alpha. Implementation planning splits it into sub-projects, each with its own plan and issues. They are listed in build order; each depends only on the ones before it:

1. **Core runtime.**
   - The Compose skeleton.
   - Auth with password reset and demo accounts.
   - Workspaces, Spaces, Dots and Teams.
   - The scheduler with leases, the Rig loop and the model gateway.
   - The AG-UI event stream and web chat.
2. **Computers and documents.**
   - agentOS workspaces and the browser supervisor.
   - The document tools (docx, xlsx, pdf, OCR, Typst).
   - The evidence model and the artifact store.
3. **Gates and policy.**
   - The approval host and the evidence verifier.
   - The policy function and job statuses.
   - Replay and kill-switch tests.
   - Templates and their acceptance contracts.
4. **Routing.**
   - Stages 0–2: Laya in Rust, calibration, and lead-Dot planning and delegation.
5. **Channels.** One adapter boundary, with seven adapters.
6. **Views and shells.** The tasks-and-files board, the live graph, the 3D office, Tauri and Capacitor.
7. **Craft integration.** PdfCraft, PhotoCraft and WordCraft behind their gates.
8. **Federation.**

### 19.1 Review rule

The model that didn't write a PR reviews it, then the maintainer approves.

## 20. References

Internal research notes (market research, vendor evaluations, spikes) are kept outside this repository.
