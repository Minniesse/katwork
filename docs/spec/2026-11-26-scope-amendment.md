# Scope amendment: the expanded scope in alpha waves (proposal)

- Status: Proposed (2026-11-26). Every change below is a proposal for the founder. None is applied to `katwork-design.md` until the founder approves it.
- Decision owner: Founder
- Roadmap: `docs/roadmap.md` (sub-projects 9 to 14, "Additions to existing sub-projects", "Re-plan: staged alphas", "Differentiation", and open questions Q8 to Q28)

## Why

On 2026-11-26 the founder made three rulings about a competing desktop agent and the alpha. The first adopts every one of its capability areas into KatWork's scope. The second puts all of that scope before the alpha. The third accepts staged alphas. Alpha 1 ships the current spec scope (sub-projects 1 to 7). The new areas follow in Alpha 2 (connectors, routines, team admin, and the platform additions), Alpha 3 (companion and spend) and Alpha 4 (voice and meetings), each with its own plan approval. The spec still describes the narrower alpha: §1.1 lists nine goals, and §1.2 rules out voice and video calls, a hosted cloud and native binaries. This file lists every spec change the new scope needs. Each is written as a proposal, tagged with the alpha wave it belongs to, and points to the roadmap question that decides it. A tag such as [Alpha 2] means the change applies to the spec from that wave on; until then the spec as written describes the earlier waves. Where a ruling settles a point, the entry says so. Where it does not, the entry stays open.

KatWork takes ideas and scope from that product only. Its code, prompts and text are not used.

## §1.1 Goals for the alpha

1. **Heading date.** [Alpha 1, then each wave] Change "Goals for the alpha (2027-01-31)" to "Goals for Alpha 1" with the date the founder approves. Add a short table of the later waves and their goals. The re-plan proposes these dates (roadmap Q1):
   - Alpha 1: week of 2027-12-06.
   - Alpha 2: week of 2028-07-17.
   - Alpha 3: week of 2028-11-13.
   - Alpha 4: week of 2029-02-26.
2. **New goals.** [Alphas 2 to 4] Add these after goal 9, one per new sub-project:
   - 10. People can let a Kat act on their own computer through an optional companion app, within grants they set (sub-project 9). [Alpha 3]
   - 11. Kats can pay, within spend limits and with a person's approval for every payment (sub-project 10). [Alpha 3]
   - 12. People can talk to Kats by voice in Thai and English. Kats can make approved phone calls and capture meetings with recorded consent (sub-project 11). [Alpha 4]
   - 13. Kats can use connectors, MCP servers and plugins from a catalog, under the same approval gate (sub-project 12). [Alpha 2]
   - 14. Routines run Kats on a schedule or a trigger (sub-project 13). [Alpha 2]
   - 15. Admins can share Kats, set roles and service accounts, and enforce allowlists from an admin console (sub-project 14). [Alpha 2]
3. **Goal 3 (channels).** [Alpha 3] Add SMS and inbound mail to the listed channels, if §11 adds them (see §11 below).
4. **Goal 4 (real work on real computers).** [Alpha 3] Add "and, when a person allows it, on that person's own computer".
5. **Goal 7 (four views).** [Alpha 2] No change to the four views. The admin console and the review queue are pages of the board, not new views (proposal; see §10).

## §1.2 Non-goals for the alpha

1. **"Voice and video calls."** [Alpha 4] The rulings place voice, phone calls and meeting capture in Alpha 4, so this line can no longer stand as written. Proposal: remove it. Add a rule instead: phone calls to third parties and live meeting capture stay off in any install until the Thai legal read passes (roadmap Q11).
2. **"A hosted cloud offering."** [Alpha 3] Payments and credits may need a KatWork-run service. Proposal: keep this non-goal, and use payment rails that each install brings (roadmap Q9 and Q10). If the founder chooses a KatWork-run payment service, this line changes.
3. **"Native Linux binaries inside a Kat's computer."** [Alpha 2] A full shell, stdio MCP servers and plugin commands would break this. Proposal: keep it. Stdio MCP servers and plugin commands run as pinned containers outside agentOS, and a full shell waits for KatsAgentOS (roadmap Q17).
4. **"A streamed desktop for the Craft apps"** [Alpha 1] waits for sub-project 7. The staged-alpha ruling, as relayed, places sub-project 7 in Alpha 1. Local computer use, screen recording and the general desktop on 2b's GUI tier, all in later waves, need the same virtual display. Proposal: once the founder confirms that sub-project 7 is in Alpha 1 (roadmap Q3), remove this line. That adds the streamed Craft desktop (an app running windowed on a virtual display) to Alpha 1, so it is the one Alpha 1 change that adds scope, and it does so only if Q3 is confirmed. Without it, sub-project 7 and its 20 tasks leave Alpha 1.
5. **"High availability or multi-host scheduling."** [Alpha 2] Private worker pools would need multi-host scheduling. Proposal: keep the non-goal. Whether private pools belong to an alpha wave is a founder question (roadmap Q23).
6. **New non-goals, proposed** [all waves] (each is an open question, roadmap Q18 to Q21):
   - Browser stealth that hides that a Kat's browser is automated.
   - Egress through a person's own machine.
   - Product telemetry on by default.
   - Link previews in chat.
   - A model that approves actions on its own.

## §8 Alpha templates

1. **Meeting minutes.** [Alpha 4] "Input: typed notes, DOCX or a supplied transcript; no audio in the alpha" becomes "Input: typed notes, DOCX, a supplied transcript, or audio. Audio comes from an upload, or from a meeting the Kat captured with recorded consent." Audio needs sub-project 11's Thai speech engine and the recording rules in §13.
2. **Phase 3 capability pulled forward.** [Alpha 2] Scheduled browser research moves from Phase 3 into Alpha 2 through sub-project 13. The Phase 3 use cases that need it (competitor digest, and others) stay in Phase 3. Only the capability moves.
3. **Phase 4 capability pulled forward.** [Alpha 2] Image and video generation through the model gateway (opt-in, with cost caps) moves into Alpha 2 through sub-project 12. The creative-Kat use cases stay in Phase 4.
4. **The six templates.** [Alpha 1] No change. The new scope adds no template, and §18's cut order still names none (roadmap Q4).
5. **A developer template** [Alpha 2] (forge, pull requests, CI findings) is a candidate use case the new scope suggests. Proposal: list it under Phase 2, not as a seventh alpha template. The forge tools themselves stay alpha scope in sub-project 12; only a template built on them would wait.

## §10 Office views

1. **Board.** [Alpha 2] Add a review queue (memory, skills, teach recordings), an approval-rules page and an admin console (allowlists, the denylist, the egress dashboard with each Kat's applied status, and network diagnostics). These are pages of the tasks-and-files board, so the four views stay four.
2. **Chat.** [Alpha 3 for payment and order cards, Alpha 4 for voice] Add voice input and output, voice memos, and payment and order cards. Group chat already exists per channel.
3. **Shells.** [Alpha 3] The Tauri desktop shell gains the companion features: a tray, notifications, a capture overlay and element picking. A URL scheme, if any, uses a typed route table: nothing is sent automatically, and tokens travel in the fragment.
4. **Computer panel.** [Alpha 2] It can show a general Linux desktop (2b's GUI tier) as well as a browser or a Craft app.
5. **Tool-result cards.** [Alpha 3] If the founder adopts them (roadmap Q20), they render in a sandboxed frame with no network, and a callback runs only after a person acts. This changes ADR-0010.
6. **Hiding a Kat's screen.** [Alpha 2] If an admin can hide a Kat's screen, the activity log and the take-over path stay on (roadmap Q21).
7. **Kat creation.** [Alpha 3] Kat creation gains avatars and template browsing. A system-prompt override is logged in the audit log.

## §11 Channels, memory, models

1. **Channels.** [Alpha 3] Add two adapters, each behind the one adapter boundary: SMS (a Kat's own number) and inbound mail (a Kat's own address). Both deliver untrusted text (ADR-0010). The seven listed adapters stay.
2. **Messages on a person's own computer** [Alpha 3] (for example a desktop messaging app) are companion tools in sub-project 9, not channel adapters.
3. **Connectors.** [Alpha 2] Add a connector catalog beside the channels. It covers OAuth and API-key connectors and general MCP servers (remote HTTPS first). Each connector names whose credential it uses (the person's, a Kat service account, or a delegated one), and its secrets are write-only.
4. **Memory.** [Alpha 2] Add a review queue for shared memory and skills, and Kat-authored skills with versions, review and revert. Shared memory written after an untrusted read is flagged, and needs a person, as `remember` already does.
5. **Models.** [Alpha 2 for image and video, Alpha 4 for speech] The registry gains modalities: speech to text, text to speech, image and video generation. Each modality carries data-residency like the text models. A local speech engine is offered for local-only Spaces once one passes the Thai gate.
6. **Routines.** [Alpha 2] Add a trigger model (schedule, webhook, channel message, connector event) on the scheduler. Webhook keys are stored encrypted.

## §13 Security and privacy

1. **Isolation.** [Alpha 3] Add the companion as a new execution target that is not a Kat computer. Proposal: it checks each approval itself, for example against a signed proposal digest, and does not accept an "approved" flag from the server. Its privileges sit in a separate helper (companion trust-model ADR). ADR-0009's kill criteria do not cover it; it needs its own conformance test.
2. **Inputs as untrusted.** [Alpha 2, and each later source with its wave] Add trigger-delivered text, inbound SMS and mail, meeting transcripts, connector and MCP results, shared memory and skills, and teach recordings to the list of untrusted sources. Each gets a trust flag on its tool calls and a carrier in KatBench-Inject. A run that read them cannot hold an outward tool without a person.
3. **Deletion.** [Alpha 2 for screen and teach recordings, Alpha 4 for audio] Extend the purge to recordings (audio, screen and teach recordings), transcripts, voice memos, and personal data read through the companion. Add a retention limit for recordings (recording-retention ADR).
4. **Consent.** [Alpha 4] Add a consent record for each recording of another person and each call to a third party. No call and no live meeting capture runs until the legal read passes (roadmap Q11).
5. **Payments.** [Alpha 3] Card data is stored as write-only vault fields; the model never sees card numbers. Every payment is a Proposal with amount, merchant and recipient, and spend limits are checked in code. A payment whose result KatWork cannot confirm is recorded with its own outcome, separate from success and failure.
6. **Sending as a person.** [Alpha 3] Every send on a person's behalf is a Proposal and is checked after sending. A standing grant, if the founder allows one (roadmap Q21), is bound to one recipient and content class and capped.
7. **Clipboard.** [Alpha 2] Clipboard transfer into or out of a Kat's computer is explicit and logged.
8. **Browser.** [Alpha 2] Dialogs in a Kat's browser go to the person or fail. They are never accepted automatically. Credential fill clears the fields and hands over to the person when it fails.
9. **Egress.** [Alpha 2] The gateway fails closed when its policy cannot refresh. A tighter policy applies at once; a looser one applies only on reconnect.
10. **Plugins.** [Alpha 2] A plugin package passes strict ingestion checks before install: size limits, no links outside the package, no executable surprises, and a manifest that matches its contents.
11. **Telemetry.** [all waves] No change. No telemetry leaves the install unless an admin turns it on (roadmap Q19).

## §16 Open-core boundary

[Alpha 2 and Alpha 3] The table gains rows for the new features. Which column each goes in is a founder decision (roadmap Q10); the recommendation there is:
- **Free core:** approvals, spend limits and every safety gate.
- **Enterprise:** approval rules, the team marketplace, service accounts and the admin console.
- **Cloud:** credits and top-ups.

## Other sections the new scope touches

These are smaller and are listed so that nothing is missed:
- **§4 Domain model:** [Alpha 2, payment instruments and orders Alpha 3] routines and triggers; connectors and their credentials; skills and versions; roles beyond admin and member; version numbers on tracked tasks for conflict detection; payment instruments and orders.
- **§5.2 Services:** [companion relay Alpha 3, speech engine Alpha 4, the rest Alpha 2] a companion relay, the speech engine, the connector runtime (pinned containers for stdio MCP and plugin commands), and the routine trigger endpoint. Also, if roadmap Q28 is approved, Playwright as a second browser runtime beside chromiumoxide [Alpha 2]; it is not counted in the roadmap's estimates.
- **§6.4:** [Alpha 2] rooms where Kats talk to each other, with cost and loop rules. The 5-task, 3-level DAG limit still holds.
- **§6.5 Approvals:** [Alpha 2, payment second factor Alpha 3] new action types: `pay`, `message-on-behalf` and `record`. Add a `pending_external` state, typed denial reasons, and a line on each approval card explaining why the job stopped. Bulk approval covers only identical requests, never outward or destructive ones. A second factor for payments.
- **§7.2:** [Alpha 2] a disclosure card before sharing a Kat, listing what travels with it.
- **§9:** [Alpha 2] the general desktop on the GUI tier, teach recording, screen recording, and the vault's card and contact fields.
- **§17:** [Alpha 1] replace the week table with the re-plan's table once the founder approves a date.
- **§18:** [all waves] add risks for legal exposure (calls, recording, payments), Thai speech quality, and the pace assumption.
- **ADR-0002:** [Alpha 2] questions a Kat asks while it keeps working change the resume model; needs an ADR note.
- **ADR-0009:** [Alpha 2] a general desktop as a GUI-tier use; stealth browsing and egress through a person's machine as rejected alternatives, if the founder agrees (roadmap Q18).
- **ADR-0010:** [Alpha 2, tool-result cards Alpha 3] new untrusted sources (above); tool-result cards, if adopted (roadmap Q20).

## New ADRs needed

Each paragraph states the problem only. The ADR makes the decision, and the founder approves it.

### Companion trust model [needed before Alpha 3]

Today every place a Kat acts is a server-side computer that KatWork isolates and controls (ADR-0009). A companion on a person's own computer reverses that. The person's files, apps, messages and accounts are in reach, and the host is not KatWork's. The ADR must settle several things:
- what the companion may do, and how a person grants and revokes each capability;
- how the companion confirms that an action was approved, and whether it may rely on the server's word for it or must check something it can verify itself, such as a signed proposal digest;
- how its privileges are split across separate helpers;
- what happens when the server is compromised or unreachable;
- which conformance test stands in for ADR-0009's kill criteria on a host KatWork does not own.

### MCP posture [needed before Alpha 2]

ADR-0010 and ADR-0011 cover untrusted content and the Craft apps' MCP servers behind a policy proxy. General MCP servers are wider. They come from third parties and run commands or call remote services. They return text and resources that may carry injections, they ask for credentials, and they can elicit input from a person. The ADR must settle several things:
- which transports are allowed: remote HTTPS only at first, or stdio in pinned containers;
- how tools are allowlisted per Kat and per Space: always, ask or blocked;
- how MCP results are flagged as untrusted;
- whose credentials a server sees;
- whether a server added by one person may run for the whole team.

### Voice, STT and TTS for Thai [needed before Alpha 4]

Voice chat, dictation, phone calls and meeting capture all need speech-to-text and text-to-speech that work for Thai office speech. That speech mixes Thai and English, names, numbers and Buddhist-Era dates. No engine is qualified yet. The ADR must set a qualification gate, as ADR-0008 did for OCR:
- a test set and error thresholds per language;
- which engines are candidates, hosted and local, and their licences and terms;
- the data residency of audio;
- how a Space that allows only local models gets voice.

### Payment rails [needed before Alpha 3]

Kats paying for things need instruments, limits and records. A self-hosted install has no merchant relationship, card-issuing account or payment processor of its own. The ADR must settle several things:
- which rails an install uses (roadmap Q9);
- how instruments are stored and shown to Kats without exposing card data to a model;
- how spend limits and expected prices are checked in code;
- what second factor approves a payment;
- how a payment with an unknown result is recorded and resolved;
- what happens to refunds and disputes.

### Recording retention and PDPA [needed before Alpha 2 for screen and teach recordings, and before Alpha 4 for audio]

Meeting capture, voice memos, phone calls, screen recording and teach recordings create recordings of people, often people who are not KatWork users. Under Thailand's PDPA these are personal data. Some need consent, and all need a retention limit and deletion. The ADR must settle several things:
- what consent is recorded, by whom, and when;
- how long each kind of recording and transcript is kept;
- how §13's purge reaches recordings and everything derived from them;
- how a person who was recorded asks for deletion.

It depends on two legal reads: a PDPA read on screen and teach recordings, needed before Alpha 2, and the read on calls and meeting recording in roadmap Q11, needed before Alpha 4. The roadmap's schedule asks for the ADR and the first read by the Alpha 2 plan approval (proposed week of 2027-10-11).

A connector-credentials ADR is also needed, before Alpha 2. It decides whose credential a connector uses: the person's, a Kat service account, or a delegated one. The roadmap names it under sub-project 12.

## Differentiation proposals

The roadmap's "Differentiation" section answers the founder's request for research into what people need. Where its responses are not already in the spec, an ADR or an approved plan, they are listed here. Each is a proposal, with an owning sub-project, a task estimate and a proposed wave. Roadmap Q27 decides each one. Until the founder rules, all 29 tasks are counted in Alpha 2. The recommendation in Q27 pulls the five marked "claim" into Alpha 1, because the published claims rest on them.

| Id | Proposal | Spec section | Owner | Tasks | Proposed wave |
|---|---|---|---|---|---|
| DP1 | Seeded-defect KatBench worlds for every alpha template, adding sign, ratio, currency and unit-scale traps (claim 1) | §15 | 3 | 3 | Alpha 1 if Q27, else Alpha 2 |
| DP2 | Claims ordered by risk in the evidence report, and a timing test for opening a claim's source span (claim 2) | §7.1, §15 | 6 | 2 | Alpha 1 if Q27, else Alpha 2 |
| DP3 | Reviewer minutes per deliverable, logged from use | §15 | 6 | 1 | Alpha 2 |
| DP4 | A completion receipt generated from the job log: inputs read against the input list, checks run and not run, tried and rejected, stops, time and tokens. A skipped file or a zero-match run is a failed check (claim 3) | §7.1, §19 item 3 | 3 | 3 | Alpha 1 if Q27, else Alpha 2 |
| DP5 | A data screen per Space (providers, what each model call sent); a personal-data flag on a folder that routes its jobs to a local model; the redaction check on every outward send, not only the HA template | §11, §13 | 3, with 6 | 3 | Alpha 2 |
| DP6 | An offline-install test in CI: zero outbound connections (claim 4) | §13, §15 | 2b | 1 | Alpha 1 if Q27, else Alpha 2 |
| DP7 | Each template declares its date era and digit form; a check fails mixed forms (claim 5) | §7.3 | 3 | 1 | Alpha 1 if Q27, else Alpha 2 |
| DP8 | Thai print checked on two renderers | §15 | 3 | 1 | Alpha 2 |
| DP9 | Approval cards: approve with a comment, unsure, opening the cited span first for high-risk claims, a card budget per job, the card content stored in the audit row, a seal-and-initial approval type, and an alarm when approvals become too fast and uniform | §7.2 | 3, with 6 | 4 | Alpha 2 |
| DP10 | Redo one section without restarting the job; a target for how fast a stop takes effect | §6.6 | 4 | 2 | Alpha 2 |
| DP11 | A matched-and-exceptions output for reconciliation | §8 | 3 | 2 | Alpha 2 |
| DP12 | A cost estimate before a team job, a workspace cost cap, an "unpriced" state for models without a price, and no silent switch to a weaker model | §6.6, §11 | 4, with 6 | 3 | Alpha 2 |
| DP13 | A decision log with reasons and rejected options | §11 | 2c | 2 | Alpha 2 |
| DP14 | A first run with one visible next action, and a one-page glossary | §10 | 6 | 1 | Alpha 2 |

Not listed: an official letter and memo template, which is roadmap Q26 and is not counted.

## Open questions

The founder's questions for this amendment are in the roadmap, Q1 and Q8 to Q28, each with a recommended option. Roadmap Q3 (sub-project 7 and the alpha) also decides §1.2 item 4 above. Alpha 1 is the spec as written, re-dated. Only two things would add scope to it: confirming Q3, which brings the streamed Craft desktop, and accepting the Q27 recommendation, which brings the claim-carrying differentiation proposals.
