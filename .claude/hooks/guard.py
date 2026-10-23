#!/usr/bin/env python3
"""PreToolUse guard for Bash calls, shared by Claude Code and Codex.

Reads the hook event JSON on stdin. Exit 0 lets the command run; exit 2 blocks
it and the reason on stderr goes back to the agent. Blocks:

- `git push` that updates or deletes main/master: explicit refspecs, HEAD or @
  on main, --all/--mirror, `:` (matching), config-driven pushes
  (remote.<name>.push, push.default upstream/matching), and refspecs built at
  run time; also deleting a remote branch while open PRs use it as their base
- `git reset --hard`, and `git clean` that would delete (force, or
  clean.requireForce=false) unless it is a dry run
- commits and merges when the author or committer email isn't a GitHub
  noreply address
- `gh pr merge` with --admin, --auto, --squash or --rebase; without a review
  that covers the PR (see merge_verdict); or with --delete-branch while open
  PRs use the head branch as their base
- `gh api` writes that merge, move or delete refs or write contents

Commands are parsed with a quote-aware shell lexer. It follows `;`, `&&`, `|`
chains, subshells, `$(...)` and backticks, `sh -c`, `eval`, heredocs and
here-strings fed to a shell, shell scripts run by path, git and gh aliases, and
wrappers such as env, command, sudo, timeout, xargs and find -exec.

This is a guardrail for cooperative agents, not a security boundary: server-side
branch protection is what actually stops a push to main.

Run the self-test with `.claude/hooks/guard.py --self-test`.
"""

import json
import os
import re
import shlex
import subprocess
import sys

PROTECTED = {"main", "master"}
NOREPLY = "@users.noreply.github.com"
SHELLS = {"bash", "sh", "zsh", "dash", "ksh", "mksh", "fish"}
INTERPRETERS = {"python", "python3", "node", "perl", "ruby", "php", "osascript", "deno", "bun", "lua", "tclsh"}
MAX_DEPTH = 6
MAX_SCRIPT_BYTES = 256 * 1024


class Blocked(Exception):
    pass


class ParseError(Exception):
    pass


def run(args, cwd, timeout=20):
    try:
        out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


# ================================================================ shell lexer


class Word:
    """One shell word: its value after quote removal, and whether that value
    depends on run-time expansion ($var, $(...), globs, braces)."""

    __slots__ = ("text", "dynamic")

    def __init__(self, text, dynamic=False):
        self.text = text
        self.dynamic = dynamic

    def __repr__(self):
        return f"Word({self.text!r}{', dynamic' if self.dynamic else ''})"


class Command:
    """A simple command: its words, plus what it reads on stdin from heredocs
    and here-strings."""

    __slots__ = ("words", "stdin")

    def __init__(self):
        self.words = []
        self.stdin = []


class Parsed:
    def __init__(self):
        self.pipelines = []  # list of lists of Command
        self.subs = []  # bodies of $(...), `...`, <(...), >(...)


ANSI_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "\\": "\\", "'": "'", '"': '"', "a": "\a", "b": "\b",
                "e": "\x1b", "E": "\x1b", "f": "\f", "v": "\v", "?": "?"}


def read_balanced(s, i, open_ch="(", close_ch=")"):
    """s[i] is just past an opening paren; return (content, index past the
    matching close), skipping quoted text."""
    depth, j = 1, i
    while j < len(s):
        c = s[j]
        if c == "\\":
            j += 2
            continue
        if c == "'":
            k = s.find("'", j + 1)
            if k < 0:
                raise ParseError("unterminated quote")
            j = k + 1
            continue
        if c == '"':
            j = skip_double(s, j + 1)
            continue
        if c == "`":
            k = find_backtick(s, j + 1)
            j = k + 1
            continue
        if c == open_ch:
            depth += 1
        elif c == close_ch:
            depth -= 1
            if depth == 0:
                return s[i:j], j + 1
        j += 1
    raise ParseError("unbalanced parentheses")


def skip_double(s, j):
    while j < len(s):
        c = s[j]
        if c == "\\":
            j += 2
            continue
        if c == '"':
            return j + 1
        if c == "$" and s[j + 1 : j + 2] == "(":
            _, j = read_balanced(s, j + 2)
            continue
        if c == "`":
            j = find_backtick(s, j + 1) + 1
            continue
        j += 1
    raise ParseError("unterminated double quote")


def find_backtick(s, j):
    while j < len(s):
        if s[j] == "\\":
            j += 2
            continue
        if s[j] == "`":
            return j
        j += 1
    raise ParseError("unterminated backtick")


OPERATORS = ["&&", "||", ";;", "|&", ";", "|", "&", "(", ")", "\n"]
REDIRECTS = ["<<<", "<<-", "<<", "&>>", "&>", ">>", ">|", "<>", ">&", "<&", ">", "<"]


def parse(s):
    """Parse a shell string into pipelines of simple commands."""
    out = Parsed()
    pipeline, cmd = [], Command()
    word, has_word, dynamic, word_quoted = [], False, False, False
    redirect = None  # pending redirection operator awaiting its target word
    heredocs = []  # (delimiter, strip_tabs, quoted, command) awaiting a body
    i, n = 0, len(s)

    def end_word():
        nonlocal word, has_word, dynamic, redirect, word_quoted
        if not has_word:
            return
        text = "".join(word)
        if redirect is None:
            cmd.words.append(Word(text, dynamic))
        elif redirect in ("<<", "<<-"):
            heredocs.append((text, redirect == "<<-", word_quoted, cmd))
        elif redirect == "<<<":
            cmd.stdin.append(text)
        redirect = None
        word, has_word, dynamic, word_quoted = [], False, False, False

    def end_command():
        nonlocal cmd
        end_word()
        if cmd.words or cmd.stdin:
            pipeline.append(cmd)
        cmd = Command()

    def end_pipeline():
        nonlocal pipeline
        end_command()
        if pipeline:
            out.pipelines.append(pipeline)
        pipeline = []

    while i < n:
        c = s[i]
        if c in " \t":
            end_word()
            i += 1
            continue
        if c == "\\":
            if s[i + 1 : i + 2] == "\n":
                i += 2
                continue
            word.append(s[i + 1 : i + 2])
            has_word, word_quoted = True, True
            i += 2
            continue
        if c == "#" and not has_word:
            k = s.find("\n", i)
            i = n if k < 0 else k
            continue
        if c == "'":
            k = s.find("'", i + 1)
            if k < 0:
                raise ParseError("unterminated quote")
            word.append(s[i + 1 : k])
            has_word, word_quoted = True, True
            i = k + 1
            continue
        if c == "$" and s[i + 1 : i + 2] == "'":
            j, buf = i + 2, []
            while j < n and s[j] != "'":
                if s[j] == "\\" and j + 1 < n:
                    e = s[j + 1]
                    if e in ANSI_ESCAPES:
                        buf.append(ANSI_ESCAPES[e])
                        j += 2
                    elif e == "x":
                        m = re.match(r"[0-9A-Fa-f]{1,2}", s[j + 2 :])
                        buf.append(chr(int(m.group(0), 16)) if m else "x")
                        j += 2 + (len(m.group(0)) if m else 0)
                    elif e in "01234567":
                        m = re.match(r"[0-7]{1,3}", s[j + 1 :])
                        buf.append(chr(int(m.group(0), 8)))
                        j += 1 + len(m.group(0))
                    else:
                        buf.append(e)
                        dynamic = True
                        j += 2
                else:
                    buf.append(s[j])
                    j += 1
            if j >= n:
                raise ParseError("unterminated $'")
            word.append("".join(buf))
            has_word, word_quoted = True, True
            i = j + 1
            continue
        if c == '"':
            j = i + 1
            buf = []
            while True:
                if j >= n:
                    raise ParseError("unterminated double quote")
                d = s[j]
                if d == '"':
                    j += 1
                    break
                if d == "\\" and j + 1 < n:
                    if s[j + 1] in '$`"\\\n':
                        if s[j + 1] != "\n":
                            buf.append(s[j + 1])
                        j += 2
                        continue
                    buf.append(d)
                    j += 1
                    continue
                if d == "$" and s[j + 1 : j + 2] == "(":
                    inner, k = read_balanced(s, j + 2)
                    out.subs.append(inner)
                    buf.append(s[j:k])
                    dynamic = True
                    j = k
                    continue
                if d == "`":
                    k = find_backtick(s, j + 1)
                    out.subs.append(s[j + 1 : k])
                    buf.append(s[j : k + 1])
                    dynamic = True
                    j = k + 1
                    continue
                if d == "$" and j + 1 < n and (s[j + 1].isalnum() or s[j + 1] in "_{@*#?$!-"):
                    dynamic = True
                buf.append(d)
                j += 1
            word.append("".join(buf))
            has_word, word_quoted = True, True
            i = j
            continue
        if c == "$" and s[i + 1 : i + 2] == "(":
            if s[i + 2 : i + 3] == "(":
                inner, k = read_balanced(s, i + 3)
                k += 1  # arithmetic: the second closing paren
            else:
                inner, k = read_balanced(s, i + 2)
                out.subs.append(inner)
            word.append(s[i:k])
            has_word, dynamic = True, True
            i = k
            continue
        if c == "`":
            k = find_backtick(s, i + 1)
            out.subs.append(s[i + 1 : k])
            word.append(s[i : k + 1])
            has_word, dynamic = True, True
            i = k + 1
            continue
        if c in "<>" and s[i + 1 : i + 2] == "(":
            inner, k = read_balanced(s, i + 2)
            out.subs.append(inner)
            word.append(s[i:k])
            has_word, dynamic = True, True
            i = k
            continue
        if c == "$" and i + 1 < n and (s[i + 1].isalnum() or s[i + 1] in "_{@*#?$!-"):
            if s[i + 1] == "{":
                _, k = read_balanced(s, i + 2, "{", "}")
            else:
                m = re.match(r"[A-Za-z_][A-Za-z0-9_]*|[0-9@*#?$!-]", s[i + 1 :])
                k = i + 1 + len(m.group(0))
            word.append(s[i:k])
            has_word, dynamic = True, True
            i = k
            continue
        # redirections, with an optional fd number already collected as a word
        op = next((r for r in REDIRECTS if s.startswith(r, i)), None)
        if op:
            if has_word and re.fullmatch(r"[0-9]+", "".join(word)) and not word_quoted:
                word, has_word = [], False
            else:
                end_word()
            redirect = op
            i += len(op)
            if op in (">&", "<&"):
                m = re.match(r"\s*([0-9]+|-)", s[i:])
                if m:
                    i += len(m.group(0))
                    redirect = None
            continue
        op = next((o for o in OPERATORS if s.startswith(o, i)), None)
        if op:
            if op in ("|", "|&"):
                end_command()
            else:
                end_pipeline()
            i += len(op)
            if op == "\n" and heredocs:
                i = read_heredoc_bodies(s, i, heredocs, out)
            continue
        if c in "*?[" or (c == "{" and re.match(r"\{[^{}\s]*(,|\.\.)[^{}\s]*\}", s[i:])):
            dynamic = True
        if c == "~" and not has_word:
            dynamic = True
        word.append(c)
        has_word = True
        i += 1
    end_pipeline()
    if heredocs:  # bodies never terminated: treat everything after as the body
        for delim, _, quoted, owner in heredocs:
            owner.stdin.append("")
    return out


def read_heredoc_bodies(s, i, heredocs, out):
    for delim, strip_tabs, quoted, owner in heredocs:
        lines = []
        while i <= len(s):
            k = s.find("\n", i)
            line = s[i:] if k < 0 else s[i:k]
            i = len(s) + 1 if k < 0 else k + 1
            check = line.lstrip("\t") if strip_tabs else line
            if check == delim:
                break
            lines.append(check)
        body = "\n".join(lines)
        owner.stdin.append(body)
        if not quoted:  # an unquoted delimiter expands $(...) and backticks in the body
            try:
                out.subs.extend(parse_substitutions(body))
            except ParseError:
                out.subs.append(body)
    heredocs.clear()
    return min(i, len(s))


def parse_substitutions(text):
    """Command substitutions inside text that the shell expands (a heredoc
    with an unquoted delimiter)."""
    subs, i = [], 0
    while i < len(text):
        if text[i] == "\\":
            i += 2
            continue
        if text.startswith("$(", i) and not text.startswith("$((", i):
            inner, i = read_balanced(text, i + 2)
            subs.append(inner)
            continue
        if text[i] == "`":
            k = find_backtick(text, i + 1)
            subs.append(text[i + 1 : k])
            i = k + 1
            continue
        i += 1
    return subs


# ================================================================ unwrapping

RESERVED = {"!", "{", "}", "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "esac", "coproc"}
NOT_COMMANDS = {"for", "select", "case", "function", "[[", "]]", "in"}

# wrapper -> options that take a separate value
WRAPPERS = {
    "command": set(), "builtin": set(), "noglob": set(), "nohup": set(), "chronic": set(), "unbuffer": set(),
    "exec": {"-a"},
    "time": {"-f", "-o", "--format", "--output"},
    "sudo": {"-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T", "--user", "--group", "--chdir",
             "--host", "--prompt", "--role", "--type", "--other-user", "--command-timeout", "--close-from"},
    "doas": {"-u", "-C"},
    "nice": {"-n", "--adjustment"},
    "ionice": {"-c", "-n", "-p", "-P", "-u", "--class", "--classdata"},
    "stdbuf": {"-i", "-o", "-e", "--input", "--output", "--error"},
    "caffeinate": {"-t", "-w"},
    "timeout": {"-s", "-k", "--signal", "--kill-after"},
    "flock": {"-w", "-E", "--timeout", "--conflict-exit-code"},
}
WRAPPER_POSITIONALS = {"timeout": 1, "flock": 1}


def unwrap(words, depth):
    """Strip assignments, reserved words and wrappers. Returns
    (argv, assignments, opaque, scripts): opaque means more arguments arrive
    at run time (xargs, find -exec {}); scripts are strings the wrapper runs
    as shell code."""
    env, opaque, scripts = {}, False, []
    while words:
        w = words[0]
        head = os.path.basename(w.text)
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$", w.text, re.S)
        if m and not w.text.startswith("="):
            env[m.group(1)] = m.group(2)
            words = words[1:]
        elif w.text in RESERVED:
            words = words[1:]
        elif w.text in NOT_COMMANDS:
            return [], env, opaque, scripts
        elif head == "env":
            words = words[1:]
            while words:
                a = words[0].text
                if a in ("-S", "--split-string") and len(words) > 1:
                    scripts.append(" ".join(x.text for x in words[1:]))
                    return [], env, opaque, scripts
                if a.startswith("--split-string=") or (a.startswith("-S") and len(a) > 2):
                    rest = a.split("=", 1)[1] if a.startswith("--") else a[2:]
                    scripts.append(" ".join([rest] + [x.text for x in words[1:]]))
                    return [], env, opaque, scripts
                if a in ("-u", "--unset", "-C", "--chdir", "-P"):
                    words = words[2:]
                elif a == "--":
                    words = words[1:]
                    break
                elif a.startswith("-"):
                    words = words[1:]
                elif "=" in a:
                    k, _, v = a.partition("=")
                    env[k] = v
                    words = words[1:]
                else:
                    break
        elif head in WRAPPERS:
            if head == "command" and len(words) > 1 and words[1].text in ("-v", "-V"):
                return [], env, opaque, scripts  # a lookup, not a run
            if head == "flock" and any(x.text in ("-c", "--command") for x in words[1:]):
                k = next(idx for idx, x in enumerate(words) if x.text in ("-c", "--command"))
                if k + 1 < len(words):
                    scripts.append(words[k + 1].text)
                return [], env, opaque, scripts
            values = WRAPPERS[head]
            words = words[1:]
            while words and words[0].text.startswith("-") and words[0].text != "-":
                a = words[0].text
                if a == "--":
                    words = words[1:]
                    break
                words = words[2:] if a in values else words[1:]
            words = words[WRAPPER_POSITIONALS.get(head, 0) :]
        elif head == "watch":
            words = words[1:]
            while words and words[0].text.startswith("-"):
                words = words[2:] if words[0].text in ("-n", "--interval", "-d") and len(words) > 1 else words[1:]
            scripts.append(" ".join(x.text for x in words))
            return [], env, opaque, scripts
        elif head == "xargs":
            words = words[1:]
            with_value = {"-I", "-L", "-n", "-P", "-s", "-d", "-E", "-a", "--max-args", "--max-procs",
                          "--delimiter", "--arg-file", "--replace", "--max-lines"}
            while words and words[0].text.startswith("-"):
                a = words[0].text
                words = words[2:] if a in with_value else words[1:]
            opaque = True
        elif head == "find":
            k = next((idx for idx, x in enumerate(words) if x.text in ("-exec", "-execdir", "-ok", "-okdir")), None)
            if k is None:
                return [], env, opaque, scripts
            words = [x for x in words[k + 1 :] if x.text not in ("+", ";")]
            if any(x.text == "{}" for x in words):
                opaque = True
        elif head == "eval":
            scripts.append(" ".join(x.text for x in words[1:]))
            return [], env, opaque, scripts
        else:
            return words, env, opaque, scripts
    return [], env, opaque, scripts


def shell_invocation(argv):
    """For a shell interpreter: (inline script or None, script file or None,
    reads_stdin)."""
    i, script_mode = 1, False
    while i < len(argv):
        a = argv[i].text
        if a == "--" or a == "-":
            i += 1
            break
        if a in ("-o", "+o", "-O", "+O", "--rcfile", "--init-file"):
            i += 2
            continue
        if a.startswith("--"):
            if a == "--command":
                script_mode = True
            i += 1
            continue
        if a.startswith(("-", "+")) and len(a) > 1:
            if "c" in a[1:]:
                script_mode = True
            if "s" in a[1:]:
                return None, None, True
            i += 1
            continue
        break
    rest = argv[i:]
    if script_mode:
        return (rest[0].text if rest else ""), None, False
    if rest:
        return None, rest[0].text, False
    return None, None, True


def program_name(text):
    """The command a word runs: its basename, with zsh's =cmd path expansion undone."""
    if text.startswith("=") and len(text) > 1:
        text = text[1:]
    return os.path.basename(text)


def is_stdin_shell(cmd, depth):
    argv, _, _, _ = unwrap(cmd.words, depth)
    if not argv:
        return False
    head = program_name(argv[0].text)
    if head in SHELLS:
        _, _, reads_stdin = shell_invocation(argv)
        return reads_stdin
    if head in ("source", ".") and len(argv) > 1 and argv[1].text in ("/dev/stdin", "-"):
        return True
    return head in INTERPRETERS and (len(argv) == 1 or argv[1].text == "-")


# ================================================================ checking

COARSE = [
    (r"\bgit\b[^\n;|&]*\bpush\b[^\n;|&]*(?:^|[\s:+/'\"])(?:main|master)(?=$|[\s'\";|&)])", "a `git push` to main"),
    (r"\bgit\b[^\n;|&]*\breset\b[^\n;|&]*--hard", "`git reset --hard`"),
    (r"\bgit\b[^\n;|&]*\bclean\b[^\n;|&]*(\s-[a-zA-Z]*f|--force)", "a forced `git clean`"),
    (r"\bgh\b[^\n;|&]*\bpr\b[^\n;|&]*\bmerge\b", "`gh pr merge`"),
    (r"\bgh\b[^\n;|&]*\bapi\b[^\n;|&]*(/merges?\b|git/refs)", "a `gh api` merge or ref write"),
]


def coarse_check(text, where):
    for pattern, what in COARSE:
        if re.search(pattern, text):
            raise Blocked(f"{where} contains {what}, and code there can't be checked. Run the command directly.")


def check_script(text, cwd, depth=0):
    if depth > MAX_DEPTH:
        raise Blocked("commands nested this deeply can't be checked. Run them directly.")
    try:
        parsed = parse(text)
    except ParseError:
        coarse_check(text, "this command (which doesn't parse)")
        return cwd
    for sub in parsed.subs:
        check_script(sub, cwd, depth + 1)
    shell_vars = {}  # NAME=value assignments seen so far, to resolve $NAME as a program
    for pipeline in parsed.pipelines:
        for k, cmd in enumerate(pipeline):
            feeds_shell = any(is_stdin_shell(later, depth) for later in pipeline[k + 1 :])
            cwd = check_command(cmd, cwd, depth, feeds_shell, shell_vars)
    return cwd


def read_script_file(path, cwd):
    full = os.path.join(cwd, os.path.expanduser(path))
    try:
        if os.path.isfile(full) and os.path.getsize(full) <= MAX_SCRIPT_BYTES:
            with open(full, "r", errors="replace") as f:
                return f.read()
    except OSError:
        pass
    return None


def check_command(cmd, cwd, depth, feeds_shell, shell_vars):
    argv, env, opaque, scripts = unwrap(cmd.words, depth)
    for script in scripts:
        check_script(script, cwd, depth + 1)
    if feeds_shell:  # this command's output is run as code by a later shell
        texts = list(cmd.stdin)
        producer = os.path.basename(argv[0].text) if argv else ""
        if producer in ("echo", "printf", "cat"):
            texts.append(" ".join(w.text for w in argv[1:]))
        for t in texts:
            if producer == "printf":  # a format string can assemble any command
                coarse_check(t, "a printf piped into a shell")
            check_script(t, cwd, depth + 1)
    if not argv:
        shell_vars.update(env)
        return cwd
    prog = argv[0]
    head = program_name(prog.text)
    args = argv[1:]
    if head in ("cd", "pushd"):
        if args and not args[0].dynamic and args[0].text != "-":
            return os.path.normpath(os.path.join(cwd, os.path.expanduser(args[0].text)))
        return cwd
    if head in SHELLS:
        inline, path, reads_stdin = shell_invocation(argv)
        if inline is not None:
            check_script(inline, cwd, depth + 1)
        elif path is not None:
            body = read_script_file(path, cwd)
            if body is not None:
                check_script(body, cwd, depth + 1)
        if reads_stdin or inline is not None:
            for t in cmd.stdin:
                check_script(t, cwd, depth + 1)
        return cwd
    if head in ("source", "."):
        if args and args[0].text in ("/dev/stdin", "-"):
            for t in cmd.stdin:
                check_script(t, cwd, depth + 1)
        elif args:
            body = read_script_file(args[0].text, cwd)
            if body is not None:
                check_script(body, cwd, depth + 1)
        return cwd
    if head in INTERPRETERS:
        for t in cmd.stdin + [w.text for w in args]:
            coarse_check(t, f"code passed to {head}")
        return cwd
    if head == "git":
        check_git(args, cwd, opaque, depth, env)
        return cwd
    if head == "gh":
        check_gh(args, cwd, opaque, depth)
        return cwd
    if head in ("curl", "wget", "http", "https", "xh"):
        check_http_client(args)
        return cwd
    if prog.dynamic:  # the program itself is chosen at run time
        raw = " ".join(w.text for w in argv)
        if "IFS" in raw:  # word splitting on $IFS turns one word into a command line
            check_script(re.sub(r"\$\{?IFS\}?", " ", raw), cwd, depth + 1)
        names = set(re.findall(r"\b(git|gh)\b", prog.text))
        m = re.fullmatch(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?", prog.text)
        if m and m.group(1) in shell_vars:
            names.add(program_name(shell_vars[m.group(1)]))
        if "git" in names:
            check_git(args, cwd, opaque, depth, env)
        if "gh" in names:
            check_gh(args, cwd, opaque, depth)
        return cwd
    if "/" in prog.text:  # a script run by path
        body = read_script_file(prog.text, cwd)
        if body is not None and re.match(r"#!\s*\S*(/|env\s+)(ba|z|da|k)?sh\b", body):
            check_script(body, cwd, depth + 1)
    return cwd


def check_env_identity(env):
    for name in ("GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"):
        value = env.get(name)
        if value is not None and not value.endswith(NOREPLY):
            raise Blocked(f"{name} is set to '{value}', not a GitHub noreply address.")


# ---------------------------------------------------------------- git

GIT_GLOBAL_WITH_VALUE = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env", "--super-prefix"}
GIT_BUILTINS = {
    "add", "am", "annotate", "apply", "archive", "bisect", "blame", "branch", "bundle", "cat-file", "check-ignore",
    "checkout", "cherry", "cherry-pick", "clean", "clone", "commit", "commit-tree", "config", "count-objects",
    "describe", "diff", "diff-files", "diff-index", "diff-tree", "difftool", "fetch", "for-each-ref",
    "format-patch", "fsck", "gc", "grep", "hash-object", "help", "init", "log", "ls-files", "ls-remote", "ls-tree",
    "maintenance", "merge", "merge-base", "merge-file", "merge-tree", "mergetool", "mv", "name-rev", "notes",
    "pull", "push", "range-diff", "read-tree", "rebase", "reflog", "remote", "repack", "replace", "rerere", "reset",
    "restore", "rev-list", "rev-parse", "revert", "rm", "send-pack", "shortlog", "show", "show-branch", "show-ref",
    "sparse-checkout", "stash", "status", "submodule", "subtree", "switch", "symbolic-ref", "tag", "update-index",
    "update-ref", "var", "verify-commit", "verify-tag", "version", "whatchanged", "worktree", "write-tree",
}
COMMITTING = {"commit", "merge", "pull", "cherry-pick", "revert", "am", "rebase", "commit-tree"}


def check_git(args, cwd, opaque, depth, env):
    globals_, i = [], 0
    while i < len(args) and args[i].text.startswith("-"):
        a = args[i].text
        if a in GIT_GLOBAL_WITH_VALUE:
            value = args[i + 1].text if i + 1 < len(args) else ""
            if a == "-C":
                if not args[i + 1 : i + 2] or not args[i + 1].dynamic:  # $(pwd) and the like: keep cwd
                    cwd = os.path.normpath(os.path.join(cwd, os.path.expanduser(value)))
            else:
                globals_ += [a, value]
            i += 2
        else:
            if a.startswith(("--git-dir=", "--work-tree=", "--namespace=", "--config-env=")):
                globals_.append(a)
            i += 1
    if i >= len(args):
        return
    git = ["git"] + globals_
    sub, rest = args[i], args[i + 1 :]
    if sub.dynamic:  # subcommand chosen at run time: apply every check that could fire
        for name in ("push", "reset", "clean"):
            check_git_sub(name, rest, cwd, git, opaque, depth, env)
        return
    check_git_sub(sub.text, rest, cwd, git, opaque, depth, env)


def check_git_sub(sub, rest, cwd, git, opaque, depth, env):
    if sub not in GIT_BUILTINS:
        alias = run(git + ["config", "--get", f"alias.{sub}"], cwd)
        if alias:
            if depth > MAX_DEPTH:
                raise Blocked("git alias chain too deep to check.")
            tail = " ".join(shlex.quote(w.text) for w in rest)
            if alias.startswith("!"):
                check_script(alias[1:] + " " + tail, cwd, depth + 1)
            else:
                check_script(" ".join(shlex.quote(x) for x in git) + " " + alias + " " + tail, cwd, depth + 1)
        return
    if sub in ("push", "send-pack"):
        check_push(rest, cwd, git, opaque)
    elif sub == "subtree" and rest and rest[0].text == "push":
        tail, k = [], 1
        while k < len(rest):
            if rest[k].text in ("-P", "--prefix", "-m", "--message"):
                k += 2
                continue
            if not rest[k].text.startswith(("--prefix=", "--message=")):
                tail.append(rest[k])
            k += 1
        check_push(tail, cwd, git, opaque)
    elif sub == "reset":
        options = rest[: next((k for k, w in enumerate(rest) if w.text == "--"), len(rest))]
        if opaque or any(w.text == "--hard" for w in options):
            raise Blocked("`git reset --hard` discards work. Use `git stash`, or `git restore <path>` for specific files.")
    elif sub == "clean":
        check_clean(rest, cwd, git, opaque)
    if sub in COMMITTING:
        check_identity(rest, cwd, git, env)


def check_identity(rest, cwd, git, env):
    for i, w in enumerate(rest):
        author = None
        if w.text == "--author" and i + 1 < len(rest):
            author = rest[i + 1].text
        elif w.text.startswith("--author="):
            author = w.text.split("=", 1)[1]
        if author is not None:
            m = re.search(r"<([^>]*)>", author)
            if not m or not m.group(1).endswith(NOREPLY):
                raise Blocked(f"--author '{author}' isn't a GitHub noreply address.")
    check_env_identity(os.environ)
    check_env_identity(env)
    email = run(git + ["config", "user.email"], cwd) or ""
    if not email.endswith(NOREPLY):
        raise Blocked(
            f"git user.email is '{email}', not a GitHub noreply address. "
            f"Set it with `git config user.email <id+login>{NOREPLY}` first."
        )


def check_clean(rest, cwd, git, opaque):
    force = dry = False
    i = 0
    while i < len(rest):
        a = rest[i].text
        if a == "--":
            break
        if a.startswith("--"):
            name = a.split("=", 1)[0]
            if name == "--force":
                force = True
            elif name == "--dry-run":
                dry = True
            elif name == "--exclude" and "=" not in a:
                i += 1
        elif a.startswith("-") and len(a) > 1:
            for k, ch in enumerate(a[1:]):
                if ch == "f":
                    force = True
                elif ch == "n":
                    dry = True
                elif ch == "e":
                    if k == len(a) - 2:
                        i += 1  # the pattern is the next word
                    break  # the rest of the cluster is the pattern
        i += 1
    if dry and not opaque:
        return
    if not force:
        require = run(git + ["config", "--type=bool", "--get", "clean.requireForce"], cwd)
        force = require == "false"
    if force or opaque:
        raise Blocked("`git clean` that deletes files is blocked. Run `git clean -n` and remove files by name.")


def check_push(rest, cwd, git, opaque):
    if opaque:
        raise Blocked("`git push` with arguments supplied at run time (xargs, find -exec) can't be checked. Run it directly.")
    positionals, deleting, repo_option, end = [], False, False, False
    i = 0
    while i < len(rest):
        w = rest[i]
        a = w.text
        if end or not a.startswith("-") or a == "-":
            positionals.append(w)
        elif a == "--":
            end = True
        elif a.startswith("--"):
            name, eq, _ = a.partition("=")
            if name == "--dry-run":
                return
            if name in ("--all", "--mirror", "--branches"):
                raise Blocked(f"`git push {name}` would update main. Push only your branch.")
            if name == "--delete":
                deleting = True
            elif name == "--repo":
                repo_option = True
                if not eq:
                    if i + 1 < len(rest):
                        positionals.append(rest[i + 1])  # check it like a refspec too
                    i += 1
            elif name in ("--push-option", "--receive-pack", "--exec") and not eq:
                i += 1
        else:
            letters = a[1:]
            for k, ch in enumerate(letters):
                if ch == "n":
                    return
                if ch == "d":
                    deleting = True
                if ch == "o":
                    if k == len(letters) - 1:
                        i += 1
                    break
        i += 1
    for k, w in enumerate(positionals):
        if CURRENT_BRANCH_SUB.match(w.text):
            positionals[k] = Word("HEAD")  # the usual way to name the current branch
        elif w.dynamic or "@{" in w.text:
            raise Blocked(f"`git push` argument '{w.text}' is computed at run time. Write the branch name, or HEAD.")
    # With --repo every positional is checked; otherwise the first is the remote.
    refspecs = positionals if repo_option else positionals[1:]
    remote = None if repo_option or not positionals else positionals[0].text
    if not refspecs:
        if deleting:
            return
        for dst in implicit_destinations(cwd, git, remote):
            if dst in PROTECTED:
                raise Blocked(f"this `git push` would update {dst} (from your branch or push config). "
                              "Push an explicit refspec such as `git push -u origin HEAD`.")
        return
    branch = None
    for w in refspecs:
        spec = w.text
        force_spec = spec.startswith("+")
        spec = spec[1:] if force_spec else spec
        if spec == ":":
            raise Blocked("`git push <remote> :` pushes every matching branch, main included.")
        if deleting:
            src, dst = "", spec
        elif ":" in spec:
            src, dst = spec.split(":", 1)
        else:
            src, dst = spec, spec
        if dst in ("HEAD", "@"):
            if branch is None:
                branch = current_branch(cwd, git) or "HEAD"
            dst = branch
        dst = normalize_ref(dst)
        if dst in PROTECTED or "*" in dst:
            raise Blocked(f"`git push` to {dst} is not allowed. Push your branch and open a PR.")
        if src == "":
            check_no_dependents(dst, cwd, "delete")


CURRENT_BRANCH_SUB = re.compile(
    r"^\$\(\s*git\s+(branch\s+--show-current|rev-parse\s+--abbrev-ref\s+HEAD|symbolic-ref\s+(-q\s+)?--short\s+(-q\s+)?HEAD)\s*\)$"
)


def normalize_ref(ref):
    ref = re.sub(r"/+", "/", ref.strip()).lstrip("/")
    return ref[len("refs/heads/") :] if ref.startswith("refs/heads/") else ref


def implicit_destinations(cwd, git, remote):
    """Branches a push without refspecs would update."""
    branch = current_branch(cwd, git)
    remote = remote or (branch and run(git + ["config", "--get", f"branch.{branch}.pushRemote"], cwd)) \
        or run(git + ["config", "--get", "remote.pushDefault"], cwd) \
        or (branch and run(git + ["config", "--get", f"branch.{branch}.remote"], cwd)) or "origin"
    if run(git + ["config", "--type=bool", "--get", f"remote.{remote}.mirror"], cwd) == "true":
        return ["main"]  # a mirror remote gets every branch
    mapped = run(git + ["config", "--get-all", f"remote.{remote}.push"], cwd)
    if mapped:
        dsts = []
        for spec in mapped.splitlines():
            spec = spec.lstrip("+")
            src, _, dst = spec.partition(":")
            dst = dst or src
            if dst in ("HEAD", "@"):
                dst = branch or "HEAD"
            dsts.append(normalize_ref(dst) if "*" not in dst else "main")
        return dsts
    mode = run(git + ["config", "--get", "push.default"], cwd) or "simple"
    if mode == "matching":
        local = run(git + ["for-each-ref", "--format=%(refname:short)", "refs/heads"], cwd) or ""
        return [b for b in local.splitlines() if b in PROTECTED] or []
    if mode in ("upstream", "tracking"):
        merge = branch and run(git + ["config", "--get", f"branch.{branch}.merge"], cwd)
        return [normalize_ref(merge)] if merge else []
    return [branch] if branch else []


def current_branch(cwd, git=("git",)):
    return run(list(git) + ["symbolic-ref", "--short", "-q", "HEAD"], cwd)


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


def check_http_client(args):
    """curl and friends against the GitHub API: the same writes `gh api` may not make."""
    texts = [w.text for w in args]
    joined = " ".join(texts)
    urls = [t for t in texts if "api.github.com" in t]
    if not urls:
        return
    method = None
    for k, t in enumerate(texts):
        if t in ("-X", "--request", "--method") and k + 1 < len(texts):
            method = texts[k + 1].upper()
        elif t.startswith(("-X", "--request=", "--method=")) and len(t) > 2:
            method = t.split("=", 1)[1].upper() if "=" in t else t[2:].upper()
    data = any(t.startswith(("-d", "--data", "--json", "-F", "--form", "-T", "--upload-file", "--post-data",
                             "--post-file", "--body-data")) for t in texts)
    write = (method or ("POST" if data else "GET")) not in ("GET", "HEAD")
    for url in urls:
        path = re.sub(r"^.*api\.github\.com", "", url).split("?", 1)[0].rstrip("/")
        if path.endswith("/graphql") and GRAPHQL_WRITES.search(joined):
            raise Blocked("GraphQL mutations that merge PRs or move refs are blocked.")
        if write and API_WRITE_PATHS.search(path):
            raise Blocked("GitHub API writes that merge PRs or move, delete or write branches are blocked. "
                          "Use `gh pr merge --merge` or `git push`, which are checked.")


# ---------------------------------------------------------------- gh

GH_COMMANDS = {
    "alias", "api", "attestation", "auth", "browse", "cache", "co", "codespace", "completion", "config",
    "copilot", "extension", "gist", "gpg-key", "help", "issue", "label", "org", "pr", "preview", "project",
    "release", "repo", "ruleset", "run", "search", "secret", "ssh-key", "status", "variable", "workflow",
}
GH_FLAGS_WITH_VALUE = {"-R", "--repo", "-b", "--body", "-F", "--body-file", "-t", "--subject", "-A",
                       "--author-email", "--match-head-commit", "--hostname"}
GRAPHQL_WRITES = re.compile(r"\b(mergePullRequest|enablePullRequestAutoMerge|updateRefs?|deleteRef|createRef|"
                            r"createCommitOnBranch|mergeBranch|updatePullRequestBranch)\b")
API_WRITE_PATHS = re.compile(r"(^|/)(pulls/[^/]+/merge|merges|git/refs(/.*)?|contents/.*|branches/[^/]+/rename)$")


def check_gh(args, cwd, opaque, depth):
    texts = [w.text for w in args]
    positionals, flags = [], []  # flags: (name, value or None)
    i = 0
    while i < len(texts):
        a = texts[i]
        if a.startswith("--") and len(a) > 2:
            name, eq, value = a.partition("=")
            if not eq and name in GH_FLAGS_WITH_VALUE:
                value = texts[i + 1] if i + 1 < len(texts) else ""
                i += 1
            flags.append((name, value if (eq or name in GH_FLAGS_WITH_VALUE) else None))
        elif a.startswith("-") and len(a) > 1:
            letters = a[1:]
            for k, ch in enumerate(letters):
                flag = "-" + ch
                if flag in GH_FLAGS_WITH_VALUE:
                    value = letters[k + 1 :].lstrip("=")
                    if not value:
                        value = texts[i + 1] if i + 1 < len(texts) else ""
                        i += 1
                    flags.append((flag, value))
                    break
                flags.append((flag, None))
        else:
            positionals.append(args[i])
            if len(positionals) == 1 and a == "api":
                check_gh_api(args[i + 1 :], cwd)
                return
            if len(positionals) == 1 and a not in GH_COMMANDS:
                check_gh_alias(a, texts[i + 1 :], cwd, depth)
                return
        i += 1
    words = [w.text for w in positionals]
    if words[:2] == ["repo", "sync"] and len(words) > 2:
        raise Blocked("`gh repo sync <repo>` rewrites a remote branch. Ask the maintainer.")
    if words[:2] != ["pr", "merge"]:
        return
    if opaque:
        raise Blocked("`gh pr merge` with arguments supplied at run time can't be checked.")
    repo, delete_branch = [], False
    for name, value in flags:
        on = value is None or value.lower() not in ("false", "0")
        if name == "--admin" and on:
            raise Blocked("`gh pr merge --admin` bypasses branch protection and review.")
        if name == "--auto" and on:
            raise Blocked("`gh pr merge --auto` merges later, after the review check. Merge directly once reviewed.")
        if name in ("--squash", "-s", "--rebase", "-r") and on:
            raise Blocked("use merge commits (`gh pr merge --merge`) so stacked PRs keep their base history.")
        if name in ("--delete-branch", "-d") and on:
            delete_branch = True
        if name in ("-R", "--repo"):
            repo = ["--repo", value or ""]
    target = positionals[2] if len(positionals) > 2 else None
    if target is not None and target.dynamic:
        raise Blocked("`gh pr merge` target is computed at run time. Write the PR number.")
    reason = review_check(target.text if target else None, repo, cwd)
    if reason:
        raise Blocked(f"PR can't merge yet: {reason}")
    if delete_branch:
        head = run(["gh", "pr", "view"] + ([target.text] if target else []) + repo
                   + ["--json", "headRefName", "-q", ".headRefName"], cwd)
        if not head:
            raise Blocked("couldn't read the PR's head branch, so --delete-branch can't be checked.")
        check_no_dependents(head, cwd, "delete")


def check_gh_alias(name, extra, cwd, depth):
    expansion = gh_alias(name, cwd)
    if not expansion or depth > MAX_DEPTH:
        return
    used = set()

    def sub(m):
        n = int(m.group(1))
        used.add(n)
        return shlex.quote(extra[n - 1]) if n <= len(extra) else ""

    body = re.sub(r"\$([1-9])", sub, expansion.lstrip("!"))
    tail = " ".join(shlex.quote(x) for idx, x in enumerate(extra, 1) if idx not in used)
    script = (body if expansion.startswith("!") else "gh " + body) + " " + tail
    check_script(script, cwd, depth + 1)


def gh_alias(name, cwd):
    out = run(["gh", "alias", "list"], cwd)
    for line in (out or "").splitlines():
        key, sep, value = line.partition(":")
        if sep and key.strip() == name:
            return value.strip().strip("'\"")
    return None


def check_gh_api(args, cwd):
    method, endpoint, fields, texts = None, None, False, []
    i = 0
    while i < len(args):
        a = args[i].text
        nxt = args[i + 1].text if i + 1 < len(args) else ""
        if a in ("-X", "--method"):
            method, i = nxt.upper(), i + 2
            continue
        if a.startswith("--method="):
            method = a.split("=", 1)[1].upper()
        elif a.startswith("-X") and len(a) > 2:
            method = a[2:].lstrip("=").upper()
        elif a in ("-f", "-F", "--field", "--raw-field", "--input"):
            fields = True
            texts.append(nxt)
            if a in ("-F", "--field", "--input"):
                path = nxt.split("=", 1)[1] if "=" in nxt and a != "--input" else nxt
                body = read_script_file(path.lstrip("@"), cwd) if path.startswith("@") or a == "--input" else None
                if body:
                    texts.append(body)
            i += 2
            continue
        elif a in ("-H", "--header", "-q", "--jq", "-t", "--template", "--cache", "--hostname", "-p", "--preview"):
            i += 2
            continue
        elif not a.startswith("-") and endpoint is None:
            endpoint = args[i]
        else:
            texts.append(a)
        i += 1
    if endpoint is None:
        return
    if endpoint.text == "graphql":
        if GRAPHQL_WRITES.search(" ".join(texts)):
            raise Blocked("GraphQL mutations that merge PRs or move refs are blocked. Use `gh pr merge --merge`.")
        return
    write = (method or ("POST" if fields else "GET")) not in ("GET", "HEAD")
    path = endpoint.text.split("?", 1)[0].rstrip("/")
    if write and (endpoint.dynamic or API_WRITE_PATHS.search(path)):
        raise Blocked("`gh api` writes that merge PRs or move, delete or write branches are blocked. "
                      "Use `gh pr merge --merge` or `git push`, which are checked.")


# ---------------------------------------------------------------- merge review rule

def merge_verdict(head, reviews, parents, base_ancestor, changes_at):
    """Decide whether a PR may merge. Returns None, or the reason it can't.

    - head: the PR's head commit
    - reviews: [{"commit": sha, "submittedAt": iso, "state": str, "body": str}]
    - parents: {sha: [parent shas]} for the PR's commits
    - base_ancestor(sha): True if sha is already in the base branch
    - changes_at(sha): a fingerprint of the PR's own changes (base...sha), or
      None if it can't be computed

    A review covers the head when every commit after the reviewed one, along
    the first-parent chain, is a merge of commits already in the base branch,
    and those merges left the PR's own changes exactly as reviewed. What the
    review says is for the maintainer to weigh; the guard only checks that one
    covers the code being merged.
    """
    reviewed = {r["commit"] for r in reviews if r.get("commit")}
    if not reviewed:
        return "it has no review. The cross-review posts one (see the katwork-review skill), then the maintainer merges."
    chain, sha = [], head
    while sha not in reviewed:
        ps = parents.get(sha)
        if not ps:
            return (f"no review covers its head {head[:7]}: the commits after the last review "
                    "aren't all merges of the base branch. Ask for a review of the delta.")
        if len(ps) < 2:
            return (f"commit {sha[:7]} came after the last review and isn't a merge of the base branch. "
                    "Ask for a review of the delta.")
        for extra in ps[1:]:
            if not base_ancestor(extra):
                return f"merge {sha[:7]} after the last review brings in {extra[:7]}, which isn't in the base branch."
        chain.append(sha)
        sha = ps[0]
    covered = sha
    if chain:
        before, after = changes_at(covered), changes_at(head)
        if before is None or after is None:
            return "couldn't compare the PR's changes before and after the base merges. Ask for a review on the head."
        if before != after:
            return (f"merging the base branch after review {covered[:7]} changed the PR's own diff "
                    "(a conflict resolution, say). Ask for a review of the merge.")
    return None


def review_check(target, repo, cwd):
    view = ["gh", "pr", "view"] + ([target] if target else []) + repo
    out = run(view + ["--json", "number,url,headRefOid,baseRefName,reviews"], cwd)
    if out is None:
        return "couldn't read the PR with `gh pr view`, so the merge can't be checked."
    pr = json.loads(out)
    m = re.search(r"github\.com/([^/]+/[^/]+)/pull/(\d+)", pr.get("url", ""))
    if not m:
        return "couldn't tell which repository the PR is in."
    slug, number, base = m.group(1), m.group(2), pr.get("baseRefName", "")
    reviews = [{"commit": (r.get("commit") or {}).get("oid"), "submittedAt": r.get("submittedAt"),
                "state": r.get("state"), "body": r.get("body")} for r in pr.get("reviews", [])]
    lines = run(["gh", "api", "--paginate", f"repos/{slug}/pulls/{number}/commits",
                 "--jq", ".[] | {sha, parents: [.parents[].sha]}"], cwd)
    if lines is None:
        return "couldn't list the PR's commits, so the merge can't be checked."
    parents = {}
    for line in lines.splitlines():
        if line.strip():
            c = json.loads(line)
            parents[c["sha"]] = c["parents"]

    def base_ancestor(sha):
        behind = run(["gh", "api", f"repos/{slug}/compare/{sha}...{base}", "--jq", ".behind_by"], cwd)
        return behind == "0"

    def changes_at(sha):
        out = run(["gh", "api", f"repos/{slug}/compare/{base}...{sha}", "--jq", ".files"], cwd, timeout=40)
        return changes_fingerprint(out)

    return merge_verdict(pr.get("headRefOid", ""), reviews, parents, base_ancestor, changes_at)


def changes_fingerprint(files_json):
    """The +/- lines of each changed file, which merges of unrelated base
    changes leave alone. None when GitHub omits a patch or truncates the list."""
    if files_json is None:
        return None
    try:
        files = json.loads(files_json) or []
    except ValueError:
        return None
    if len(files) >= 300:
        return None
    result = []
    for f in files:
        patch = f.get("patch")
        if patch is None:
            if f.get("status") == "removed":
                result.append((f.get("filename"), "removed"))
                continue
            return None
        lines = tuple(l for l in patch.splitlines() if l[:1] in "+-" and not l.startswith(("+++", "---")))
        result.append((f.get("filename"), f.get("status"), lines))
    return tuple(sorted(result, key=lambda x: x[0] or ""))


# ================================================================ entry


def command_text(payload):
    cmd = (payload.get("tool_input") or {}).get("command", "")
    if isinstance(cmd, list):
        return " ".join(shlex.quote(str(x)) for x in cmd)
    return str(cmd)


def decide(payload):
    """Return None to allow, or the reason to block."""
    cwd = payload.get("cwd") or os.getcwd()
    try:
        check_script(command_text(payload), cwd)
    except Blocked as e:
        return str(e)
    except RecursionError:
        return "this command nests too deeply to check."
    return None


def main():
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    try:
        payload = json.load(sys.stdin)
    except ValueError:
        print("BLOCKED: guard.py got hook input that isn't JSON.", file=sys.stderr)
        return 2
    try:
        reason = decide(payload)
    except Exception as e:  # a guard bug must not wave a command through
        reason = f"the guard failed ({type(e).__name__}: {e}); run `.claude/hooks/guard.py --self-test`."
    if reason:
        print(f"BLOCKED by .claude/hooks/guard.py: {reason}", file=sys.stderr)
        return 2
    return 0


# ================================================================ self-test


def self_test():
    """Offline: no network, no `gh`. Scratch repos stand in for this one."""
    import tempfile

    empty = tempfile.mkstemp()[1]
    os.environ.update(GIT_CONFIG_GLOBAL=empty, GIT_CONFIG_NOSYSTEM="1")  # ignore the machine's git config

    def git(*a, cwd):
        subprocess.run(["git", *a], cwd=cwd, check=True, capture_output=True)

    remote = tempfile.mkdtemp()
    git("init", "-q", "--bare", "-b", "main", ".", cwd=remote)
    repo = tempfile.mkdtemp()  # on branch work, tracking origin/main as new task branches do
    git("init", "-q", "-b", "main", ".", cwd=repo)
    git("config", "user.email", "1+bot" + NOREPLY, cwd=repo)
    git("config", "user.name", "bot", cwd=repo)
    git("commit", "-q", "--allow-empty", "-m", "init", cwd=repo)
    git("remote", "add", "origin", remote, cwd=repo)
    git("push", "-q", "origin", "main", cwd=repo)
    git("switch", "-q", "-c", "work", "--track", "origin/main", cwd=repo)
    git("config", "alias.ship", "push origin HEAD:main", cwd=repo)
    git("config", "alias.st", "status", cwd=repo)
    on_main = tempfile.mkdtemp()
    git("init", "-q", "-b", "main", ".", cwd=on_main)
    scripts = os.path.join(repo, "tools")
    os.makedirs(scripts)
    with open(os.path.join(scripts, "ship.sh"), "w") as f:
        f.write("#!/bin/bash\nset -e\ngit push origin main\n")
    with open(os.path.join(scripts, "fine.sh"), "w") as f:
        f.write("#!/usr/bin/env bash\ngit status\n")

    blocked = [
        # push to main, every spelling
        "git push origin main", "git push origin master", "git push -f origin main", "git push --force origin main",
        "git push --force-with-lease origin HEAD:main", "git push origin HEAD:main", "git push origin @:main",
        "git push origin +main", "git push origin +HEAD:main", "git push origin refs/heads/main",
        "git push origin HEAD:refs/heads/main", "git push origin HEAD:refs/heads//main",
        "git push origin feature:refs/heads/master", "git push origin :main", "git push origin --delete main",
        "git push -d origin master", "git push --all origin", "git push --mirror origin", "git push origin :",
        "git push origin 'refs/heads/*:refs/heads/*'", "git push origin \"main\"", "git push origin 'HEAD:main'",
        "git push origin ma\"in\"", "git push origin $'main'", "git push origin $'\\x6dain'", "git push origin m\\ain",
        "git push -u origin main", "git push origin -- main", "git push -fu origin main",
        "git push -o ci.skip origin main", "git push --push-option ci.skip origin main",
        # --repo (review finding 1)
        "git push --repo=origin HEAD:main", "git push --repo origin HEAD:main",
        "git push -f --repo=origin HEAD:refs/heads/main", "git push --repo=origin --delete main",
        # run-time refspecs
        "B=main; git push origin $B", "git push origin ${BRANCH}", "git push origin \"$(echo main)\"",
        "git push origin {main,x}", "git push origin `echo main`", "git push origin @{u}",
        # implicit destinations
        f"cd {on_main} && git push", f"git -C {on_main} push", f"git -C {on_main} push -u origin HEAD",
        f"git -C {on_main} push origin @", "git -c push.default=upstream push", "git -c push.default=upstream push origin",
        "git -c remote.origin.push=HEAD:refs/heads/main push", "git -c remote.origin.push=HEAD:refs/heads/main push origin",
        "git -c remote.origin.push=refs/heads/*:refs/heads/* push", "git -c push.default=matching push",
        "git -c remote.origin.mirror=true push", f"git -C {on_main} push origin \"$(git branch --show-current)\"",
        # git options before push
        "git -C . push origin main", "git --no-pager push origin main", "git -c color.ui=never push origin main",
        "git --git-dir=.git push origin main", "/usr/bin/git push origin main", "\\git push origin main",
        "\"git\" push origin main",
        # aliases
        "git -c alias.p=push p origin main", "git -c 'alias.pm=!git push origin main' pm", "git ship",
        "git -c alias.x=ship x",
        # wrappers and prefixes
        "FOO=1 git push origin main", "env git push origin main", "env -i PATH=/usr/bin git push origin main",
        "env -S 'git push origin main'", "command git push origin main", "builtin command git push origin main",
        "exec git push origin main", "nohup git push origin main", "time git push origin main",
        "sudo git push origin main", "sudo -u me git push origin main", "timeout 30 git push origin main",
        "timeout -s KILL 30s git push origin main", "nice -n 5 git push origin main", "noglob git push origin main",
        "echo main | xargs git push origin", "xargs -n1 git push origin < branches.txt",
        "find . -maxdepth 0 -exec git push origin main \\;", "watch -n 5 git push origin main",
        "flock /tmp/l git push origin main", "flock /tmp/l -c 'git push origin main'",
        # shells, eval, substitutions
        "bash -c 'git push origin main'", "bash -lc 'git push origin main'", "bash -e -c 'git push origin main'",
        "bash -o pipefail -c 'git push origin main'", "sh -c \"git push origin main\"", "zsh -c 'git push origin main'",
        "eval 'git push origin main'", "eval \"git push origin main\"", "echo $(git push origin main)",
        "echo \"$(git push origin main)\"", "echo `git push origin main`", "cat <(git push origin main)",
        "x=$(git push origin main)", "(git push origin main)", "{ git push origin main; }",
        "if true; then git push origin main; fi", "for b in main; do git push origin $b; done",
        "true; git push origin main", "true && git push origin main", "false || git push origin main",
        "git status | git push origin main", "git status & git push origin main", "echo ok\ngit push origin main",
        "git push origin main # comment", "bash -c 'bash -c \"git push origin main\"'",
        # heredocs and here-strings fed to code (review finding 2)
        "bash <<'EOF'\ngit push origin main\nEOF", "bash <<'EOF'\ngit reset --hard\nEOF",
        "bash <<'EOF'\ngit clean -fd\nEOF", "bash <<EOF\ngit push origin main\nEOF", "sh -s <<EOF\ngit push origin main\nEOF",
        "bash - <<'X'\ngit push origin main\nX", "bash <<-EOF\n\tgit push origin main\n\tEOF",
        "cat <<'EOF' | bash\ngit push origin main\nEOF", "bash <<< 'git push origin main'",
        "echo 'git push origin main' | bash", "printf 'git push origin %s\\n' main | sh",
        "source /dev/stdin <<'EOF'\ngit push origin main\nEOF", "cat <<EOF\n$(git push origin main)\nEOF",
        "python3 - <<'EOF'\nimport os\nos.system('git push origin main')\nEOF",
        "python3 -c \"import os; os.system('git push origin main')\"",
        "bash tools/ship.sh", "tools/ship.sh", "./tools/ship.sh", "source tools/ship.sh", ". tools/ship.sh",
        # program spellings and plumbing
        "=git push origin main", "git${IFS}push${IFS}origin${IFS}main", "g=git; $g push origin main",
        "\"$(which git)\" push origin main", "git\tpush\torigin\tmain", "git push \\\n origin main",
        "git send-pack ../remote.git main", "git subtree push --prefix=docs origin main",
        "git -c 'alias.x=!f() { git push origin main; }; f' x", f"cd {on_main} && git -C \"$(pwd)\" push",
        "curl -X PUT -H 'Authorization: token x' https://api.github.com/repos/o/r/pulls/12/merge",
        "curl -X DELETE https://api.github.com/repos/o/r/git/refs/heads/feature",
        "curl -d '{\"base\":\"main\"}' https://api.github.com/repos/o/r/merges",
        # reset and clean (review finding 3)
        "git reset --hard", "git reset --hard origin/main", "git reset -q --hard HEAD", "git -C . reset --hard",
        "git clean -fd", "git clean -xdf", "git clean -f", "git clean -ffdx", "git clean --force -d",
        "git clean -f -- -notes", "git clean -f -- -n", "git clean -fe '*.log'", "git clean -f -e -n",
        "git clean -d -f --exclude=-n", "git -c clean.requireForce=false clean -d",
        "git -c clean.requireForce=false clean", "echo x | xargs git clean",
        # identity
        "git commit --author='X <x@example.com>' -m x", "GIT_AUTHOR_EMAIL=x@example.com git commit -m x",
        "env GIT_COMMITTER_EMAIL=x@example.com git commit -m x", "git -c user.email=x@example.com commit -m x",
        "git -c user.email=x@example.com merge origin/main",
        # gh merges
        "gh pr merge 12 --admin --merge", "gh pr merge 12 --admin=true", "gh pr merge 12 --squash",
        "gh pr merge 12 -s", "gh pr merge 12 -sd", "gh pr merge 12 -ds", "gh pr merge --rebase", "gh pr merge 12 -r",
        "gh pr merge 12 --auto --merge", "gh --repo o/r pr merge 1 --squash", "gh pr merge -R o/r 1 --admin",
        "gh pr merge -Ro/r 1 --admin", "gh pr merge 12 --squash=true", "gh pr merge $PR --merge",
        "env gh pr merge 1 --squash", "bash -c 'gh pr merge 1 --admin'",
        # gh api writes
        "gh api -X PUT repos/o/r/pulls/12/merge", "gh api --method=PUT repos/o/r/pulls/12/merge",
        "gh api -XPUT repos/o/r/pulls/12/merge", "gh api repos/o/r/pulls/12/merge -X PUT",
        "gh api --method DELETE repos/o/r/git/refs/heads/feature", "gh api -XDELETE repos/o/r/git/refs/heads/x",
        "gh api -X PATCH repos/o/r/git/refs/heads/main -f sha=abc", "gh api repos/o/r/git/refs -f ref=refs/heads/x -f sha=y",
        "gh api repos/o/r/merges -f base=main -f head=x", "gh api -X PUT repos/o/r/contents/a.txt -f message=m",
        "gh api graphql -f query='mutation { mergePullRequest(input: {pullRequestId: \"x\"}) { clientMutationId } }'",
        "gh api graphql -f query='mutation { updateRef(input: {}) { clientMutationId } }'",
        "gh repo sync o/r --branch main",
    ]
    allowed = [
        "git push origin sp1/task-3-auth", "git push -u origin HEAD", "git push -u origin @",
        "git push --force-with-lease origin sp1/task-4-tenancy", "git push origin main:feature",
        "git push origin HEAD:refs/heads/feature", "git push --dry-run origin main", "git push -n origin main",
        "git push origin --tags", "git push origin v1.0", "git push origin feature-x",
        "git push -u origin \"$(git branch --show-current)\"", "git push origin $(git rev-parse --abbrev-ref HEAD)",
        "git push", "git push origin", "git -c push.default=current push", "git push --repo=origin",
        "git status && git log --oneline -5", "git st", "git -C . status", "git log origin/main..HEAD",
        "git diff main...HEAD", "git fetch origin main", "git merge origin/main", "git rebase origin/main",
        "git switch main", "git checkout -b sp1/task-9-events origin/main", "git branch -D old",
        "git commit -m 'never git push origin main'", "git commit -m \"docs: mention git push origin main\"",
        "git commit -m 'run `git push origin main` never'", "git commit --author='Bot <1+bot@users.noreply.github.com>' -m x",
        "gh pr create --body-file - <<'EOF'\nNever run git push origin main or git reset --hard.\nEOF",
        "cat <<'EOF' > notes.md\ngit push origin main\nEOF", "git commit -F - <<'EOF'\nfix: guard git push origin main\nEOF",
        "echo 'git push origin main'", "echo git push origin main", "printf '%s\\n' 'git reset --hard'",
        "grep -n 'git push origin main' docs/x.md", "rg 'gh pr merge' .", "echo main | grep main",
        "git clean -n", "git clean -n -d", "git clean -nd", "git clean -fn", "git clean --dry-run -f",
        "git clean -f -n -- x", "git clean -d", "git reset --soft HEAD~1", "git reset HEAD file", "git reset -- --hard",
        "cargo test --workspace --locked", "bash -c 'cargo test'", "bash tools/fine.sh", "tools/fine.sh",
        "scripts/test-db.sh start", "env FOO=1 cargo test", "timeout 60 cargo test", "command -v git",
        "gh pr view 12 --json reviews", "gh pr list --base main", "gh pr checks 12", "gh pr diff 12",
        "gh api repos/o/r/pulls/12", "gh api repos/o/r/pulls/12/merge", "gh api repos/o/r/git/refs/heads/main",
        "gh api graphql -f query='query { viewer { login } }'", "gh pr comment 12 --body 'gh pr merge later'",
        "gh repo sync", "gh issue list", "gh pr review 12 --comment --body-file r.md",
        "for f in a b; do echo $f; done", "x=1; echo $x", "cd /tmp && ls",
        "PG=/opt/pg/bin; $PG/initdb -D $SP/pg", "$PG/pg_ctl -D \"$S/pg\" -l $S/pg/log start", "$EDITOR notes.md", "curl -s https://api.github.com/repos/o/r/pulls/12", "git subtree push --prefix=docs origin docs-branch",
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
    # Generated variants: every wrapper and chain around every core case.
    core_blocked = ["git push origin main", "git push origin HEAD:main", "git push -f origin +main",
                    "git reset --hard", "git clean -fd", "gh pr merge 3 --admin", "gh pr merge 3 --squash"]
    core_allowed = ["git push origin feature", "git status", "git clean -n", "gh pr view 3"]
    forms = ["{}", "env {}", "env A=1 {}", "command {}", "sudo -u x {}", "timeout 5 {}", "nohup {}", "A=1 {}",
             "bash -c '{}'", "sh -c \"{}\"", "eval '{}'", "true; {}", "true && {}", "false || {}", "echo x | {}",
             "({})", "echo $({})", "x=\"$({})\"", "bash <<'EOF'\n{}\nEOF", "bash <<< '{}'", "echo '{}' | bash"]
    generated = 0
    for form in forms:
        for core in core_blocked:
            generated += 1
            cmd = form.format(core)
            if decide({"tool_input": {"command": cmd}, "cwd": repo}) is None:
                print(f"FAIL (generated, should block): {cmd!r}")
                failures += 1
        for core in core_allowed:
            generated += 1
            cmd = form.format(core)
            reason = decide({"tool_input": {"command": cmd}, "cwd": repo})
            if reason is not None:
                print(f"FAIL (generated, should allow): {cmd!r}: {reason}")
                failures += 1
    failures += merge_self_test()
    total = len(blocked) + len(allowed) + generated + MERGE_CASES
    print(f"guard self-test: {total - failures}/{total} passed")
    return 1 if failures else 0


MERGE_CASES = 10


def merge_self_test():
    """merge_verdict on synthetic histories. Commits: A reviewed, B base commit,
    M merge of base, D a plain commit, X a commit outside the base."""
    same = ("diff",)
    base = {"B", "B2"}

    def verdict(head, reviews, parents, changes=None):
        changes = changes or {}
        return merge_verdict(head, reviews, parents, lambda s: s in base, lambda s: changes.get(s, same))

    ready = {"commit": "A", "submittedAt": "1", "state": "COMMENTED", "body": "**Verdict: Ready for maintainer.**"}
    cases = [
        ("review on head", verdict("A", [ready], {"A": ["P"]}), True),
        ("no review", verdict("A", [], {"A": ["P"]}), False),
        ("base merge after review", verdict("M", [ready], {"M": ["A", "B"], "A": ["P"]}), True),
        ("two base merges", verdict("M2", [ready], {"M2": ["M", "B2"], "M": ["A", "B"]}), True),
        ("plain commit after review", verdict("D", [ready], {"D": ["A"]}), False),
        ("merge of a non-base branch", verdict("M", [ready], {"M": ["A", "X"]}), False),
        ("conflict edits in the merge", verdict("M", [ready], {"M": ["A", "B"]}, {"M": ("other",)}), False),
        ("diff unavailable", verdict("M", [ready], {"M": ["A", "B"]}, {"M": None}), False),
        ("review on an older commit only", verdict("D", [ready], {"D": ["C"], "C": ["A"]}), False),
        ("base merge, then a plain commit", verdict("D", [ready], {"D": ["M"], "M": ["A", "B"]}), False),
    ]
    assert len(cases) == MERGE_CASES
    failures = 0
    for name, reason, ok in cases:
        if (reason is None) != ok:
            print(f"FAIL (merge rule, {name}): {'allowed' if reason is None else reason}")
            failures += 1
    return failures


if __name__ == "__main__":
    sys.exit(main())
