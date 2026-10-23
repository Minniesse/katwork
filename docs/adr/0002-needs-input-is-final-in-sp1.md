# 0002. `needs_input` is final in sub-project 1; resume comes in sub-project 3

- Status: Accepted (2026-10-22); amended 2026-10-23
- Spec: §4 (job statuses), §6.2, §6.4 (clarifying), §19

## Context

A Dot can stop to ask a person something (the `ask_user` tool) or because a tool needs approval. The job then ends `needs_input`. The person's answer can either resume the same job or start a new one.

Resuming the same job needs a parked task, a rule for who may answer, and a change to the set of final statuses. Approval resume needs the same machinery, and it arrives with the approval host in sub-project 3. In sub-project 1, jobs carry no plan or evidence that a new job would lose.

## Decision

- In sub-project 1, `needs_input` is a final job status.
- The person's next message in that thread starts a **follow-up job** linked to the first one by `jobs.parent_job_id`.
- A run that stops for approval keeps the exact pending call on its task. `tasks.pending_tool_call` holds the ledger row id, the provider's tool call id, the tool name, the canonical arguments and their SHA-256, and the ledger row holds the same canonical arguments. Sub-project 3 can therefore rebuild and resume exactly that call after a restart, without a data migration of old rows.
- Sub-project 3 adds resume for both questions and approvals in one change, together with the approval host. Who may answer or approve is the requester or a Space admin, always recorded with identity, channel and time.

## Consequences

- Sub-project 1 needs no parked-task state and no answer-authorization rule.
- Sub-project 3 carries one migration: resumable `needs_input`, and the copy of plan and evidence that team jobs will need by then.
- The spec's job-status table and Stage 0 state both phases: a follow-up job in sub-project 1, and resume from sub-project 3, before the alpha ships.

## Amendment (2026-10-23): approval is not executable in sub-project 1

- In sub-project 1 nothing can approve a pending call: there is no approval route or tool, the stored `pending_tool_call` is never dispatched, and the person's next message starts a follow-up job.
- Sub-project 3's approval host binds each approval to the pending call's `args_sha256` and records the approver, who must be the requester or a Space admin, with a receipt keyed by the call. The call is dispatched only once its receipt validates. Repeating the same approval returns the stored receipt and result; a different decision for the same call is rejected with 409.
