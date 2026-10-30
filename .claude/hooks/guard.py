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
- `gh api` writes that merge, move or delete refs or write contents, in any
  flag spelling and any endpoint spelling GitHub routes there (see api_path),
  and GraphQL merge or ref mutations, including bodies read
  from stdin or files (an unreadable GraphQL body is refused). One REST merge
  passes, for sessions without GraphQL, when every part is written out and a
  review covers the head it names (see rest_merge_request):
  `gh api -X PUT repos/<owner>/<repo>/pulls/<n>/merge -f merge_method=merge -f sha=<head sha>`

Commands are parsed with a quote-aware shell lexer. It follows `;`, `&&`, `|`
chains, subshells, `$(...)` and backticks, `sh -c`, `eval`, heredocs and
here-strings fed to a shell, shell scripts run by path, git and gh aliases, and
wrappers such as env, command, sudo, timeout, xargs and find -exec.

It stops agents' accidents, not deliberate evasion: the server-side ruleset on
main is the boundary. docs/agents/environment.md ("Guard scope") lists the
evasion classes it accepts.

Run the self-test with `.claude/hooks/guard.py --self-test`.
"""

import json
import os
import re
import shlex
import subprocess
import sys
import urllib.parse

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

    __slots__ = ("words", "stdin", "stdin_dynamic", "stdin_files")

    def __init__(self):
        self.words = []
        self.stdin = []
        self.stdin_dynamic = []  # per stdin text: True if the shell expands it ($(...), $VAR)
        self.stdin_files = []  # targets of < redirections


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
        elif redirect == "<":
            cmd.stdin_files.append(text)
        elif redirect == "<<<":
            cmd.stdin.append(text)
            cmd.stdin_dynamic.append(dynamic)
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
                if not cmd.words and not cmd.stdin and not pipeline:
                    cmd.words.append(Word("(subshell)", True))  # `( ... ) |`: output unknown
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
            owner.stdin_dynamic.append(False)
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
        owner.stdin_dynamic.append(not quoted and ("$" in body or "`" in body))
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
            stdin = stdin_of(pipeline, k, cwd, depth)
            cwd = check_command(cmd, cwd, depth, feeds_shell, shell_vars, stdin)
    return cwd


def stdin_of(pipeline, k, cwd, depth):
    """What pipeline[k] reads on stdin, as (texts, complete). complete is False
    when some of it can't be seen: a file that can't be read, or output from a
    program the guard can't predict."""
    cmd = pipeline[k]
    texts, complete = [], True
    for n, text in enumerate(cmd.stdin):
        dyn = cmd.stdin_dynamic[n] if n < len(cmd.stdin_dynamic) else False
        t, c = resolve_text(text, dyn, cwd, depth)
        texts, complete = texts + t, complete and c
    for path in cmd.stdin_files:
        t, c = redirect_input(path, cwd, depth)
        texts, complete = texts + t, complete and c
    if k > 0:
        t, c = output_of(pipeline, k - 1, cwd, depth)
        texts, complete = texts + t, complete and c
    return texts, complete


def output_of(pipeline, k, cwd, depth):
    """What pipeline[k] prints, for the commands whose output is predictable:
    echo, printf, and cat or tee passing on files and their own stdin."""
    argv, _, _, _ = unwrap(pipeline[k].words, depth)
    producer = program_name(argv[0].text) if argv else ""
    if producer in ("echo", "printf"):
        return [" ".join(w.text for w in argv[1:])], True
    if producer not in ("cat", "tee"):
        return [], False
    operands = [w for w in argv[1:] if w.text == "-" or not w.text.startswith("-")]
    texts, complete = [], True
    if producer == "tee" or not operands or any(w.text == "-" for w in operands):
        texts, complete = stdin_of(pipeline, k, cwd, depth)
    if producer == "cat":
        for w in operands:
            if w.text == "-":
                continue
            body = None if w.dynamic else read_script_file(w.text, cwd)
            if body is None:
                complete = False
            else:
                texts.append(body)
    return texts, complete


def redirect_input(target, cwd, depth):
    """The contents behind `< target` or `--input target`: a file, or a
    process substitution <(...)."""
    if target.startswith("<(") and target.endswith(")"):
        return script_output(target[2:-1], cwd, depth + 1)
    body = read_script_file(target, cwd)
    return ([body], True) if body is not None else ([], False)


def resolve_text(text, dynamic, cwd, depth):
    """A word or heredoc body as the shell will expand it, as (texts, complete).
    Command substitutions with predictable output are added as that output;
    any other expansion ($VAR, ${...}) makes it incomplete."""
    if not dynamic:
        return [text], True
    try:
        subs = parse_substitutions(text)
    except ParseError:
        return [text], False
    texts, complete, rest = [text], True, text
    for inner in subs:
        t, c = script_output(inner, cwd, depth + 1)
        texts, complete = texts + t, complete and c
        rest = rest.replace("$(" + inner + ")", "").replace("`" + inner + "`", "")
    if "$" in rest or "`" in rest:
        complete = False
    return texts, complete


def script_output(text, cwd, depth):
    """What a script prints, when the last command of each pipeline is predictable."""
    if depth > MAX_DEPTH:
        return [], False
    try:
        parsed = parse(text)
    except ParseError:
        return [], False
    texts, complete = [], True
    for pipeline in parsed.pipelines:
        t, c = output_of(pipeline, len(pipeline) - 1, cwd, depth)
        texts, complete = texts + t, complete and c
    return texts, complete


def read_script_file(path, cwd):
    full = os.path.realpath(os.path.join(cwd, os.path.expanduser(path)))
    if full.startswith(("/dev/", "/proc/")):
        return None  # devices and the hook's own stdin are never a script
    try:
        if os.path.isfile(full) and os.path.getsize(full) <= MAX_SCRIPT_BYTES:
            with open(full, "r", errors="replace") as f:
                return f.read()
    except OSError:
        pass
    return None


def check_command(cmd, cwd, depth, feeds_shell, shell_vars, stdin=([], True)):
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
        # typed: `gh` is the first word of a command on the line itself, as prefix rules (Codex's) see it
        typed = depth == 0 and len(argv) == len(cmd.words) and prog.text == "gh"
        check_gh(args, cwd, opaque, depth, stdin, typed)
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
            check_gh(args, cwd, opaque, depth, stdin)
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


def long_option(arg, full, minimum):
    """Git accepts any unambiguous prefix of a long option. Returns True for
    --<prefix of full>, False for --no-<prefix of full>, None otherwise."""
    body = arg[2:].split("=", 1)[0]
    if body.startswith("no-") and len(body) - 3 >= minimum and full.startswith(body[3:]):
        return False
    if len(body) >= minimum and full.startswith(body):
        return True
    return None


def check_clean(rest, cwd, git, opaque):
    force = dry = False  # each flag in order; the last dry-run flag wins
    i = 0
    while i < len(rest):
        a = rest[i].text
        if a == "--":
            break
        if a.startswith("--"):
            flag_dry = long_option(a, "dry-run", 1)
            flag_force = long_option(a, "force", 3)
            if flag_dry is not None:
                dry = flag_dry
            elif flag_force is not None:
                force = flag_force
            elif long_option(a, "exclude", 2) and "=" not in a:
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
    dry, everything = False, None  # the last dry-run flag wins
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
            flag_dry = long_option(a, "dry-run", 2)
            if flag_dry is not None:
                dry = flag_dry
            elif name in ("--all", "--mirror", "--branches"):
                everything = name
            elif name == "--delete":
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
                    dry = True
                if ch == "d":
                    deleting = True
                if ch == "o":
                    if k == len(letters) - 1:
                        i += 1
                    break
        i += 1
    if dry:
        return
    if everything:
        raise Blocked(f"`git push {everything}` would update main. Push only your branch.")
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
        rest = re.sub(r"^(?:[A-Za-z][A-Za-z0-9+.-]*://)?[^/?#]*", "", url)  # after the scheme, host and any port
        path = api_path(rest)
        legacy = legacy_path(re.sub(r"^.*api\.github\.com", "", url))  # as main read it
        if (path == "graphql" or legacy.endswith("/graphql")) and GRAPHQL_WRITES.search(joined):
            raise Blocked(f"GraphQL mutations that merge PRs or move refs are blocked. {MERGE_WAYS}")
        if write and (hides_delimiter(rest) or API_WRITE_PATHS.search(path) or API_WRITE_PATHS.search(legacy)):
            raise Blocked(f"GitHub API writes that merge PRs or move, delete or write branches are blocked. {MERGE_WAYS}")


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
API_WRITE_PATHS = re.compile(r"(^|/)(pulls/[^/]+/(merge|ccr/auto_merge|update-branch)|merges|git/refs(/.*)?|"
                             r"contents/.*|branches/[^/]+/rename)$")
MERGE_PATH = re.compile(r"(^|/)pulls/[^/]+/merge$")
AUTO_MERGE_PATH = re.compile(r"(^|/)pulls/[^/]+/ccr/auto_merge$")  # the cloud session's route for auto-merge


def raw_path(endpoint):
    """An endpoint or URL's path as written: no scheme, host or port, cut at
    its first `?` or `#` (the query and fragment), nothing decoded."""
    return re.split(r"[?#]", re.sub(r"^[A-Za-z][A-Za-z0-9+.-]*://[^/?#]*", "", endpoint), maxsplit=1)[0]


def decoded(text):
    """text percent-decoded until it stops changing: %256D gives %6D, then m."""
    for _ in range(8):
        once = urllib.parse.unquote(text)
        if once == text:
            break
        text = once
    return text


def api_path(endpoint):
    """The path GitHub routes for an API endpoint or URL, for matching. The
    raw path (raw_path) has `\\` read as `/`; each segment is cut at `;` (its
    parameters); empty and `.` segments are dropped and `..` applied; only
    then is each segment percent-decoded until stable and trimmed, and the
    whole is lower-cased. So `merge#`, `merge/.`, `./merge`, `//merge`,
    `%6Derge`, `merge;x` and `MERGE` read as `merge`, while an encoded
    delimiter stays inside its segment: `pulls/24%3F/merge` reads as
    `pulls/24?/merge`, still a merge (and hides_delimiter refuses it)."""
    segments = []
    for segment in raw_path(endpoint).replace("\\", "/").split("/"):
        segment = segment.split(";", 1)[0]
        if segment == "..":
            segments = segments[:-1]
        elif segment not in ("", "."):
            segment = decoded(segment).strip()
            if segment:
                segments.append(segment)
    return "/".join(segments).lower()


def hides_delimiter(endpoint):
    """True when percent-encoding, once or more, hides a `?`, `#`, `/`, `\\`,
    `.` or `;` in the path. GitHub may route that either way, so a write to
    it is refused rather than guessed."""
    path = raw_path(endpoint)
    plain = decoded(path)
    return any(plain.count(c) > path.count(c) for c in "?#/\\.;")


def legacy_path(endpoint):
    """The path as main's guard read it (cut at `?`, trailing `/` stripped).
    Writes are checked against it as well, so protection only grows."""
    return endpoint.split("?", 1)[0].rstrip("/")
REST_MERGE = "gh api -X PUT repos/<owner>/<repo>/pulls/<n>/merge -f merge_method=merge -f sha=<head sha>"
REST_MERGE_ENDPOINT = re.compile(r"/?repos/([A-Za-z0-9][A-Za-z0-9-]*/(?!\.+/)[A-Za-z0-9._-]+)/pulls/([1-9][0-9]*)/merge")
MERGE_WAYS = (f"Merge with `gh pr merge <n> --merge`, or where GraphQL is unavailable `{REST_MERGE}` with the "
              "owner, repo, PR number and full head SHA written out; push branches with `git push`. Those are checked.")


def check_gh(args, cwd, opaque, depth, stdin=([], True), typed=False):
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
                check_gh_api(args[i + 1 :], cwd, stdin, opaque, texts[:i], typed)
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


GH_API_VALUE_FLAGS = {
    "-X": "method", "--method": "method",
    "-f": "field", "--raw-field": "field", "-F": "field", "--field": "field",
    "--input": "input",
    "-H": "other", "--header": "other", "-q": "other", "--jq": "other", "-t": "other", "--template": "other",
    "--cache": "other", "--hostname": "other", "-p": "other", "--preview": "other",
}


def split_api_flag(arg):
    """(flag, attached value or None) for a gh api option that takes a value,
    in any spelling: -fkey=v, -f=key=v, --raw-field=key=v, -XPUT, and POSIX
    short clusters such as -iXPUT, where boolean flags come first."""
    if arg.startswith("--"):
        name, eq, value = arg.partition("=")
        return (name, value if eq else None) if name in GH_API_VALUE_FLAGS else (None, None)
    if arg.startswith("-") and len(arg) >= 2:
        for k in range(1, len(arg)):
            flag = "-" + arg[k]
            if flag in GH_API_VALUE_FLAGS:
                rest = arg[k + 1 :]
                if not rest:
                    return flag, None
                return flag, rest[1:] if rest.startswith("=") else rest
    return None, None


def json_strings(text):
    """Every string inside a JSON document, decoded, so escapes can't hide a word."""
    try:
        doc = json.loads(text)
    except ValueError:
        return ""
    found = []

    def walk(x):
        if isinstance(x, str):
            found.append(x)
        elif isinstance(x, dict):
            for k, v in x.items():
                found.append(str(k))
                walk(v)
        elif isinstance(x, list):
            for v in x:
                walk(v)

    walk(doc)
    return " ".join(found)


STDIN_PATHS = {"-", "/dev/stdin", "/dev/fd/0", "/proc/self/fd/0"}


def check_gh_api(args, cwd, stdin=([], True), opaque=False, leading=(), typed=False):
    """args follow `api`; leading are the words between `gh` and `api`; typed
    says `gh` starts a command on the line itself (see check_command)."""
    method, endpoint, writes, unreadable = None, None, False, False
    bodies = []  # (text, dynamic) pieces of the request body
    stdin_texts, stdin_complete = stdin
    i = 0
    while i < len(args):
        a = args[i].text
        flag, value = split_api_flag(a)
        if flag is None:
            if not a.startswith("-") and endpoint is None:
                endpoint = args[i]
            i += 1
            continue
        dynamic = args[i].dynamic
        if value is None:
            value = args[i + 1].text if i + 1 < len(args) else ""
            dynamic = args[i + 1].dynamic if i + 1 < len(args) else False
            i += 1
        i += 1
        kind = GH_API_VALUE_FLAGS[flag]
        if kind == "method":
            method = value.upper()
            continue
        if kind == "other":
            continue
        writes = True  # any field or input body makes gh send a POST by default
        if kind == "field":
            bodies.append((value, dynamic))
            source = value.split("=", 1)[1] if "=" in value else ""
            if flag in ("-f", "--raw-field") or not source.startswith("@"):
                continue
            source = source[1:]
        else:
            source = value
        if source in STDIN_PATHS:
            bodies.extend((t, False) for t in stdin_texts)
            unreadable = unreadable or not stdin_complete
        elif dynamic and not source.startswith("<("):
            unreadable = True
        else:
            t, c = redirect_input(source, cwd, 0)
            bodies.extend((x, False) for x in t)
            unreadable = unreadable or not c
    if endpoint is None:
        return
    path = api_path(endpoint.text)  # as GitHub routes it: `/graphql` and `merge#` are spellings too
    legacy = legacy_path(endpoint.text)  # as main read it
    if path == "graphql" or legacy == "graphql":
        texts = []
        for body, dynamic in bodies:
            t, c = resolve_text(body, dynamic, cwd, 0)
            texts, unreadable = texts + t, unreadable or not c
        text = " ".join(texts + [json_strings(b) for b in texts])
        if GRAPHQL_WRITES.search(text):
            raise Blocked(f"GraphQL mutations that merge PRs or move refs are blocked. {MERGE_WAYS}")
        if unreadable:
            raise Blocked("can't read this GraphQL request's body, so it can't be checked for merges or ref writes. "
                          "Pass the query inline with -f query=..., or from a file in the repo.")
        return
    write = (method or ("POST" if writes else "GET")) not in ("GET", "HEAD")
    hidden = hides_delimiter(endpoint.text)
    if write and (endpoint.dynamic or hidden or API_WRITE_PATHS.search(path) or API_WRITE_PATHS.search(legacy)):
        if hidden:
            raise Blocked("this write's endpoint hides a `?`, `#`, `/`, `\\`, `.` or `;` behind percent-encoding, so "
                          f"GitHub may route it either way. Write the path out plainly. {MERGE_WAYS}")
        if AUTO_MERGE_PATH.search(path) or AUTO_MERGE_PATH.search(legacy):
            raise Blocked("auto-merge (`pulls/<n>/ccr/auto_merge`) merges later, after the review check, like "
                          f"`gh pr merge --auto`. Merge once a review covers the head, with `{REST_MERGE}`.")
        if not (MERGE_PATH.search(path) or MERGE_PATH.search(legacy)):
            raise Blocked(f"`gh api` writes that merge PRs or move, delete or write branches are blocked. {MERGE_WAYS}")
        slug, number, sha = rest_merge_request(args, leading, opaque, typed)
        reason = rest_review_check(slug, number, sha, cwd)
        if reason:
            raise Blocked(f"PR can't merge yet: {reason}")


def rest_merge_request(args, leading, opaque, typed):
    """The one `gh api` merge the guard lets through, for sessions without
    GraphQL: REST_MERGE and nothing else. It is a command of its own that
    starts `gh api` and then the method (`-X PUT`, `-XPUT`, `-X=PUT`, `--method
    PUT`, `--method=PUT`), so a prefix rule such as Codex's, which sees the
    command as typed and unwraps nothing, can ask a person first, as it does
    for `gh pr merge`. The endpoint and -f/-F/--raw-field/--field follow in
    any order and spelling. Every word must be written out, and each field
    given once (gh applies -F after -f, whatever their order). Returns
    (owner/repo, number, sha), or raises Blocked naming what differs."""

    def refuse(why):
        raise Blocked(f"this REST merge isn't the one form the guard checks: {why}. Write `{REST_MERGE}`, "
                      "with the owner, repo, PR number and full head SHA written out.")

    if opaque:
        refuse("more arguments arrive at run time (xargs, find -exec)")
    if leading:
        refuse(f"`gh {' '.join(leading)}` before `api` isn't part of it")
    if not typed:
        refuse("it must be a command of its own that starts `gh api`, not one under a wrapper (`timeout`, `env`), "
               "after `VAR=`, through an alias, or inside `bash -c`, `eval` or `$(...)`")
    methods, fields, positionals = [], [], []
    i = 0
    while i < len(args):
        w, start = args[i], i
        if w.dynamic:
            refuse(f"'{w.text}' is computed at run time")
        a = w.text
        if a.startswith("--"):
            name, eq, value = a.partition("=")
            value = value if eq else None
        elif a.startswith("-") and len(a) > 1:
            name, value = a[:2], a[2:]
            value = value[1:] if value.startswith("=") else (value or None)  # -XPUT, -X=PUT; -X alone takes the next word
        else:
            positionals.append(a)
            i += 1
            continue
        into = {"-X": methods, "--method": methods, "-f": fields, "--raw-field": fields,
                "-F": fields, "--field": fields}.get(name)
        if into is None:
            refuse(f"`{a}` isn't part of it")
        if into is methods and start != 0:
            refuse("the method goes right after `api`: `gh api -X PUT repos/...`")
        if value is None:
            if i + 1 >= len(args):
                refuse(f"`{a}` has no value")
            if args[i + 1].dynamic:
                refuse(f"'{args[i + 1].text}' is computed at run time")
            value = args[i + 1].text
            i += 1
        into.append(value)
        i += 1
    if len(methods) != 1 or methods[0] != "PUT":
        refuse("it takes exactly one method, PUT, right after `api`")
    m = REST_MERGE_ENDPOINT.fullmatch(positionals[0]) if len(positionals) == 1 else None
    if not m:
        refuse(f"the endpoint must be repos/<owner>/<repo>/pulls/<n>/merge, not '{' '.join(positionals)}'")
    given = {}
    for field in fields:
        key, eq, value = field.partition("=")
        if not eq or key not in ("merge_method", "sha"):
            refuse(f"field '{field}' isn't part of it")
        if key in given:
            refuse(f"`{key}` is given twice")
        given[key] = value
    if given.get("merge_method") != "merge":
        refuse("it needs `merge_method=merge`: merge commits keep stacked PRs' history")
    sha = given.get("sha", "")
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        refuse("it needs `sha=<the PR's head>`, all 40 lowercase hex digits, so GitHub refuses the merge "
               "if the head moves after this check")
    return m.group(1), m.group(2), sha


def rest_review_check(slug, number, sha, cwd):
    """The review rule for the REST merge: sha must be the PR's current head,
    and a review must cover it, all read over REST."""
    pr, reason = pr_over_rest(slug, number, cwd)
    if reason:
        return reason
    if pr["head"] != sha:
        return (f"sha {sha[:7]} isn't the PR's head, which is {pr['head'] or 'unknown'}. "
                "Merge the head a review covers.")
    return pr_verdict(pr, cwd)


# ---------------------------------------------------------------- merge review rule

def merge_verdict(head, reviews, parents, base_ancestor, merge_clean):
    """Decide whether a PR may merge. Returns None, or the reason it can't.

    - head: the PR's head commit
    - reviews: [{"commit": sha, "submittedAt": iso, ...}]
    - parents: {sha: [parent shas]} for the PR's commits
    - base_ancestor(sha): True if sha is already in the base branch
    - merge_clean(merge, first, second): True if the merge's tree is exactly
      git's own clean merge of its two parents

    A review covers the head when every commit after the reviewed one, along
    the first-parent chain, is a two-parent merge that brings in commits
    already in the base branch and is byte-for-byte git's clean automatic
    merge. Such a merge adds nothing of its own, so the reviewed edits are
    still exactly where they were reviewed. What the review says is for the
    maintainer to weigh; the guard only checks that one covers the code.
    """
    reviewed = {r["commit"] for r in reviews if r.get("commit")}
    if not reviewed:
        return "it has no review. The cross-review posts one (see the katwork-review skill), then the maintainer merges."
    sha = head
    while sha not in reviewed:
        ps = parents.get(sha)
        if not ps:
            return (f"no review covers its head {head[:7]}: the commits after the last review "
                    "aren't all merges of the base branch. Ask for a review of the delta.")
        if len(ps) == 1:
            return (f"commit {sha[:7]} came after the last review and isn't a merge of the base branch. "
                    "Ask for a review of the delta.")
        if len(ps) > 2:
            return f"merge {sha[:7]} after the last review has {len(ps)} parents. Ask for a review of the merge."
        if not base_ancestor(ps[1]):
            return f"merge {sha[:7]} after the last review brings in {ps[1][:7]}, which isn't in the base branch."
        if not merge_clean(sha, ps[0], ps[1]):
            return (f"merge {sha[:7]} after the last review isn't git's own clean merge of its parents: it resolves "
                    "a conflict or changes code (or its commits couldn't be fetched here). Ask for a review of the merge.")
        sha = ps[0]
    return None


def merge_is_clean(merge, first, second, cwd):
    """True when merge's tree is exactly what `git merge-tree` produces for a
    conflict-free merge of first and second."""
    out = run(["git", "merge-tree", "--write-tree", first, second], cwd)
    actual = run(["git", "rev-parse", "--verify", "-q", merge + "^{tree}"], cwd)
    return bool(out) and bool(actual) and out.splitlines()[0].strip() == actual


def have_commits(shas, slug, cwd):
    """Make sure the commits are in the local object store, fetching them by
    id from GitHub if needed."""
    missing = [s for s in shas if run(["git", "cat-file", "-e", s + "^{commit}"], cwd) is None]
    if missing:
        run(["git", "fetch", "--quiet", "--no-tags", "--no-write-fetch-head",
             f"https://github.com/{slug}.git", *missing], cwd, timeout=120)
    return all(run(["git", "cat-file", "-e", s + "^{commit}"], cwd) is not None for s in shas)


def review_check(target, repo, cwd):
    """The review rule for `gh pr merge`, on the PR `gh pr view` reads."""
    view = ["gh", "pr", "view"] + ([target] if target else []) + repo
    out = run(view + ["--json", "number,url,headRefOid,baseRefName,reviews"], cwd)
    if out is None:  # without GraphQL, `gh pr merge` can't merge either
        return ("couldn't read the PR with `gh pr view`, so the merge can't be checked. Where GraphQL is "
                f"unavailable, merge with `{REST_MERGE}`.")
    pr, reason = pr_from_view(out)
    return reason or pr_verdict(pr, cwd)


def pr_from_view(out):
    """(pr, None) from `gh pr view --json` output, or (None, reason). pr holds
    slug, number, head, base and reviews ({"commit", "submittedAt"}). A
    pending review hasn't been posted, so it never counts."""
    pr = json.loads(out)
    m = re.search(r"github\.com/([^/]+/[^/]+)/pull/(\d+)", pr.get("url", ""))
    if not m:
        return None, "couldn't tell which repository the PR is in."
    reviews = [{"commit": (r.get("commit") or {}).get("oid"), "submittedAt": r.get("submittedAt")}
               for r in pr.get("reviews", []) if r.get("state") != "PENDING"]
    return {"slug": m.group(1), "number": m.group(2), "head": pr.get("headRefOid", ""),
            "base": pr.get("baseRefName", ""), "reviews": reviews}, None


def pr_over_rest(slug, number, cwd):
    """pr_from_view's result for the REST merge, read over REST: the PR, then
    its reviews by each one's commit_id."""
    endpoint = f"repos/{slug}/pulls/{number}"
    out = run(["gh", "api", endpoint, "--jq", "{number, url: .html_url, head: .head.sha, base: .base.ref}"], cwd)
    try:
        pr = json.loads(out) if out is not None else None
    except ValueError:
        pr = None
    if not isinstance(pr, dict):
        return None, f"couldn't read the PR with `gh api {endpoint}`, so the merge can't be checked."
    m = re.fullmatch(r"https://github\.com/([A-Za-z0-9-]+/[A-Za-z0-9._-]+)/pull/([0-9]+)", str(pr.get("url")))
    if not m or m.group(2) != number:
        return None, "couldn't tell which repository the PR is in."
    slug = m.group(1)
    lines = run(["gh", "api", "--paginate", f"repos/{slug}/pulls/{number}/reviews",
                 "--jq", ".[] | {commit: .commit_id, state, submittedAt: .submitted_at}"], cwd)
    try:
        found = [json.loads(line) for line in lines.splitlines() if line.strip()] if lines is not None else None
        reviews = [{"commit": r.get("commit"), "submittedAt": r.get("submittedAt")}
                   for r in found if r.get("state") != "PENDING"] if found is not None else None
    except (ValueError, AttributeError):
        reviews = None
    if reviews is None:
        return None, "couldn't list the PR's reviews over REST, so the merge can't be checked."
    return {"slug": slug, "number": number, "head": pr.get("head") or "", "base": pr.get("base") or "",
            "reviews": reviews}, None


def pr_verdict(pr, cwd):
    """merge_verdict for a PR read by pr_from_view or pr_over_rest."""
    slug, number, base, reviews = pr["slug"], pr["number"], pr["base"], pr["reviews"]
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

    def merge_clean(merge, first, second):
        return have_commits([merge, first, second], slug, cwd) and merge_is_clean(merge, first, second, cwd)

    return merge_verdict(pr["head"], reviews, parents, base_ancestor, merge_clean)


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
    with open(os.path.join(repo, "mutation.json"), "w") as f:
        f.write('{"query": "mutation { mergePullRequest(input: {}) { clientMutationId } }"}')
    with open(os.path.join(repo, "query.json"), "w") as f:
        f.write('{"query": "query { viewer { login } }"}')

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
        # attached field flags still mean a write (re-review finding 7)
        "gh api repos/o/r/merges -fbase=main -fhead=feature", "gh api repos/o/r/merges --raw-field=base=main --raw-field=head=x",
        "gh api repos/o/r/merges -Fbase=main -Fhead=x", "gh api repos/o/r/git/refs --field=ref=refs/heads/x --field=sha=y",
        "gh api repos/o/r/merges -f=base=main", "gh api repos/o/r/merges --input=body.json",
        # GraphQL bodies from stdin, files and fields (re-review finding 8)
        "gh api graphql --input - <<'EOF'\n{\"query\": \"mutation { mergePullRequest(input: {pullRequestId: \\\"x\\\"}) { clientMutationId } }\"}\nEOF",
        "gh api graphql --input - <<'EOF'\n{\"query\": \"mutation { enablePullRequestAutoMerge(input: {}) { clientMutationId } }\"}\nEOF",
        "gh api graphql --input=- <<< '{\"query\": \"mutation { updateRefs(input: {}) { clientMutationId } }\"}'",
        "gh api graphql -F query=@- <<'EOF'\nmutation { deleteRef(input: {refId: \"x\"}) { clientMutationId } }\nEOF",
        "gh api graphql --input - <<'EOF'\n{\"query\": \"mutation { \\u006dergePullRequest(input: {}) { clientMutationId } }\"}\nEOF",
        "gh api graphql --input mutation.json", "gh api graphql --input - < mutation.json", "cat mutation.json | gh api graphql --input -",
        "echo '{\"query\": \"mutation { updateRef(input: {}) { clientMutationId } }\"}' | gh api graphql --input -",
        "gh api graphql --input missing.json", "cat missing.json | gh api graphql --input -", "some-tool | gh api graphql --input -",
        "gh api graphql -F query=@missing.graphql",
        # short-flag clusters and cat redirects (final review)
        "gh api repos/o/r/pulls/28/merge -iXPUT", "gh api repos/o/r/merges -ifbase=main -ifhead=feature",
        "gh api repos/o/r/git/refs/heads/main -iXDELETE", "gh api repos/o/r/merges -iFbase=main",
        "gh api -iX PUT repos/o/r/pulls/1/merge", "gh api repos/o/r/pulls/1/merge -iX=PUT",
        "gh api repos/o/r/pulls/1/merge -iXput", "gh api repos/o/r/merges -if base=main",
        "cat < mutation.json | gh api graphql --input -", "cat mutation.json | cat | gh api graphql --input -",
        "gh api graphql --input - < <(cat mutation.json)", "cat < mutation.json | tee /dev/null | gh api graphql --input -",
        "cat <<'EOF' | cat | gh api graphql --input -\n{\"query\": \"mutation { deleteRef(input: {}) { clientMutationId } }\"}\nEOF",
        "gh api graphql --input - < <(some-tool)", "gh api graphql --input - < <(cat missing.json)",
        "cat - < mutation.json | gh api graphql --input -", "gh api graphql -F query=@- < <(echo 'mutation { updateRef(input: {}) { x } }')",
        "gh api graphql --input /dev/stdin < mutation.json", "gh api graphql --input <(cat mutation.json)",
        "(cat mutation.json) | gh api graphql --input -", "{ cat mutation.json; } | gh api graphql --input -",
        "gh api graphql --input - <<< \"$(cat mutation.json)\"", "gh api graphql -f query=\"$(cat mutation.json)\"",
        "gh api graphql -f query=\"$Q\"", "gh api graphql --input - <<EOF\n$(cat mutation.json)\nEOF",
        "gh api graphql --input - < \"$F\"", "gh api -iXPOST repos/o/r/merges", "gh api repos/o/r/merges -iHAccept:x -fbase=main",
        # a later negation cancels a dry run (re-review finding 9)
        "git push --dry-run --no-dry-run origin main", "git push -n --no-dry-run origin main",
        "git push --dry-run --no-dry origin main", "git push --dry-run --all --no-dry-run origin",
        "git clean -f -n --no-dry-run -- x", "git clean -fn --no-dry-run", "git clean --dry-run -f --no-d",
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
        "gh api graphql --input - <<'EOF'\n{\"query\": \"query { viewer { login } }\"}\nEOF",
        "gh api graphql -f query='query { repository(owner: \"o\", name: \"r\") { id } }'", "gh api graphql --input query.json",
        "cat < query.json | gh api graphql --input -", "cat query.json | cat | gh api graphql --input -",
        "gh api graphql --input - < <(cat query.json)", "gh api -i repos/o/r/pulls/1", "gh api repos/o/r/pulls/1 -iq .title",
        "gh api -iXGET repos/o/r/pulls/1/merge",
        "gh api graphql --input /dev/stdin < query.json", "gh api graphql --input <(cat query.json)",
        "gh api graphql --input - <<< \"$(cat query.json)\"", "gh api graphql -f query=\"$(cat query.json)\"",
        "gh api graphql -f query='query($n: Int!) { viewer { login } }' -F n=1", "gh api repos/o/r/merges -iHx",
        "gh api repos/o/r/pulls -ifstate=open --method GET",
        "cat query.json | gh api graphql --input -", "gh api repos/o/r/pulls -q '.[].number'", "gh api -H 'Accept: x' repos/o/r",
        "gh pr create --body-file - <<'EOF'\nWe never call mergePullRequest or updateRef here.\nEOF",
        "gh pr review 12 --comment --body-file - <<'EOF'\nThe guard blocks deleteRef.\nEOF",
        "git push --no-dry-run --dry-run origin main", "git push --dry origin main", "git clean -f --no-dry-run -n", "git clean -f --d",
        "for f in a b; do echo $f; done", "x=1; echo $x", "cd /tmp && ls",
        "PG=/opt/pg/bin; $PG/initdb -D $SP/pg", "$PG/pg_ctl -D \"$S/pg\" -l $S/pg/log start", "$EDITOR notes.md", "curl -s https://api.github.com/repos/o/r/pulls/12", "git subtree push --prefix=docs origin docs-branch",
        # the REST forms the docs give for sessions without GraphQL
        "gh api repos/{owner}/{repo}/pulls -f title='SP2a Task 4: x' -f head=sp2a/task-4-x -f base=main -F body=@body.md",
        "gh api repos/{owner}/{repo}/pulls/12 --jq .head.sha", "gh api repos/{owner}/{repo}/pulls/12/reviews --paginate",
        "gh api repos/{owner}/{repo}/pulls/12 -H 'Accept: application/vnd.github.diff'",
        "gh api 'repos/{owner}/{repo}/commits/abc/check-runs?per_page=100' --jq '.check_runs[] | .name'",
        "gh api repos/{owner}/{repo}/pulls/12/reviews -f commit_id=abc -f event=COMMENT -F body=@review.md",
        "gh api -X PATCH repos/{owner}/{repo}/pulls/12 -f base=main", "gh api repos/{owner}/{repo}/issues/12/comments -F body=@c.md",
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
    failures += rest_self_test()
    total = len(blocked) + len(allowed) + generated + MERGE_CASES + REST_CASES
    print(f"guard self-test: {total - failures}/{total} passed")
    return 1 if failures else 0


MERGE_CASES = 13


def merge_self_test():
    """merge_verdict on synthetic histories, then on real commits. Synthetic:
    A reviewed, B a base commit, M a merge of the base, D a plain commit, X a
    commit outside the base, and every merge is clean unless named in dirty."""
    base = {"B", "B2"}

    def verdict(head, reviews, parents, dirty=()):
        return merge_verdict(head, reviews, parents, lambda s: s in base, lambda m, a, b: m not in dirty)

    ready = {"commit": "A", "submittedAt": "1", "state": "COMMENTED", "body": "**Verdict: Ready for maintainer.**"}
    cases = [
        ("review on head", verdict("A", [ready], {"A": ["P"]}), True),
        ("no review", verdict("A", [], {"A": ["P"]}), False),
        ("base merge after review", verdict("M", [ready], {"M": ["A", "B"], "A": ["P"]}), True),
        ("two base merges", verdict("M2", [ready], {"M2": ["M", "B2"], "M": ["A", "B"]}), True),
        ("plain commit after review", verdict("D", [ready], {"D": ["A"]}), False),
        ("merge of a non-base branch", verdict("M", [ready], {"M": ["A", "X"]}), False),
        ("merge that isn't git's clean merge", verdict("M", [ready], {"M": ["A", "B"]}, {"M"}), False),
        ("octopus merge", verdict("M", [ready], {"M": ["A", "B", "B2"]}), False),
        ("review on an older commit only", verdict("D", [ready], {"D": ["C"], "C": ["A"]}), False),
        ("base merge, then a plain commit", verdict("D", [ready], {"D": ["M"], "M": ["A", "B"]}), False),
    ]
    cases += real_merge_cases()
    assert len(cases) == MERGE_CASES
    failures = 0
    for name, reason, ok in cases:
        if (reason is None) != ok:
            print(f"FAIL (merge rule, {name}): {'allowed' if reason is None else reason}")
            failures += 1
    return failures


def real_merge_cases():
    """Real commits. The PR's reviewed commit A turns preview_enabled() on;
    base commit B edits the README. A clean merge of B keeps the review. A
    merge with the same parents that instead turns admin_enabled() on moves
    the reviewed edit (same +/- lines, different place) and must not. A merge
    that resolves a conflict with base commit C needs a review too."""
    import tempfile

    repo = tempfile.mkdtemp()

    def git(*a):
        out = subprocess.run(["git", *a], cwd=repo, check=True, capture_output=True, text=True)
        return out.stdout.strip()

    def write(name, text):
        with open(os.path.join(repo, name), "w") as f:
            f.write(text)

    app = "def preview_enabled():\n    return {p}\n\n\ndef helper():\n    return 1\n\n\ndef admin_enabled():\n    return {a}\n"
    git("init", "-q", "-b", "main")
    git("config", "user.email", "1+bot" + NOREPLY)
    git("config", "user.name", "bot")
    write("app.py", app.format(p="False", a="False"))
    write("README.md", "base\n")
    git("add", ".")
    git("commit", "-q", "-m", "P")
    git("switch", "-q", "-c", "pr")
    write("app.py", app.format(p="True", a="False"))
    git("commit", "-q", "-am", "A: turn preview on")
    a = git("rev-parse", "HEAD")
    git("switch", "-q", "main")
    write("README.md", "base\nmore\n")
    git("commit", "-q", "-am", "B: readme")
    b = git("rev-parse", "HEAD")
    git("switch", "-q", "pr")
    git("merge", "-q", "--no-edit", "main")
    good = git("rev-parse", "HEAD")
    write("app.py", app.format(p="False", a="True"))
    git("add", "app.py")
    moved = git("commit-tree", git("write-tree"), "-p", a, "-p", b, "-m", "Merge main")
    git("switch", "-q", "--discard-changes", "main")
    write("app.py", app.format(p="None", a="False"))
    git("commit", "-q", "-am", "C: preview returns None")
    c = git("rev-parse", "HEAD")
    write("app.py", app.format(p="True", a="False"))
    git("add", "app.py")
    resolved = git("commit-tree", git("write-tree"), "-p", a, "-p", c, "-m", "Merge main, keep ours")

    def base_ancestor(sha):
        return subprocess.run(["git", "merge-base", "--is-ancestor", sha, "main"], cwd=repo).returncode == 0

    def verdict(head):
        review = {"commit": a, "submittedAt": "1"}
        parents = {good: [a, b], moved: [a, b], resolved: [a, c]}
        return merge_verdict(head, [review], parents, base_ancestor,
                             lambda m, x, y: merge_is_clean(m, x, y, repo))

    return [
        ("real clean merge of the base", verdict(good), True),
        ("real merge that moves the reviewed edit", verdict(moved), False),
        ("real merge that resolves a conflict", verdict(resolved), False),
    ]


REST_CASES = 123


def rest_self_test():
    """The REST merge, and `gh pr merge` with and without GraphQL, end to end
    through decide() with every `gh` call answered by a fake. A scratch repo
    holds real commits: P on main, A (the reviewed
    commit) on the PR branch, B on main, M (git's clean merge of B into A) and
    D (a plain commit on A). The fake serves repo o/r: PR 21 (head A, reviewed
    on A), 22 (head M, reviewed on A), 23 (head D, reviewed on A), 24 (head A,
    no review) and 25 (head A, only a pending review on A). Without GraphQL,
    as in a cloud session, `gh pr view` fails."""
    global run
    import tempfile

    repo = tempfile.mkdtemp()

    def git(*a):
        out = subprocess.run(["git", *a], cwd=repo, check=True, capture_output=True, text=True)
        return out.stdout.strip()

    def commit(name, text, message):
        with open(os.path.join(repo, name), "w") as f:
            f.write(text)
        git("add", name)
        git("commit", "-q", "-m", message)
        return git("rev-parse", "HEAD")

    git("init", "-q", "-b", "main")
    git("config", "user.email", "1+bot" + NOREPLY)
    git("config", "user.name", "bot")
    p = commit("app.py", "x = 0\n", "P")
    git("switch", "-q", "-c", "pr")
    a = commit("app.py", "x = 1\n", "A")
    git("switch", "-q", "main")
    b = commit("README.md", "base\n", "B")
    git("switch", "-q", "pr")
    git("merge", "-q", "--no-edit", "main")
    m = git("rev-parse", "HEAD")
    git("switch", "-q", "-c", "later", a)
    d = commit("app.py", "x = 2\n", "D")
    prs = {21: (a, [(a, "COMMENTED")], {a: [p]}), 22: (m, [(a, "COMMENTED")], {a: [p], m: [a, b]}),
           23: (d, [(a, "COMMENTED")], {a: [p], d: [a]}), 24: (a, [], {a: [p]}), 25: (a, [(a, "PENDING")], {a: [p]})}

    def answers(graphql):
        """What `gh` prints, by the words after `gh`. Any other call fails, as
        every GraphQL call does in a cloud session."""
        table = {f"api repos/o/r/compare/{b}...main --jq .behind_by": "0",
                 "alias list": "mrg: 'api -X PUT repos/o/r/pulls/$1/merge -f merge_method=merge -f sha=$2'"}
        for n, (head, reviews, commits) in prs.items():
            url = f"https://github.com/o/r/pull/{n}"
            posted = [{"commit": c, "state": s, "submittedAt": None if s == "PENDING" else "1"} for c, s in reviews]
            for slug in ("o/r", "{owner}/{repo}"):  # gh fills {owner}/{repo} in, so a guard reading it would pass
                table[f"api repos/{slug}/pulls/{n} --jq {{number, url: .html_url, head: .head.sha, base: .base.ref}}"] = \
                    json.dumps({"number": n, "url": url, "head": head, "base": "main"})
            table[f"api --paginate repos/o/r/pulls/{n}/reviews --jq .[] | "
                  "{commit: .commit_id, state, submittedAt: .submitted_at}"] = "\n".join(json.dumps(r) for r in posted)
            table[f"api --paginate repos/o/r/pulls/{n}/commits --jq .[] | {{sha, parents: [.parents[].sha]}}"] = \
                "\n".join(json.dumps({"sha": c, "parents": ps}) for c, ps in commits.items())
            if graphql:
                table[f"pr view {n} --json number,url,headRefOid,baseRefName,reviews"] = json.dumps(
                    {"number": n, "url": url, "headRefOid": head, "baseRefName": "main",
                     "reviews": [{"commit": {"oid": r["commit"]}, "state": r["state"], "submittedAt": r["submittedAt"]}
                                 for r in posted]})
        return table

    put = "gh api -X PUT repos/o/r/pulls/{n}/merge -f merge_method=merge -f sha={sha}"
    cases = [  # (command, allowed, GraphQL served)
        # the REST merge: a review on the head, or on an earlier commit followed only by clean base merges
        (put.format(n=21, sha=a), True, False),
        (put.format(n=22, sha=m), True, False),
        (f"gh api --method PUT repos/o/r/pulls/21/merge -F merge_method=merge -F sha={a}", True, False),
        (f"gh api --method=PUT repos/o/r/pulls/21/merge --field sha={a} --raw-field merge_method=merge", True, False),
        (f"gh api -XPUT /repos/o/r/pulls/21/merge -fmerge_method=merge -f=sha={a}", True, False),
        (f"timeout 60 {put.format(n=22, sha=m)} && echo merged", False, False),  # a wrapper hides it from Codex
        (f"true && {put.format(n=22, sha=m)} && echo merged", True, False),
        (f"gh api -X=PUT repos/o/r/pulls/21/merge -f merge_method=merge -f sha={a}", True, False),
        # no review covers the head: none, one on an older plain commit, one still pending
        (put.format(n=24, sha=a), False, False),
        (put.format(n=23, sha=d), False, False),
        (put.format(n=25, sha=a), False, False),
        # sha isn't the head: the reviewed commit the head has moved past, or another commit
        (put.format(n=22, sha=a), False, False),
        (put.format(n=21, sha=b), False, False),
        # merge_method squash, rebase, missing, given twice (gh applies -F last), from a file or a variable
        (f"gh api -X PUT repos/o/r/pulls/21/merge -f merge_method=squash -f sha={a}", False, False),
        (f"gh api -X PUT repos/o/r/pulls/21/merge -f merge_method=rebase -f sha={a}", False, False),
        (f"gh api -X PUT repos/o/r/pulls/21/merge -f sha={a}", False, False),
        (f"gh api -X PUT repos/o/r/pulls/21/merge -f merge_method=merge -f merge_method=squash -f sha={a}", False, False),
        (f"gh api -X PUT repos/o/r/pulls/21/merge -F merge_method=squash -f merge_method=merge -f sha={a}", False, False),
        (f"gh api -X PUT repos/o/r/pulls/21/merge -F merge_method=@method.txt -f sha={a}", False, False),
        (f"gh api -X PUT repos/o/r/pulls/21/merge -f merge_method=$M -f sha={a}", False, False),
        # sha missing, short, upper case, computed at run time, given twice, or from a file
        ("gh api -X PUT repos/o/r/pulls/21/merge -f merge_method=merge", False, False),
        (put.format(n=21, sha=a[:7]), False, False),
        (put.format(n=21, sha=a.upper()), False, False),
        (put.format(n=21, sha="$SHA"), False, False),
        (put.format(n=21, sha="\"$(git rev-parse HEAD)\""), False, False),
        (put.format(n=21, sha=a) + f" -f sha={b}", False, False),
        ("gh api -X PUT repos/o/r/pulls/21/merge -f merge_method=merge -F sha=@sha.txt", False, False),
        # the PR number computed at run time, or another endpoint or method
        (put.format(n="$N", sha=a), False, False),
        (put.format(n="\"$(cat pr.txt)\"", sha=a), False, False),
        (put.format(n=21, sha=a).replace("o/r", "{owner}/{repo}"), False, False),
        (f"gh api -X PUT 'repos/o/r/pulls/21/merge?merge_method=squash' -f merge_method=merge -f sha={a}", False, False),
        (put.format(n=21, sha=a).replace("repos/", "https://api.github.com/repos/"), False, False),
        (put.format(n=21, sha=a).replace("PUT", "POST"), False, False),
        (put.format(n=21, sha=a).replace("-X PUT ", ""), False, False),
        (put.format(n=21, sha=a).replace("-X PUT", "-X POST -X PUT"), False, False),
        (f"gh api repos/o/r/pulls/21/merge -X PUT -f merge_method=merge -f sha={a}", False, False),  # Codex asks only for `gh api -X PUT`
        (put.format(n=21, sha=a).replace("PUT", "put"), False, False),
        # extra fields, options and arguments that change the request
        (put.format(n=21, sha=a) + " -f commit_title=x", False, False),
        (put.format(n=21, sha=a) + " --input body.json", False, False),
        (put.format(n=21, sha=a) + " -H 'X-HTTP-Method-Override: PATCH'", False, False),
        (put.format(n=21, sha=a) + " --hostname example.com", False, False),
        (put.format(n=21, sha=a) + " extra", False, False),
        (put.format(n=21, sha=a).replace("gh api", "gh --hostname example.com api"), False, False),
        ("echo x | xargs " + put.format(n=21, sha=a), False, False),
        # every other merge, ref and contents write stays blocked
        (f"gh api -X PUT repos/o/r/contents/a.txt -f merge_method=merge -f sha={a}", False, False),
        (f"gh api -X PATCH repos/o/r/git/refs/heads/main -f sha={a}", False, False),
        (f"gh api repos/o/r/merges -f merge_method=merge -f sha={a}", False, False),
        ("gh api -X POST repos/o/r/branches/main/rename -f new_name=x", False, False),
        (f"curl -X PUT https://api.github.com/repos/o/r/pulls/21/merge -d '{{\"merge_method\":\"merge\",\"sha\":\"{a}\"}}'",
         False, False),
        ("gh api graphql -f query='mutation { mergePullRequest(input: {pullRequestId: \"x\"}) { clientMutationId } }'",
         False, False),
        # endpoint spellings GitHub routes to the merge, ref, merges and contents endpoints (review round 1)
        *((f"gh api -X PUT '{ep}' -f merge_method=squash", False, False) for ep in (
            "repos/o/r/pulls/24/merge#", "repos/o/r/pulls/24/merge#x", "repos/o/r/pulls/24/merge/.",
            "repos/o/r/pulls/24/./merge", "repos/o/r/pulls/24/x/../merge", "repos/o/r/pulls/24//merge",
            "repos/o/r/pulls//24/merge", "repos/o/r/pulls/24/%6Derge", "repos/o/r/pulls/24%2Fmerge",
            "repos/o/r/pulls/24/merge;x", "repos/o/r/pulls/24/MERGE", "repos/o/r/PULLS/24/merge",
            "repos/o/r/pulls/24/%256Derge")),
        (f"gh api -X PUT repos/o/r/pulls/21/merge# -f merge_method=merge -f sha={a}", False, False),
        (f"gh api -X PATCH repos/o/r/git/./refs/heads/main -f sha={a}", False, False),
        (f"gh api -X PATCH repos/o/r/git//refs/heads/main -f sha={a}", False, False),
        (f"gh api -X PATCH repos/o/r/GIT/REFS/heads/main -f sha={a}", False, False),
        ("gh api repos/o/r/merges# -f base=main -f head=x", False, False),
        ("gh api repos/o/r/merges/. -f base=main -f head=x", False, False),
        ("gh api -X PUT repos/o/r/CONTENTS/a.txt -f message=m", False, False),
        ("gh api /graphql -f query='mutation { mergePullRequest(input: {pullRequestId: \"x\"}) { clientMutationId } }'",
         False, False),
        ("gh api https://api.github.com/graphql -f query='mutation { updateRef(input: {}) { clientMutationId } }'",
         False, False),
        ("curl -X PUT 'https://api.github.com/repos/o/r/pulls/24/merge#' -d '{\"merge_method\":\"squash\"}'", False, False),
        ("curl -X PUT https://api.github.com/repos/o/r/pulls/24/./merge -d '{\"merge_method\":\"squash\"}'", False, False),
        # the cloud session's auto-merge route, and REST update-branch (review round 1); other CCR routes pass
        ("gh api -X PUT repos/o/r/pulls/24/ccr/auto_merge", False, False),
        ("gh api -X PUT repos/o/r/pulls/21/ccr/auto_merge -f merge_method=SQUASH", False, False),
        ("gh api -X DELETE repos/o/r/pulls/21/ccr/auto_merge", False, False),
        ("gh api -X PUT 'repos/o/r/pulls/24/ccr//auto_merge'", False, False),
        (f"gh api -X PUT repos/o/r/pulls/21/update-branch -f expected_head_sha={a}", False, False),
        ("curl -X PUT https://api.github.com/repos/o/r/pulls/24/ccr/auto_merge", False, False),
        ("gh api repos/o/r/pulls/21/ccr/review_threads", True, False),
        ("gh api -X POST repos/o/r/pulls/21/ccr/ready_for_review", True, False),
        # only as typed, where a prefix rule (Codex's) sees `gh api -X PUT`; no wrapper, VAR=, alias or nesting
        *((form.format(put.format(n=21, sha=a)), False, False) for form in (
            "env {}", "GH_X=1 {}", "nice {}", "{{ {}; }}", "bash -c '{}'", "eval '{}'", "x=$({})")),
        (put.format(n=21, sha=a).replace("gh api", "/usr/bin/gh api"), False, False),
        (f"gh mrg 21 {a}", False, False),
        # an encoded `?` or `#` is part of its segment, not a cut (review round 2): `main` refuses all of these
        *((f"gh api -X PUT repos/o/r/pulls/{ep} -f merge_method={how}", False, False) for ep, how in (
            ("24%3F/merge", "squash"), ("24%23/merge", "squash"), ("24%3f/merge", "squash"),
            ("24%3Fx/merge", "rebase"), ("24%253F/merge", "squash"), ("24%2523/merge", "squash"))),
        ("gh api -X PUT repos/o/r/pulls/24%3F/ccr/auto_merge", False, False),
        ("gh api -X PUT repos/o/r/pulls/24%23/update-branch", False, False),
        ("curl -X PUT https://api.github.com/repos/o/r/pulls/24%3F/merge -d '{\"merge_method\":\"squash\"}'", False, False),
        # a write whose endpoint hides `/`, `.`, `\` or `;` behind percent-encoding is refused, not guessed
        *((f"gh api -X PUT repos/o/r/pulls/24/{ep} -f merge_method=squash", False, False)
          for ep in ("merge%2F%2E%2E", "merge%5C", "merge%3Bx")),
        # curl's host with a port, and its host found by a greedy match (review round 2)
        ("curl -X POST https://api.github.com:443/graphql "
         "-d '{\"query\":\"mutation { mergePullRequest(input: {}) { clientMutationId } }\"}'", False, False),
        ("curl -X POST https://api.github.com:443/graphql/ "
         "-d '{\"query\":\"mutation { mergePullRequest(input: {}) { clientMutationId } }\"}'", False, False),
        ("curl -X PUT 'https://api.github.com/repos/o/r/pulls/24/merge?x=api.github.com' -d '{}'", False, False),
        ("curl -X PUT https://api.github.com:443/repos/o/r/pulls/24/merge -d '{}'", False, False),
        # options and run-time words the review found no failing case for (review round 1)
        *((put.format(n=21, sha=a) + extra, False, False) for extra in (
            " --input=body.json", " --hostname=example.com", " --verbose", " --silent", " -i")),
        (put.format(n=21, sha=a).replace("-f merge_method=merge", "-fmerge_method=$'\\merge'"), False, False),
        (put.format(n=21, sha=a).replace("-f merge_method=merge", "-f merge_method=$'\\merge'"), False, False),
        # gh pr merge reads the PR with gh pr view; without GraphQL it is refused, even on a covered head
        ("gh pr merge 22 --merge", True, True),
        ("gh pr merge 24 --merge", False, True),
        ("gh pr merge 25 --merge", False, True),
        ("gh pr merge 22 --merge", False, False),
        ("gh pr merge https://github.com/o/r/pull/21 --merge", False, False),
        ("gh pr merge 21 --merge --repo o/r", False, False),
        ("gh pr merge 23 --merge", False, False),
        ("gh pr merge 25 --merge", False, False),
        ("gh pr merge --merge", False, False),
    ]
    assert len(cases) == REST_CASES
    real_run, failures = run, 0

    def faking(table):
        def answer(args, cwd, timeout=20):
            return table.get(" ".join(args[1:])) if args[:1] == ["gh"] else real_run(args, cwd, timeout)
        return answer

    try:
        for cmd, ok, graphql in cases:
            run = faking(answers(graphql))
            reason = decide({"tool_input": {"command": cmd}, "cwd": repo})
            if (reason is None) != ok:
                where = "with" if graphql else "without"
                print(f"FAIL (merge {where} GraphQL, should {'allow' if ok else 'block'}): {cmd!r}: {reason}")
                failures += 1
    finally:
        run = real_run
    return failures


if __name__ == "__main__":
    sys.exit(main())
