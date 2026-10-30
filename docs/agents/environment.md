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

`guard.py` runs before every shell command in both tools. It parses the command the way the shell would, through `&&`/`;`/`|` chains, `sh -c`, `eval`, heredocs fed to a shell, `$(...)`, wrappers such as `env`, `sudo`, `xargs` and `find -exec`, and git and gh aliases. It blocks:

- `git push` that updates or deletes `main` or `master`. That covers explicit refspecs, `HEAD` or a bare push from `main`, push config (`remote.<name>.push`, `push.default=upstream`), `--all`/`--mirror`, and a refspec computed at run time (write the branch name, or `HEAD`)
- deleting a remote branch, by `git push --delete` or `gh pr merge --delete-branch`, while open PRs use it as their base
- `git reset --hard`, and `git clean` that would delete files (dry runs pass)
- commits and merges whose author or committer email isn't a GitHub noreply address
- `gh pr merge` with `--admin`, `--auto`, `--squash` or `--rebase`, or when no review covers the code being merged, and `gh api` or `curl` writes that merge PRs or move refs, except the one REST merge below. The guard reads an endpoint the way GitHub routes it (percent-decoded, `.`, `..` and `//` resolved, `?` and `#` cut, in any case), so `.../merge#` or `%6Derge` is still a merge

A review covers the head when it was posted on the head commit, or on an earlier commit followed only by merges of the base branch, each byte-identical to git's own clean merge of its two parents (`git merge-tree --write-tree`). Merging `main` into a reviewed PR keeps the review valid; resolving a conflict, changing code in the merge or adding a commit needs a new review. A pending review, created but not yet submitted, covers nothing. The guard doesn't read what a review says; the coordinator weighs that at the merge gate.

Without GraphQL (a cloud session, where every GraphQL call, so every `gh pr` command, fails with HTTP 403), use REST:

- create a PR: `gh api repos/{owner}/{repo}/pulls -f title=... -f head=<branch> -f base=<base> -F body=@<file>`
- view it and its head: `gh api repos/{owner}/{repo}/pulls/<n>`; its diff: the same with `-H 'Accept: application/vnd.github.diff'`, or `git diff` of the fetched refs
- its checks: `gh api repos/{owner}/{repo}/commits/<sha>/check-runs`
- review it: `gh api repos/{owner}/{repo}/pulls/<n>/reviews -f commit_id=<sha> -f event=COMMENT -F body=@<file>` (without `event` the review stays pending)
- merge it: `gh api -X PUT repos/<owner>/<repo>/pulls/<n>/merge -f merge_method=merge -f sha=<head sha>`, the one REST merge the guard allows, under the same review rule. The method comes right after `api` (`-X PUT` or `--method PUT`, so Codex asks a person first, as it does for `gh pr merge`), the owner, repo, PR number and full head SHA are written out (no `{owner}/{repo}`, no variables), nothing else is on the command, and the SHA is the PR's current head, so GitHub refuses the merge if the head moves after the check.

`gh pr merge` also reads the PR over REST when `gh pr view` fails.

The blocking message says what to do instead. Run `.claude/hooks/guard.py --self-test` after changing it.

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
