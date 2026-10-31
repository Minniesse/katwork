# Agent environment

How Claude Code and Codex are set up in this repo, and what to do when a tool is missing or a command is blocked.

## Files

| Path | Tool | What it does |
| :- | :- | :- |
| `AGENTS.md` | both | Navigation pointers. Codex reads it directly; `CLAUDE.md` imports it with `@AGENTS.md`. |
| `CODING_STANDARDS.md` | both | The rules every review enforces. |
| `.claude/skills/<name>/` | both | Repo skills. `agents/openai.yaml` in each holds Codex's display metadata. |
| `.agents/skills/<name>` | Codex | Symlinks to `.claude/skills/<name>`, where Codex looks for repo skills. |
| `.claude/settings.json` | Claude Code | Shared allowlist for frequent safe commands, and the two hooks below. |
| `.claude/settings.local.json` | Claude Code | Your personal overrides; gitignored. |
| `.codex/config.toml` | Codex | Sandbox, approvals, network, and the two hooks below. |
| `.codex/rules/katwork.rules` | Codex | Forbidden commands, and frequent commands allowed outside the sandbox. |
| `.claude/hooks/guard.py` | both | PreToolUse guard for `git` and `gh` (below). |
| `.claude/hooks/guard_main.py` | both | The guard as it was at `a536fa5`, byte for byte. `guard.py` runs it first; edit `guard.py`, not this. |
| `.claude/hooks/session-env.sh` | both | SessionStart: puts cargo on PATH and exports the test `DATABASE_URL`. |
| `scripts/test-db.sh` | both | Private PostgreSQL 17 cluster per worktree. |

## cargo not found

`rust-toolchain.toml` pins `stable`; the MSRV job also uses the version in `Cargo.toml` (`rust-version`), so install that toolchain too (`rustup toolchain install <version>`).

Homebrew's `rustup` is keg-only, so its `bin` directory isn't on the default PATH. In Claude Code, `session-env.sh` finds cargo at `$KATWORK_RUST_BIN`, `/opt/homebrew/opt/rustup/bin`, `/usr/local/opt/rustup/bin` or `~/.cargo/bin` and adds it to PATH for every command. Set `KATWORK_RUST_BIN` in your shell if yours lives elsewhere.

To set PATH yourself instead, put the whole value in `.claude/settings.local.json`. Claude Code doesn't expand variables in `env` values, so `"$PATH"` would stay literal:

```json
{
  "env": {
    "PATH": "/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
  }
}
```

Codex has no equivalent of Claude Code's env file. Put the rustup `bin` directory on PATH in the environment that launches Codex (your shell profile, or the worker launcher). Until then, its SessionStart hook tells the model where cargo is.

## Test database

`scripts/test-db.sh --help` is the reference. It finds PostgreSQL 17 at `$KATWORK_PG_BIN`, the Homebrew `postgresql@17` prefix, `/usr/lib/postgresql/17/bin`, or PATH, and keeps each worktree's cluster in its own directory under your temp dir on its own port.

In Claude Code, every command run inside a worktree of this repo gets that worktree's `DATABASE_URL` while its cluster is running. In Codex, prefix test commands with `DATABASE_URL=<url printed by start>`.

## Blocked commands

`guard.py` runs before every shell command in both tools. It decides in three steps.

1. The guard as it was at `a536fa5` (`guard_main.py`, unchanged) decides.
2. Where that guard refuses, only the one REST merge below may still pass.
3. Where it allows, `guard.py` may still refuse.

So `guard.py` never allows anything the old guard refused, other than that merge. The guard parses the command the way the shell would, through `&&`/`;`/`|` chains, `sh -c`, `eval`, heredocs fed to a shell, `$(...)`, wrappers such as `env`, `sudo`, `xargs` and `find -exec`, and git and gh aliases. It blocks:

- `git push` that updates or deletes `main` or `master`. That covers explicit refspecs, `HEAD` or a bare push from `main`, push config (`remote.<name>.push`, `push.default=upstream`), `--all`/`--mirror`, and a refspec computed at run time (write the branch name, or `HEAD`)
- deleting a remote branch, by `git push --delete` or `gh pr merge --delete-branch`, while open PRs use it as their base
- `git reset --hard`, and `git clean` that would delete files (dry runs pass)
- commits and merges whose author or committer email isn't a GitHub noreply address
- `gh pr merge` with `--admin`, `--auto`, `--squash` or `--rebase`, or when no review covers the code being merged, and `gh api` or `curl` writes that merge PRs or move refs, except the one REST merge below. Step 3 adds these refusals:
  - an endpoint spelling GitHub routes to such a write, read as GitHub routes it (cut at `?` and `#`, then `.`, `..` and `//` resolved and percent-decoded, in any case), so `.../merge#` or `%6Derge` is still a merge;
  - a write whose path hides a `?`, `#`, `/`, `\`, `.` or `;` behind percent-encoding;
  - auto-merge through the cloud route `.../pulls/<n>/ccr/auto_merge`, and `.../pulls/<n>/update-branch`, like `gh pr merge --auto` and their GraphQL forms;
  - GraphQL mutations sent to `/graphql` or a full URL;
  - for curl, httpie and xh: the method inside a short-option cluster (`curl -sX PUT`), a method word (`http PUT`, `xh put`), request items (`key=value`), a URL glob (`{merge}`, `[m-m]erge`), and an upper-case host

A review covers the head when it was posted on the head commit, or on an earlier commit followed only by merges of the base branch, each byte-identical to git's own clean merge of its two parents (`git merge-tree --write-tree`). Merging `main` into a reviewed PR keeps the review valid; resolving a conflict, changing code in the merge or adding a commit needs a new review. A pending review, created but not yet submitted, covers nothing. The guard doesn't read what a review says; the coordinator weighs that at the merge gate.

Without GraphQL (a cloud session, where every GraphQL call fails with HTTP 403, and so do most `gh pr` commands; `gh pr diff <n>` may still work), use REST:

- create a PR: `gh api repos/{owner}/{repo}/pulls -f title=... -f head=<branch> -f base=<base> -F body=@<file>`
- view it and its head: `gh api repos/{owner}/{repo}/pulls/<n>`; its diff: the same with `-H 'Accept: application/vnd.github.diff'`, or `git diff` of the fetched refs
- its checks: `gh api 'repos/{owner}/{repo}/commits/<sha>/check-runs?per_page=100'` (a page holds 30 by default, and a failing check on page 2 is easy to miss)
- more than 100 of anything: `--paginate` stops after the first page in a cloud session, because the next-page link names the repository by its numeric id, which the session's proxy refuses. Read later pages with `page=2` and so on on the `repos/...` path. The guard's own review and commit reads fail closed on a PR with more than 100 reviews or commits.
- review it: `gh api repos/{owner}/{repo}/pulls/<n>/reviews -f commit_id=<sha> -f event=COMMENT -F body=@<file>` (without `event` the review stays pending)
- merge it: the whole command line `gh api -X PUT repos/<owner>/<repo>/pulls/<n>/merge -f merge_method=merge -f sha=<head sha>` (or `--method PUT`, or `-F` for a field). This is the one REST merge the guard allows, under the same review rule.
  - Write it word for word with the owner, repo, PR number and full head SHA, and nothing else on the line: no `cd ... &&`, pipe, redirection, wrapper, quotes, variables or `{owner}/{repo}`.
  - Run it from the clone, since a merge of the base after the review is checked with git there.
  - The SHA must be the PR's current head, so GitHub refuses the merge if the head moves after the check.
  - Codex's prefix rules ask a person before it, as they do for `gh pr merge`.

The blocking message says what to do instead. Run `.claude/hooks/guard.py --self-test` after changing it.

The self-test's differential part reads the guard at `DIFF_BASELINE` from git. It runs some three thousand merge, ref, contents and GraphQL writes, in the spellings the reviews found, through this guard and through that one. It fails if this guard allows anything the old one refused, other than the whole-line REST merge of a covered head. It also checks that `guard_main.py` is that guard, byte for byte.

## Guard scope

The guard stops accidents, not deliberate evasion. The boundary for `main` is the server-side GitHub ruleset: every change needs a PR, force-push and deletion are refused, only merge commits are allowed, and it applies to admins too.

These ways around the guard are known and accepted, because only someone deliberately evading it would write them:

- Git environment and repository overrides (`GIT_DIR`, `GIT_WORK_TREE`, `env -C`, a `-C` path computed at run time), and `GH_HOST` or `GH_REPO` set for one `gh` command, which the guard's own `gh` reads don't see
- unusual push configuration (`--repo` combined with `remote.<name>.push`, a configured `:` matching refspec)
- command substitutions nested in parameter or arithmetic expansions, and `function` bodies
- destructive options passed through variables, or abbreviated (`git clean --for`)
- files written earlier in the same command line: the guard reads a request body or script before the command runs
- code the guard can't read: compiled programs, other languages, a fresh shell or login

A GraphQL request body the guard can't resolve is refused rather than accepted. It resolves inline queries, files, heredocs, here-strings, `$(cat file)`, and pipes from `cat`, `tee`, `echo` or `printf`; output from any other program counts as unresolved.

Fix a hole when an agent could plausibly fall into it by mistake; otherwise leave it to the ruleset.

## Codex setup

1. Trust the project when Codex asks. Codex loads `.codex/` (config, hooks and rules) only for trusted projects.
2. Review and trust the two hooks with `/hooks`; Codex skips hooks it hasn't been told to trust, and asks again whenever they change.
3. The config runs commands in the `workspace-write` sandbox with `on-request` approvals and network access on. Two limits of the macOS sandbox drive this: Postgres can't create shared memory inside it, so the rules run `scripts/test-db.sh` outside the sandbox; and with network access off, the sandbox also blocks connections to the test database on `127.0.0.1`.
4. The rules also let frequent `cargo`, read-only `gh` and read-only `git` commands (plus `git fetch`) run outside the sandbox without a prompt, matching Claude Code's allowlist. `.git` is read-only inside the sandbox, so commits and pushes ask for approval.
5. The rules ask a person before `gh pr merge`. They also ask before a `gh api` command whose method comes first and is PUT in any letter case, which covers the REST merge: the guard allows it only as a whole line that starts `gh api -X PUT` or `gh api --method PUT`. The rules match a command as typed. So with hooks off they don't see either merge under a wrapper such as `timeout`, on a line Codex doesn't split into plain commands, or with the method after the endpoint. With hooks on, the guard refuses the REST merge in every such form.
