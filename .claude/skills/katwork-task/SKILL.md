---
name: katwork-task
description: Implement one task of a KatWork plan as a stacked PR. Use when asked to implement, build or pick up a plan task ("Task N"), or to address review findings on a task PR.
---

# Implement a plan task

One plan task per branch, one PR, stacked on the task it depends on. Opus builds trust-core tasks and Sonnet builds the rest, as the task's **Builder** line says. A Sonnet builder follows the task's steps exactly and stops to ask rather than choosing. Two fresh reviewers who did not write it (a deep reviewer and a checklist reviewer) and a verifier take it from there, as the `katwork-review` skill describes; the coordinator merges it on the merge gate.

## Steps

1. **Read the task.** If a research brief for the task was supplied, read it first. Then open the task's plan (`docs/plans/2026-10-22-sp1-core-runtime.md` for SP1, `docs/plans/2026-10-24-sp2a-documents-evidence.md` for SP2a) and read the task, the Global Constraints, the Review Focus items the task names, and the **Dependency order** section. Check the task's **Builder** line: an SP2a task names `Opus` or `Sonnet`; if it is not the model you are running on, stop and say so. Then read `CODING_STANDARDS.md`: the review enforces every rule in it, and the Sonnet reviewer works through its checklist item by item. Done when you can list the task's Files, Interfaces and Tests, and the issue it closes.

2. **Branch, in a worktree of your own.** Another session switching branches in a shared checkout moves your commits onto its branch. Find the base: the branch of the task this one depends on, or `main` once that task's PR has merged (`gh pr list --state all --json number,headRefName,baseRefName,state`; without GraphQL, `gh api 'repos/{owner}/{repo}/pulls?state=all&per_page=100' --jq '.[] | {number, head: .head.ref, base: .base.ref, state}'`). Then:

   ```sh
   git fetch origin
   git worktree add -b <plan>/task-<N>-<slug> <dir> origin/<base>   # or git switch -c in a worktree that's already yours
   git config user.email   # must end in @users.noreply.github.com
   ```

   `<plan>` is `sp1` or `sp2a`; `<slug>` is one or two words from the task title, as in `sp1/task-4-tenancy` or `sp2a/task-4-xlsx`.

3. **Start this worktree's database.** `scripts/test-db.sh start` prints a `DATABASE_URL` for a private cluster. Claude Code sessions export it into later commands automatically; elsewhere, prefix test commands with `DATABASE_URL=<that url>`. Run tests only against this cluster: on a shared server, parallel workers' `#[sqlx::test]` databases collide.

4. **Red, then green, one step at a time.** For each plan step, write the failing test, run it and watch it fail for the reason you expect, then write the code that makes it pass. Every failure mode the task or its Review Focus items name gets its own test. A Sonnet-built task leaves no choice open: when you meet a case the task does not decide, stop and ask in the PR (or through your coordinator) instead of choosing. Done when every test the task lists exists and passes.

5. **Run every check** and fix until all pass:

   ```sh
   cargo fmt --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   cargo deny check
   docker compose config -q
   cd web && npm test && npm run check:contrast && npm run build   # once web/ exists
   ```

   If a check can't run on this machine (no Docker, say), name it in the PR body; CI runs it.

6. **Commit.** End every message with a `Co-Authored-By:` trailer for the model writing it: `Claude <model> <noreply@anthropic.com>`.

7. **Push and open the PR** against the base from step 2:

   ```sh
   git push -u origin HEAD
   gh pr create --base <base> --title "<SP1|SP2a> Task <N>: <task title>" --body-file <file>
   ```

   Without GraphQL (a cloud session, where every `gh pr` command fails with HTTP 403), open it over REST: `gh api repos/{owner}/{repo}/pulls -f title="<title>" -f head=<branch> -f base=<base> -F body=@<file>`.

   The body, in this order:
   - `Closes #<issue>.`
   - `Stacked on #<base PR> (<base branch>).` when the base isn't `main`
   - What changed, in a few sentences, in the plan's words
   - **Tests**: each new test file and what its tests prove, then each check from step 5 with its result
   - **Ambiguity rulings**: every place the plan was unclear and what you chose
   - **Out of scope**: what the task leaves for later tasks
   - **Written by**: your model and the task's Builder line; trust-core changes (approval, policy, evidence, sandbox) also flag that they need a security review, which the Opus reviewer gives

   Done when the PR is open against the right base, CI is green on its head (`gh pr checks <n>`; without GraphQL, `gh api repos/{owner}/{repo}/commits/<sha>/check-runs`), and the body has every item. Then hand off: the two reviewers, the verifier and the coordinator's merge gate take it from here.

8. **Clean up** when you leave the worktree: `scripts/test-db.sh stop`.

## Addressing review findings

Fix each finding with a test that would have caught it, run step 5 again, and push the fixes as new commits so the reviewer can read the delta from the head they reviewed. Answer the review with one PR comment that maps each finding to its commit.

## Keeping the stack healthy

- Before the PR opens, rebase onto `origin/<base>` freely.
- Once it's open, bring base changes in with `git merge origin/<base>` rather than rebasing, so reviewed commits stay valid and PRs stacked on yours keep their history. A clean merge keeps the last review valid for `gh pr merge` and its REST form; a merge that resolves conflicts needs a review of the merge. Merges into `main` are merge commits for the same reason.
- When your base PR merges, GitHub may retarget your PR to `main`; check it with `gh pr view <n> --json baseRefName` (without GraphQL, `gh api repos/{owner}/{repo}/pulls/<n> --jq .base.ref`). Before anyone deletes a base branch, every PR based on it is retargeted with `gh pr edit <n> --base <new-base>` (without GraphQL, `gh api -X PATCH repos/{owner}/{repo}/pulls/<n> -f base=<new-base>`); deleting it first closes them.
