---
name: katwork-review
description: Review a KatWork pull request, as its deep reviewer, its checklist reviewer or its verifier, and post the review. Use when asked to review a PR, re-review its fixes, or verify review findings.
---

# Review a PR

Every PR gets two independent reviews from fresh sessions that did not write it (spec §19.1), and a verifier:
- the **deep reviewer** (Opus): correctness, security, concurrency (lock order, cancellation per #48), tenant isolation and prompt injection, backed by probes and mutations;
- the **checklist reviewer** (Sonnet): the **checklist** in `CODING_STANDARDS.md`, item by item;
- the **verifier**: tries to refute each P0 and P1 finding before it goes back to the builder, and records each as confirmed or refuted.

A PR gets at most three fix rounds; after the second, P3 findings become follow-up issues, while P0, P1 and security findings always block. Code PRs merge on the merge gate: the coordinator merges only when all three have run on the exact head SHA, nothing blocking is open and CI is green, and reads the full diff of a trust-core PR first. The founder approves plans and answers "NEEDS FOUNDER DECISION" items. `gh pr merge` and its REST form refuse unless a review covers the head: one posted on it, or on an earlier commit followed only by clean merges of the base branch.

## Steps

1. **Fix the target.** Record the PR's number, base and head:

   ```sh
   gh pr view <n> --json number,baseRefName,headRefName,headRefOid,body,reviews
   git log --format='%h %ae %(trailers:key=Co-Authored-By,valueonly)' origin/<base>..<head>
   ```

   Without GraphQL (a cloud session, where every `gh pr` command fails with HTTP 403), read the PR and its head with `gh api repos/{owner}/{repo}/pulls/<n> --jq '{number, base: .base.ref, head: .head.ref, sha: .head.sha, body}'` and its reviews with `gh api repos/{owner}/{repo}/pulls/<n>/reviews --paginate`. The trailers name the model that wrote each commit. Review only a PR you didn't write, in a fresh session that took no part in writing it. On a re-review, the scope is the delta from the head your last review names (`git diff <reviewed-sha>..<head>`) plus each earlier finding.

2. **Check out the head in a scratch worktree** and start its database:

   ```sh
   git fetch origin
   git worktree add <scratch-dir> <head-sha>
   cd <scratch-dir> && scripts/test-db.sh start
   ```

3. **Read the spec.** The issue the PR closes, its task in its plan (`docs/plans/2026-10-22-sp1-core-runtime.md` for SP1, `docs/plans/2026-10-24-sp2a-documents-evidence.md` for SP2a), the Global Constraints, and the Review Focus items the task names. Done when you can list what the task promises: files, interfaces, tests and failure modes.

4. **Read every changed file** (`gh pr diff <n>`; without GraphQL, `gh api repos/{owner}/{repo}/pulls/<n> -H 'Accept: application/vnd.github.diff'`, or `git diff origin/<base>...<head-sha>` after the fetch in step 2), and apply every rule in `CODING_STANDARDS.md` to the diff. The checklist reviewer answers each checklist item as pass or a finding. Done when each rule is either satisfied or a finding.

   **Spec deviations.** A review of a plan, an ADR or any PR that touches the spec has a "Spec deviations" section: every tool, service, dependency, product rule or decision in the change that the approved spec on `main` does not name or that contradicts it, with `file:line`, each marked "founder-decided (<label or date>)" or "NEEDS FOUNDER DECISION". "NEEDS FOUNDER DECISION" is only for a product, scope or process choice that the spec and the founder decisions do not settle; a defect that breaks a spec rule (a tenant leak, a lock-order break, a wrong limit) is a finding with a severity, never a founder question. A plan review is not Ready while a "NEEDS FOUNDER DECISION" entry is unanswered.

5. **Probe every security claim** (the deep reviewer always, with a mutation where a test should catch the claim failing; the checklist reviewer for the claims its checklist names). A claim is any sentence in the PR body, a comment or a test name that says something can't happen: a cross-tenant request gets 404, a token works once, a secret never prints, locks follow the global order, a tool outside the allowlist never runs. For each one:
   - find the test that proves it and read its assertions: does it fail if the claim is false?
   - if none does, write a throwaway probe in the scratch worktree (a test, a `curl` against the running server, or a query against the test database) and run it
   - record the outcome as **verified** (and how) or **refuted** (a finding)

   Done when every claim is verified, refuted, or listed as unverifiable and why.

6. **Run the checks** from the `katwork-task` skill's step 5 on the head, or confirm CI ran them green on it with `gh pr checks <n>` (without GraphQL, `gh api repos/{owner}/{repo}/commits/<head-sha>/check-runs`).

7. **Write the review** in one file:

   ```markdown
   **Verdict: Ready to merge.**   (or: **Verdict: Changes needed.**)

   Reviewed head `<sha>` against `<base>` as <your model>, <deep review | checklist review | verification>, scope: <full diff | delta <old>..<new>>, fix round <1|2|3>.

   ### Findings
   1. **<P0 Blocker|P1 High|P2 Medium|P3 Low>: <title>.** `<file>:<line>`. What is wrong, why it matters, the fix, and how you know.

   ### Checklist   (checklist review: one line per item, pass or finding k)

   ### Spec deviations   (plan, ADR or spec-touching PRs)
   - `<file>:<line>` <what>: founder-decided (<label or date>) | NEEDS FOUNDER DECISION

   ### Claims probed
   - <claim>: verified by <test or probe> | refuted, see finding <k>

   ### Checks
   - <command>: <result>
   ```

   "Ready to merge" means no P0 or P1 finding and no security finding is open and, for a plan, no "NEEDS FOUNDER DECISION" entry is unanswered (a plan then goes to the founder for approval; a code PR to the coordinator's merge gate).

   **As the verifier**, write one line per P0 and P1 finding of the two reviews: the finding, what you ran to refute it, and **confirmed** or **refuted**. Only confirmed findings go back to the builder.

8. **Post it as one review.** Confirm the head hasn't moved since step 1 (a review attaches to the head at the moment it's posted), then:

   ```sh
   test "$(gh pr view <n> --json headRefOid -q .headRefOid)" = "<sha>"
   gh pr review <n> --comment --body-file <file>
   ```

   Without GraphQL, over REST:

   ```sh
   test "$(gh api repos/{owner}/{repo}/pulls/<n> --jq .head.sha)" = "<sha>"
   gh api repos/{owner}/{repo}/pulls/<n>/reviews -f commit_id=<sha> -f event=COMMENT -F body=@<file>
   ```

   `event=COMMENT` posts the review; without it the review stays pending, and a pending review covers nothing. Post one review per round, with every finding in it; fixes go to the author as findings. Then remove the scratch worktree (`scripts/test-db.sh stop`, `git worktree remove <scratch-dir>`).
