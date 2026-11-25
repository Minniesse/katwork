# Issue tracker: GitHub

Issues and specs for this repo live as GitHub issues. Use the `gh` CLI for all operations.

## Conventions

Most `gh issue` and `gh pr` commands use GraphQL, which a cloud session doesn't have: there they fail with HTTP 403 (`gh pr diff` may still work). Use the REST form given after each command instead.

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line bodies. REST: `gh api repos/{owner}/{repo}/issues -f title="..." -F body=@<file>`.
- **Read an issue**: `gh issue view <number> --json number,title,body,labels,comments`. REST: `gh api repos/{owner}/{repo}/issues/<number>`, and `.../issues/<number>/comments`.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters. REST: `gh api --paginate 'repos/{owner}/{repo}/issues?state=open&labels=<label>' --jq '.[] | select(.pull_request | not) | {number, title, body, labels: [.labels[].name]}'` (this endpoint lists PRs too, and leaves out comments: read them per issue with `.../issues/<number>/comments`).
- **Make an issue a sub-issue of a parent**: `gh issue create --parent <parent> ...`, or `gh issue edit <parent> --add-sub-issue <child>` afterwards (`gh` 2.94+). Older `gh`: `gh api --method POST repos/<owner>/<repo>/issues/<parent>/sub_issues -F sub_issue_id=<child-db-id>` (database id, as in **Blocking** below). Without sub-issues, put `Part of #<parent>` at the top of the child body.
- **Comment on an issue**: `gh issue comment <number> --body "..."`. REST: `gh api repos/{owner}/{repo}/issues/<number>/comments -F body=@<file>`.
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`. REST: `gh api repos/{owner}/{repo}/issues/<number>/labels -f "labels[]=..."` / `gh api -X DELETE repos/{owner}/{repo}/issues/<number>/labels/<label>`.
- **Close**: `gh issue close <number> --comment "..."`. REST: comment as above, then `gh api -X PATCH repos/{owner}/{repo}/issues/<number> -f state=closed`.

Infer the repo from `git remote -v`; `gh` does this automatically when run inside a clone, and fills `{owner}/{repo}` in `gh api` paths the same way.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `gh pr` equivalents:

- **Read a PR**: `gh pr view <number> --comments` and `gh pr diff <number>` for the diff. REST: `gh api repos/{owner}/{repo}/pulls/<number>` (its head is `.head.sha`), `.../issues/<number>/comments`, and the diff with `gh api repos/{owner}/{repo}/pulls/<number> -H 'Accept: application/vnd.github.diff'`.
- **List external PRs for triage**: `gh api --paginate 'repos/{owner}/{repo}/pulls?state=open' --jq '.[] | select(.author_association | IN("OWNER","MEMBER","COLLABORATOR") | not) | {number, title, author: .user.login, author_association, labels: [.labels[].name]}'`.
- **Comment / label / close**: `gh pr comment`, `gh pr edit --add-label`/`--remove-label`, `gh pr close`. REST: the issue forms above work on a PR's number, and `gh api -X PATCH repos/{owner}/{repo}/pulls/<number> -f state=closed` closes it.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either: resolve with `gh pr view 42` and fall back to `gh issue view 42`. REST: `gh api repos/{owner}/{repo}/issues/42` answers for both, and has a `pull_request` key when it's a PR.

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Read it as in **Read an issue** above.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (see **Make an issue a sub-issue of a parent**). Where sub-issues aren't enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: GitHub's **native issue dependencies**, the canonical, UI-visible representation. Add an edge with `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`, _not_ the `#number` or `node_id`). GitHub reports `issue_dependencies_summary.blocked_by` (open blockers only, the live gate). Where dependencies aren't available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (`issue_dependencies_summary.blocked_by > 0`, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me`, the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.
