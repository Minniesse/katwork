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
- `gh pr merge` with `--admin`, `--auto`, `--squash` or `--rebase`, or when no review covers the code being merged, and `gh api` or `curl` writes that merge PRs or move refs

A review covers the head when it was posted on the head commit, or on an earlier commit followed only by merges of the base branch that leave the PR's own diff unchanged. Merging `main` into a reviewed PR keeps the review valid; resolving a conflict or adding a commit needs a new review. The guard doesn't read what a review says; the maintainer weighs that.

The blocking message says what to do instead. Run `.claude/hooks/guard.py --self-test` after changing it.

The guard is a guardrail for agents, not a security boundary: anything that runs code it can't see (a compiled program, a script in another language, a fresh login) gets past it. Server-side branch protection or a ruleset on `main` is what actually refuses a direct push or an unreviewed merge.

## Codex setup

1. Trust the project when Codex asks. Codex loads `.codex/` (config, hooks and rules) only for trusted projects.
2. Review and trust the two hooks with `/hooks`; Codex skips hooks it hasn't been told to trust, and asks again whenever they change.
3. The config runs commands in the `workspace-write` sandbox with `on-request` approvals and network access on. Two limits of the macOS sandbox drive this: Postgres can't create shared memory inside it, so the rules run `scripts/test-db.sh` outside the sandbox; and with network access off, the sandbox also blocks connections to the test database on `127.0.0.1`.
4. The rules also let frequent `cargo`, read-only `gh` and read-only `git` commands (plus `git fetch`) run outside the sandbox without a prompt, matching Claude Code's allowlist. `.git` is read-only inside the sandbox, so commits and pushes ask for approval.
