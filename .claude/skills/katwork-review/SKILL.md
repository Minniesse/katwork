---
name: katwork-review
description: Cross-review a KatWork pull request and post the review. Use when asked to review a PR or re-review its fixes.
---

# Cross-review a PR

The model that didn't write a PR reviews it. A Codex-written PR gets two reviews: one from Claude and a separate one from a fresh Codex session. The maintainer merges once a review says it's ready. `gh pr merge` refuses unless a review covers the head: one posted on it, or on an earlier commit followed only by clean merges of the base branch.

## Steps

1. **Fix the target.** Record the PR's number, base and head:

   ```sh
   gh pr view <n> --json number,baseRefName,headRefName,headRefOid,body,reviews
   git log --format='%h %ae %(trailers:key=Co-Authored-By,valueonly)' origin/<base>..<head>
   ```

   The trailers name the model that wrote each commit. Review only a PR you didn't write. On a re-review, the scope is the delta from the head your last review names (`git diff <reviewed-sha>..<head>`) plus each earlier finding.

2. **Check out the head in a scratch worktree** and start its database:

   ```sh
   git fetch origin
   git worktree add <scratch-dir> <head-sha>
   cd <scratch-dir> && scripts/test-db.sh start
   ```

3. **Read the spec.** The issue the PR closes, its task in `docs/plans/2026-10-22-sp1-core-runtime.md`, the Global Constraints, and the Review Focus items the task names. Done when you can list what the task promises: files, interfaces, tests and failure modes.

4. **Read every changed file** (`gh pr diff <n>`), and apply every rule in `CODING_STANDARDS.md` to the diff. Done when each rule is either satisfied or a finding.

5. **Probe every security claim.** A claim is any sentence in the PR body, a comment or a test name that says something can't happen: a cross-tenant request gets 404, a token works once, a secret never prints, locks follow the global order, a tool outside the allowlist never runs. For each one:
   - find the test that proves it and read its assertions: does it fail if the claim is false?
   - if none does, write a throwaway probe in the scratch worktree (a test, a `curl` against the running server, or a query against the test database) and run it
   - record the outcome as **verified** (and how) or **refuted** (a finding)

   Done when every claim is verified, refuted, or listed as unverifiable and why.

6. **Run the checks** from the `katwork-task` skill's step 5 on the head, or confirm CI ran them green on it with `gh pr checks <n>`.

7. **Write the review** in one file:

   ```markdown
   **Verdict: Ready for maintainer.**   (or: **Verdict: Changes needed.**)

   Reviewed head `<sha>` against `<base>` as <your model>, scope: <full diff | delta <old>..<new>>.

   ### Findings
   1. **<Blocker|High|Medium|Low>: <title>.** `<file>:<line>`. What is wrong, why it matters, the fix, and how you know.

   ### Claims probed
   - <claim>: verified by <test or probe> | refuted, see finding <k>

   ### Checks
   - <command>: <result>
   ```

   "Ready for maintainer" means no Blocker or High finding is open.

8. **Post it as one review.** Confirm the head hasn't moved since step 1 (a review attaches to the head at the moment it's posted), then:

   ```sh
   test "$(gh pr view <n> --json headRefOid -q .headRefOid)" = "<sha>"
   gh pr review <n> --comment --body-file <file>
   ```

   Post one review per round, with every finding in it; fixes go to the author as findings. Then remove the scratch worktree (`scripts/test-db.sh stop`, `git worktree remove <scratch-dir>`).
