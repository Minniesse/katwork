#!/usr/bin/env python3
"""PreToolUse guard for Bash calls, shared by Claude Code and Codex.

Reads the hook event JSON on stdin. Exit 0 lets the command run; exit 2 blocks
it and the reason on stderr goes back to the agent. Blocks:

- `git push` that updates or deletes main/master (explicit refspec, HEAD or a
  bare push from main, --all/--mirror), and remote branch deletion while open
  PRs still use that branch as their base
- `git reset --hard`, and `git clean` with --force (dry runs pass)
- `git commit` when user.email isn't a GitHub noreply address
- `gh pr merge` with --admin, --squash or --rebase; without a review recorded
  on the PR's current head commit; or with --delete-branch while open PRs
  still use the head branch as their base
- `gh api` calls that merge a PR or delete a branch ref (use the guarded
  commands above instead)

Run the self-test with `.claude/hooks/guard.py --self-test`.
"""

import json
import os
import re
import shlex
import subprocess
import sys

PROTECTED = {"main", "master"}
SHELLS = {"bash", "sh", "zsh", "dash"}
WRAPPERS = {"command", "builtin", "exec", "nohup", "time", "noglob", "sudo"}
SEPARATORS = {";", "&&", "||", "|", "|&", "&", "(", ")", "\n"}
NOREPLY = "@users.noreply.github.com"


class Blocked(Exception):
    pass


def run(args, cwd):
    try:
        out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


def strip_heredocs(text):
    """Drop heredoc bodies so prose inside a PR or commit body never matches."""
    lines = text.split("\n")
    out, end = [], None
    for line in lines:
        if end is not None:
            if line.strip() == end:
                end = None
            continue
        out.append(line)
        m = re.search(r"<<-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1", line)
        if m:
            end = m.group(2)
    return "\n".join(out)


def segments(text):
    """Split a shell string into simple commands (lists of words)."""
    text = strip_heredocs(text).replace("`", " ; ").replace("\n", " ; ")
    lexer = shlex.shlex(text, posix=True, punctuation_chars=True)
    lexer.whitespace_split = True
    lexer.commenters = ""
    current = []
    for token in lexer:
        if token in SEPARATORS or (token and set(token) <= set(";&|()")):
            if current:
                yield current
            current = []
        else:
            current.append(token)
    if current:
        yield current


def unwrap(words):
    """Strip env assignments and wrappers; recurse into `sh -c` and `eval`."""
    while words:
        head = os.path.basename(words[0])
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", words[0]):
            words = words[1:]
        elif head == "env":
            words = words[1:]
            while words and (words[0].startswith("-") or "=" in words[0]):
                words = words[1:]
        elif head in WRAPPERS:
            words = words[1:]
            while words and words[0].startswith("-"):
                words = words[1:]
        elif head in ("timeout", "nice"):
            words = words[1:]
            while words and (words[0].startswith("-") or re.match(r"^[0-9.]+[smhd]?$", words[0])):
                words = words[1:]
        elif head in SHELLS and len(words) >= 3 and re.match(r"^-[a-z]*c[a-z]*$", words[1]):
            return ("script", words[2])
        elif head == "eval":
            return ("script", " ".join(words[1:]))
        else:
            return ("argv", words)
    return ("argv", [])


def check_text(text, cwd, depth=0):
    for words in segments(text):
        kind, value = unwrap(words)
        if kind == "script":
            if depth < 3:
                check_text(value, cwd, depth + 1)
            continue
        if not value:
            continue
        program = os.path.basename(value[0])
        if program == "cd":
            target = value[1] if len(value) > 1 else os.path.expanduser("~")
            cwd = os.path.normpath(os.path.join(cwd, os.path.expanduser(target)))
        elif program == "git":
            check_git(value[1:], cwd)
        elif program == "gh":
            check_gh(value[1:], cwd)


# ---------------------------------------------------------------- git

GIT_OPTS_WITH_VALUE = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path", "--config-env"}


def check_git(args, cwd):
    i = 0
    while i < len(args) and args[i].startswith("-"):
        opt = args[i]
        if opt == "-C" and i + 1 < len(args):
            cwd = os.path.normpath(os.path.join(cwd, os.path.expanduser(args[i + 1])))
        i += 2 if opt in GIT_OPTS_WITH_VALUE else 1
    if i >= len(args):
        return
    sub, rest = args[i], args[i + 1 :]
    if sub == "push":
        check_push(rest, cwd)
    elif sub == "reset" and "--hard" in rest:
        raise Blocked("`git reset --hard` discards work. Use `git stash` or `git restore <path>` for specific files.")
    elif sub == "clean":
        flags = [a for a in rest if a.startswith("-")]
        force = any(a == "--force" or (not a.startswith("--") and "f" in a) for a in flags)
        dry = any(a == "--dry-run" or (not a.startswith("--") and "n" in a) for a in flags)
        if force and not dry:
            raise Blocked("`git clean` with --force deletes untracked files. Run `git clean -n` and remove files by name.")
    elif sub == "commit":
        email = run(["git", "config", "user.email"], cwd) or ""
        if not email.endswith(NOREPLY):
            raise Blocked(
                f"git user.email is '{email}', not a GitHub noreply address. "
                f"Set it with `git config user.email <id+login>{NOREPLY}` before committing."
            )


PUSH_OPTS_WITH_VALUE = {"-o", "--push-option", "--repo", "--receive-pack", "--exec"}


def check_push(args, cwd):
    positionals, deleting = [], False
    i = 0
    while i < len(args):
        a = args[i]
        if a in ("-n", "--dry-run"):
            return
        if a in ("--all", "--mirror", "--branches"):
            raise Blocked(f"`git push {a}` would update main. Push only your branch.")
        if a in ("-d", "--delete"):
            deleting = True
        elif a in PUSH_OPTS_WITH_VALUE:
            i += 1
        elif not a.startswith("-"):
            positionals.append(a)
        i += 1
    refspecs = positionals[1:]
    if not refspecs:
        if deleting:
            return
        branch = current_branch(cwd)
        if branch in PROTECTED:
            raise Blocked(f"bare `git push` from {branch} updates {branch}. Create a branch and push that.")
        return
    for spec in refspecs:
        spec = spec.lstrip("+")
        if deleting:
            src, dst = "", spec
        elif ":" in spec:
            src, dst = spec.split(":", 1)
        else:
            src, dst = spec, spec
        if dst == "HEAD":
            dst = current_branch(cwd) or "HEAD"
        dst = re.sub(r"^refs/heads/", "", dst)
        if dst in PROTECTED or "*" in dst:
            raise Blocked(f"`git push` to {dst} is not allowed. Push your branch and open a PR.")
        if src == "":
            check_no_dependents(dst, cwd, "delete")


def current_branch(cwd):
    return run(["git", "symbolic-ref", "--short", "-q", "HEAD"], cwd)


def check_no_dependents(branch, cwd, action):
    out = run(["gh", "pr", "list", "--base", branch, "--state", "open", "--json", "number"], cwd)
    if out is None:
        raise Blocked(f"couldn't list PRs based on {branch} (is `gh` logged in?), so it can't {action} it safely.")
    numbers = [str(pr["number"]) for pr in json.loads(out or "[]")]
    if numbers:
        listed = ", ".join("#" + n for n in numbers)
        raise Blocked(
            f"open PR(s) {listed} use {branch} as their base, and deleting it closes them. "
            f"Retarget each first with `gh pr edit <n> --base <new-base>`."
        )


# ---------------------------------------------------------------- gh

MERGE_OPTS_WITH_VALUE = {"-b", "--body", "-F", "--body-file", "-t", "--subject", "-A", "--author-email",
                         "--match-head-commit", "-R", "--repo"}


def check_gh(args, cwd):
    repo, i = [], 0
    while i < len(args) and args[i].startswith("-"):
        if args[i] in ("-R", "--repo") and i + 1 < len(args):
            repo = ["--repo", args[i + 1]]
            i += 2
        else:
            i += 1
    words = args[i:]
    if words[:1] == ["api"]:
        joined = " ".join(words)
        if re.search(r"pulls/\d+/merge", joined):
            raise Blocked("merge PRs with `gh pr merge --merge` so the review check runs.")
        if re.search(r"git/refs/heads/", joined) and re.search(r"(-X|--method)\s*DELETE", joined, re.I):
            raise Blocked("delete branches with `git push origin --delete <branch>` so the dependent-PR check runs.")
        return
    if words[:2] != ["pr", "merge"]:
        return
    rest, target, delete_branch = words[2:], None, False
    j = 0
    while j < len(rest):
        a = rest[j]
        if a == "--admin":
            raise Blocked("`gh pr merge --admin` bypasses branch protection and review.")
        if a in ("-s", "--squash", "-r", "--rebase"):
            raise Blocked("use merge commits (`gh pr merge --merge`) so stacked PRs keep their base history.")
        if a in ("-d", "--delete-branch"):
            delete_branch = True
        if a in ("-R", "--repo") and j + 1 < len(rest):
            repo = ["--repo", rest[j + 1]]
        if a in MERGE_OPTS_WITH_VALUE:
            j += 1
        elif not a.startswith("-") and target is None:
            target = a
        j += 1
    view = ["gh", "pr", "view"] + ([target] if target else []) + repo
    out = run(view + ["--json", "number,headRefName,headRefOid,reviews"], cwd)
    if out is None:
        raise Blocked("couldn't read the PR's reviews with `gh pr view`, so the merge can't be checked.")
    pr = json.loads(out)
    head = pr.get("headRefOid")
    reviewed = [r for r in pr.get("reviews", []) if (r.get("commit") or {}).get("oid") == head]
    if not reviewed:
        raise Blocked(
            f"PR #{pr.get('number')} has no review recorded on its head commit {str(head)[:7]}. "
            "The cross-review posts one (see the katwork-review skill), then the maintainer merges."
        )
    if delete_branch:
        check_no_dependents(pr.get("headRefName", ""), cwd, "delete")


# ---------------------------------------------------------------- entry


def command_text(payload):
    cmd = (payload.get("tool_input") or {}).get("command", "")
    if isinstance(cmd, list):
        if len(cmd) >= 3 and os.path.basename(cmd[0]) in SHELLS and cmd[1].startswith("-") and "c" in cmd[1]:
            return cmd[2]
        return shlex.join(cmd)
    return str(cmd)


def decide(payload):
    """Return None to allow, or the reason to block."""
    text = command_text(payload)
    cwd = payload.get("cwd") or os.getcwd()
    try:
        check_text(text, cwd)
    except Blocked as e:
        return str(e)
    except ValueError:
        # Unbalanced quotes: the shell would reject it too. Fall back to a coarse check.
        if re.search(r"\bgit\b.*\bpush\b.*\b(main|master)\b|reset\s+--hard|\bgh\b.*\bpr\s+merge\b", text):
            return "couldn't parse this command; it looks like a guarded git or gh call."
    return None


def main():
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    try:
        payload = json.load(sys.stdin)
    except ValueError:
        print("BLOCKED: guard.py got hook input that isn't JSON.", file=sys.stderr)
        return 2
    reason = decide(payload)
    if reason:
        print(f"BLOCKED by .claude/hooks/guard.py: {reason}", file=sys.stderr)
        return 2
    return 0


def self_test():
    """Offline cases: no network, no `gh`, cwd is a scratch repo on branch work."""
    import tempfile

    repo = tempfile.mkdtemp()
    subprocess.run(["git", "init", "-q", "-b", "work", repo], check=True)
    subprocess.run(["git", "-C", repo, "config", "user.email", "1+bot" + NOREPLY], check=True)
    main_repo = tempfile.mkdtemp()
    subprocess.run(["git", "init", "-q", "-b", "main", main_repo], check=True)
    blocked = [
        "git push origin main",
        "git push -f origin main",
        "git push --force-with-lease origin HEAD:main",
        "git push origin feature:refs/heads/master",
        "git push origin +main",
        "git push origin :main",
        "git push --all origin",
        "cd /tmp && git push origin main",
        "git -C . push origin main",
        "FOO=1 git push origin main",
        "bash -lc 'git push origin main'",
        "echo $(git push origin main)",
        f"cd {main_repo} && git push",
        f"git -C {main_repo} push -u origin HEAD",
        "git reset --hard",
        "git reset --hard origin/main",
        "git clean -fd",
        "git clean -xdf",
        "git clean --force -d",
        "gh pr merge 12 --admin --merge",
        "gh pr merge 12 --squash",
        "gh pr merge --rebase",
        "gh api -X PUT repos/o/r/pulls/12/merge",
        "gh api --method DELETE repos/o/r/git/refs/heads/feature",
    ]
    allowed = [
        "git push origin sp1/task-3-auth",
        "git push -u origin HEAD",
        "git push --force-with-lease origin sp1/task-4-tenancy",
        "git push origin main:feature",
        "git push --dry-run origin main",
        "git status && git log --oneline -5",
        "git commit -m 'never git push origin main'",
        "gh pr create --body-file - <<'EOF'\nNever run git push origin main or git reset --hard.\nEOF",
        "git clean -n -d",
        "git reset --soft HEAD~1",
        "cargo test --workspace --locked",
        "echo 'git push origin main'",
        "gh pr view 12 --json reviews",
    ]
    failures = 0
    for cmd in blocked:
        if decide({"tool_input": {"command": cmd}, "cwd": repo}) is None:
            print(f"FAIL (allowed, should block): {cmd!r}")
            failures += 1
    for cmd in allowed:
        reason = decide({"tool_input": {"command": cmd}, "cwd": repo})
        if reason is not None:
            print(f"FAIL (blocked, should allow): {cmd!r}: {reason}")
            failures += 1
    total = len(blocked) + len(allowed)
    print(f"guard self-test: {total - failures}/{total} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
