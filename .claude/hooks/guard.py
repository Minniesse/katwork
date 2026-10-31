#!/usr/bin/env python3
"""PreToolUse guard for Bash calls, shared by Claude Code and Codex.

Reads the hook event JSON on stdin. Exit 0 lets the command run; exit 2 blocks
it and the reason on stderr goes back to the agent.

It decides in three steps, so it never allows what main's guard refused,
apart from one command line:

1. main's guard decides. guard_main.py beside this file is main's guard.py at
   a536fa5, byte for byte, run unchanged. Its docstring lists what it blocks:
   pushes that update or delete main, `git reset --hard`, a deleting
   `git clean`, commits whose identity isn't a GitHub noreply address,
   `gh pr merge` with --admin, --auto, --squash or --rebase or without a review
   that covers the head, and `gh api`, curl and GraphQL writes that merge PRs
   or move refs.
2. Where it refuses, one command line may still pass: exactly
   `gh api -X PUT repos/<owner>/<repo>/pulls/<n>/merge -f merge_method=merge -f sha=<head sha>`
   (or `--method PUT`, or `-F` for a field) and nothing else on the line,
   when sha is the PR's head and a review covers it, both read over REST
   (canonical_rest_merge, rest_review_check). It is the merge for sessions
   without GraphQL.
3. Where it allows, the checks here may still refuse (extra_reason):
   - a write to a merge, ref, merges, contents or rename endpoint in any
     spelling GitHub routes there (api_path), and a write whose path hides a
     delimiter behind percent-encoding (hides_delimiter);
   - the cloud auto-merge route (`pulls/<n>/ccr/auto_merge`) and
     `pulls/<n>/update-branch`;
   - GraphQL mutations sent to `/graphql` or a full URL;
   - for curl, httpie and xh: the method inside a short-option cluster
     (`-sX PUT`), a method word (`http PUT`), request items (`key=value`),
     a URL glob (`{merge}`), and an upper-case host;
   - `gh pr merge` when the only review covering the head is pending.

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
import types
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))


def load_main_guard():
    """guard_main.py, main's guard as it was, loaded as a module of its own."""
    path = os.path.join(HERE, "guard_main.py")
    module = types.ModuleType("guard_main")
    module.__file__ = path
    with open(path) as f:
        exec(compile(f.read(), path, "exec"), module.__dict__)
    return module


try:
    base = load_main_guard()
except Exception as e:  # without main's guard nothing can be checked, so main() refuses everything
    base, LOAD_ERROR = None, f"{type(e).__name__}: {e}"
if base is not None:
    Blocked, NOREPLY, GRAPHQL_WRITES = base.Blocked, base.NOREPLY, base.GRAPHQL_WRITES
    command_text, merge_verdict, merge_is_clean, run = base.command_text, base.merge_verdict, base.merge_is_clean, base.run
    MAIN = {name: getattr(base, name) for name in ("check_gh_api", "check_http_client", "review_check")}


def use_run(fn):
    """Send every `gh` and git call, here and in main's guard, to fn."""
    global run
    run = base.run = fn


# ================================================================ step 2: the one REST merge

REST_MERGE = "gh api -X PUT repos/<owner>/<repo>/pulls/<n>/merge -f merge_method=merge -f sha=<head sha>"
MERGE_WAYS = (f"Merge with `gh pr merge <n> --merge`, or where GraphQL is unavailable with the whole command line "
              f"`{REST_MERGE}`, every part written out; push branches with `git push`. Those are checked.")
CANONICAL_MERGE = re.compile(
    r"\s*gh[ \t]+api[ \t]+(?:-X|--method)[ \t]+PUT[ \t]+repos/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)/pulls/([1-9][0-9]*)/merge"
    r"[ \t]+-[fF][ \t]+merge_method=merge[ \t]+-[fF][ \t]+sha=([0-9a-f]{40})\s*")


def canonical_rest_merge(text):
    """(owner/repo, number, sha) when the whole command line is the REST merge,
    word for word, with no quotes, variables or other words; else None. The
    owner and repo are plain names (no `..`, not only dots), so the path the
    line names is the path GitHub routes."""
    m = CANONICAL_MERGE.fullmatch(text)
    if not m or any(".." in name or not name.strip(".") for name in m.group(1, 2)):
        return None
    return f"{m.group(1)}/{m.group(2)}", m.group(3), m.group(4)


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
    """main's merge_verdict for a PR read by pr_from_view or pr_over_rest, with
    the commits, base and clean-merge checks main's review_check makes."""
    slug, number, base_ref, reviews = pr["slug"], pr["number"], pr["base"], pr["reviews"]
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
        behind = run(["gh", "api", f"repos/{slug}/compare/{sha}...{base_ref}", "--jq", ".behind_by"], cwd)
        return behind == "0"

    def merge_clean(merge, first, second):
        return base.have_commits([merge, first, second], slug, cwd) and merge_is_clean(merge, first, second, cwd)

    return merge_verdict(pr["head"], reviews, parents, base_ancestor, merge_clean)


# ================================================================ step 3: refusals added where main allows

WRITE_PATHS = re.compile(r"(^|/)(pulls/[^/]+/(merge|ccr/auto_merge|update-branch)|merges|git/refs(/.*)?|"
                         r"contents/.*|branches/[^/]+/rename)$")
MERGE_PATH = re.compile(r"(^|/)pulls/[^/]+/merge$")
AUTO_MERGE_PATH = re.compile(r"(^|/)pulls/[^/]+/ccr/auto_merge$")  # the cloud session's route for auto-merge
HTTP_METHODS = {"GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"}


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


def write_reason(path, hidden):
    """Why a write step 3 refuses is refused."""
    if hidden:
        return ("this write's endpoint hides a `?`, `#`, `/`, `\\`, `.` or `;` behind percent-encoding, so GitHub "
                f"may route it either way. Write the path out plainly. {MERGE_WAYS}")
    if AUTO_MERGE_PATH.search(path):
        return ("auto-merge (`pulls/<n>/ccr/auto_merge`) merges later, after the review check, like "
                f"`gh pr merge --auto`. Merge once a review covers the head, with the whole command line `{REST_MERGE}`.")
    if MERGE_PATH.search(path):
        return (f"a REST merge passes only as the whole command line `{REST_MERGE}` (or with `--method PUT`, or `-F` "
                "for a field), every part written out and nothing else on the line, once a review covers the head.")
    return f"API writes that merge PRs or move, delete or write branches are blocked. {MERGE_WAYS}"


def api_request(args, cwd, stdin):
    """What a `gh api` call sends, read the way main's check_gh_api reads it:
    (method, endpoint word, whether fields or --input make it a write,
    (text, dynamic) body pieces, whether some body can't be read)."""
    method, endpoint, writes, unreadable = None, None, False, False
    bodies = []
    stdin_texts, stdin_complete = stdin
    i = 0
    while i < len(args):
        a = args[i].text
        flag, value = base.split_api_flag(a)
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
        kind = base.GH_API_VALUE_FLAGS[flag]
        if kind == "method":
            method = value.upper()
            continue
        if kind == "other":
            continue
        writes = True
        if kind == "field":
            bodies.append((value, dynamic))
            source = value.split("=", 1)[1] if "=" in value else ""
            if flag in ("-f", "--raw-field") or not source.startswith("@"):
                continue
            source = source[1:]
        else:
            source = value
        if source in base.STDIN_PATHS:
            bodies.extend((t, False) for t in stdin_texts)
            unreadable = unreadable or not stdin_complete
        elif dynamic and not source.startswith("<("):
            unreadable = True
        else:
            t, c = base.redirect_input(source, cwd, 0)
            bodies.extend((x, False) for x in t)
            unreadable = unreadable or not c
    return method, endpoint, writes, bodies, unreadable


def check_gh_api_extra(args, cwd, stdin=([], True)):
    """Step 3 for `gh api`, then main's own check_gh_api. An endpoint that
    reads as graphql gets main's GraphQL test and still the write test."""
    method, endpoint, writes, bodies, unreadable = api_request(args, cwd, stdin)
    if endpoint is not None:
        path = api_path(endpoint.text)
        if path == "graphql":
            texts = []
            for body, dynamic in bodies:
                t, c = base.resolve_text(body, dynamic, cwd, 0)
                texts, unreadable = texts + t, unreadable or not c
            text = " ".join(texts + [base.json_strings(b) for b in texts])
            if GRAPHQL_WRITES.search(text):
                raise Blocked(f"GraphQL mutations that merge PRs or move refs are blocked. {MERGE_WAYS}")
            if unreadable:
                raise Blocked("can't read this GraphQL request's body, so it can't be checked for merges or ref "
                              "writes. Pass the query inline with -f query=..., or from a file in the repo.")
        write = (method or ("POST" if writes else "GET")) not in ("GET", "HEAD")
        hidden = hides_delimiter(endpoint.text)
        if write and (hidden or WRITE_PATHS.search(path)):
            raise Blocked(write_reason(path, hidden))
    MAIN["check_gh_api"](args, cwd, stdin)


def http_request(texts):
    """(method, sends data) for curl, wget, httpie or xh arguments: -X,
    --request and --method in any spelling, also inside a short-option
    cluster (`-sX PUT`, `-sXPUT`); a method word (`http PUT`, `xh put`); data
    options; and httpie request items (`key=value`, `key:=json`, `key@file`)."""
    method, data = None, False
    for k, t in enumerate(texts):
        following = texts[k + 1] if k + 1 < len(texts) else ""
        if t in ("-X", "--request", "--method"):
            method = following
        elif t.startswith(("--request=", "--method=")):
            method = t.split("=", 1)[1]
        elif t.startswith("-") and not t.startswith("--") and "X" in t:
            method = t[t.index("X") + 1 :].lstrip("=") or following
        elif not t.startswith("-") and t.upper() in HTTP_METHODS:
            method = t
        if t.startswith(("-d", "--data", "--json", "-F", "--form", "-T", "--upload-file", "--post-data",
                         "--post-file", "--body-data")) or re.fullmatch(r"[\w.\[\]-]+(?::=|=|@)[^=].*|[\w.\[\]-]+=", t):
            data = True
    return (method.upper() if method else None), data


def check_http_client_extra(args):
    """Step 3 for curl, wget, httpie and xh, then main's own check_http_client."""
    texts = [w.text for w in args]
    urls = [t for t in texts if "api.github.com" in t.lower()]
    if urls:
        method, data = http_request(texts)
        write = (method or ("POST" if data else "GET")) not in ("GET", "HEAD")
        joined = " ".join(texts)
        globbing = not any(t == "--globoff" or re.fullmatch(r"-[A-Za-z]*g[A-Za-z]*", t) for t in texts)
        for url in urls:
            rest = re.sub(r"^(?:[A-Za-z][A-Za-z0-9+.-]*://)?[^/?#]*", "", url)  # after the scheme, host and any port
            path = api_path(rest)
            if path == "graphql" and GRAPHQL_WRITES.search(joined):
                raise Blocked(f"GraphQL mutations that merge PRs or move refs are blocked. {MERGE_WAYS}")
            if write and globbing and re.search(r"[{}\[\]]", url):
                raise Blocked("this write's URL holds a curl glob (`{...}` or `[...]`), so the guard can't tell which "
                              f"path it reaches. Write the URL out, or pass -g. {MERGE_WAYS}")
            hidden = hides_delimiter(rest)
            if write and (hidden or WRITE_PATHS.search(path)):
                raise Blocked(write_reason(path, hidden))
    MAIN["check_http_client"](args)


def review_check_extra(target, repo, cwd):
    """Step 3 for `gh pr merge`: main's review rule with pending reviews
    dropped. Its gh reads are the ones main's review_check just made."""
    view = ["gh", "pr", "view"] + ([target] if target else []) + repo
    out = run(view + ["--json", "number,url,headRefOid,baseRefName,reviews"], cwd)
    if out is None:
        return ("couldn't read the PR with `gh pr view`, so the merge can't be checked. Where GraphQL is "
                f"unavailable, merge with the whole command line `{REST_MERGE}`.")
    pr, reason = pr_from_view(out)
    return reason or pr_verdict(pr, cwd)


EXTRA = {"check_gh_api": check_gh_api_extra, "check_http_client": check_http_client_extra,
         "review_check": review_check_extra}


def extra_reason(payload):
    """Step 3: main's guard walks the command again with the checks above in
    place of its check_gh_api, check_http_client and review_check. The first
    two end by running main's own; the third is main's review rule with
    pending reviews dropped. decide calls this only where main's guard
    allowed, or to word a refusal main's guard already made, so it can only
    refuse more."""
    for name, fn in EXTRA.items():
        setattr(base, name, fn)
    try:
        return base.decide(payload)
    finally:
        for name, fn in MAIN.items():
            setattr(base, name, fn)


# ================================================================ entry


def decide(payload):
    """Return None to allow, or the reason to block, in the three steps of
    the module docstring."""
    cwd = payload.get("cwd") or os.getcwd()
    inner, memo = run, {}

    def cached(args, cwd, timeout=20):  # gh reads repeat between the steps; git's answers can change (fetch)
        if args[:1] != ["gh"]:
            return inner(args, cwd, timeout)
        key = (tuple(args), cwd)
        if key not in memo:
            memo[key] = inner(args, cwd, timeout)
        return memo[key]

    use_run(cached)
    try:
        refused = base.decide(payload)  # 1. main's guard
        if refused is None:
            return extra_reason(payload)  # 3. refusals it adds
        merge = canonical_rest_merge(command_text(payload))  # 2. the one line it may still pass
        if merge is None:
            return extra_reason(payload) or refused
        reason = rest_review_check(*merge, cwd)
        return None if reason is None else f"PR can't merge yet: {reason}"
    finally:
        use_run(inner)


def main():
    if base is None:
        print(f"BLOCKED by .claude/hooks/guard.py: can't load guard_main.py beside it ({LOAD_ERROR}), so nothing "
              "can be checked.", file=sys.stderr)
        return 2
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
    """Offline: no network, no `gh`. Scratch repos stand in for this one. The
    differential reads main's guard at DIFF_BASELINE from this clone's git,
    and checks that guard_main.py is that guard, byte for byte."""
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
    total = len(blocked) + len(allowed) + generated + MERGE_CASES + REST_CASES + DIFF_CASES + PIN_CASES
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


REST_CASES = 174


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
            for slug in ("o/r", "{owner}/{repo}", "o/.."):  # gh fills {owner}/{repo} in; `..` might route to o/r too
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
        # only the whole command line passes (round 3): other spellings and chains are refused
        (f"gh api --method=PUT repos/o/r/pulls/21/merge --field sha={a} --raw-field merge_method=merge", False, False),
        (f"gh api -XPUT /repos/o/r/pulls/21/merge -fmerge_method=merge -f=sha={a}", False, False),
        (f"timeout 60 {put.format(n=22, sha=m)} && echo merged", False, False),  # a wrapper hides it from Codex
        (f"true && {put.format(n=22, sha=m)} && echo merged", False, False),
        (f"gh api -X=PUT repos/o/r/pulls/21/merge -f merge_method=merge -f sha={a}", False, False),
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
        (f"gh api -X PUT repos/o/../pulls/21/merge -f merge_method=merge -f sha={a}", False, False),  # a `..` name
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
        # api_path's `\` step and its trimming each have a case (review round 2)
        ("gh api -X PUT 'repos/o/r/pulls/24\\merge' -f merge_method=squash", False, False),
        ("gh api -X PUT repos/o/r/pulls/24/merge%20 -f merge_method=squash", False, False),
        # lines Codex doesn't split into plain commands, so its prefix rules never see `gh api -X PUT` (round 2)
        *((form.format(p=put.format(n=21, sha=a)), False, False) for form in (
            "( {p} )", "{p} &", "true & {p}", "{p} 2>&1", "{p} 2>/dev/null", "{p} > out.txt", "{p} < /dev/null",
            "{p} <<< x", "{p} # comment", "case x in x) {p} ;; esac", "f() ( {p} ); f", "true\n{p}")),
        *((put.format(n=21, sha=a).replace(old, new), False, False) for old, new in (
            ("gh api", "\\gh api"), ("gh api", '"gh" api'), ("gh api", 'g"h" api'), ("gh api", "gh a\\pi"),
            ("-X PUT", "-X $'PUT'"), ("-X PUT", "-X PU\\T"), ("repos/o/r/pulls/21/merge", "'repos/o/r/pulls/21/merge'"))),
        (put.format(n=21, sha=a) + " | cat", False, False),
        # options and run-time words the review found no failing case for (review round 1)
        *((put.format(n=21, sha=a) + extra, False, False) for extra in (
            " --input=body.json", " --hostname=example.com", " --verbose", " --silent", " -i")),
        (put.format(n=21, sha=a).replace("-f merge_method=merge", "-fmerge_method=$'\\merge'"), False, False),
        (put.format(n=21, sha=a).replace("-f merge_method=merge", "-f merge_method=$'\\merge'"), False, False),
        # endpoints that climb to graphql skipped the write check (review round 2): main refuses all of these
        ("EP='repos/o/r/pulls/24/merge#'; gh api -X PUT \"$EP/../graphql\" -f merge_method=merge", False, False),
        ("gh api -X PUT 'repos/o/r/contents/a.txt/../../../../../graphql' -f message=m", False, False),
        ("gh api -X PUT 'repos/o/r/contents/a\\..\\..\\..\\..\\..\\graphql' -f message=m", False, False),
        ("gh api -X PUT 'repos/o/r/contents/a.txt/..;/..;/..;/..;/..;/graphql' -f message=m", False, False),
        (f"gh api -X PATCH repos/o/r/git/refs/heads/x/../../../../../../../graphql -f sha={a}", False, False),
        ("gh api -X DELETE repos/o/r/git/refs/heads/x/../../../../../../../graphql", False, False),
        # dot segments at the end, and curl to a path ending in graphql (review round 2): main refuses these too
        ("gh api -X PUT repos/o/r/contents/x/.. -f message=m", False, False),
        ("gh api -X DELETE repos/o/r/git/refs/../", False, False),
        ("curl -X POST https://api.github.com/repos/o/r/x/graphql "
         "-d '{\"query\":\"mutation { mergePullRequest(input: {}) { clientMutationId } }\"}'", False, False),
        # curl's method in a short cluster, URL globs, httpie and xh method words and items (review round 2)
        ("curl -sX PUT https://api.github.com/repos/o/r/pulls/24/merge", False, False),
        ("curl -sXPUT https://api.github.com/repos/o/r/pulls/24/merge", False, False),
        ("curl -X PUT 'https://api.github.com/repos/o/r/pulls/24/{merge}'", False, False),
        ("curl -X PUT 'https://api.github.com/repos/o/r/pulls/24/[m-m]erge'", False, False),
        ("http PUT https://api.github.com/repos/o/r/pulls/24/merge", False, False),
        ("xh put https://api.github.com/repos/o/r/pulls/24/merge", False, False),
        (f"http https://api.github.com/repos/o/r/git/refs ref=refs/heads/x sha={a}", False, False),
        ("curl -X PUT https://API.GITHUB.COM/repos/o/r/pulls/24/merge -d '{}'", False, False),
        ("curl -sX GET https://api.github.com/repos/o/r/pulls/24/merge", True, False),
        ("curl -sg 'https://api.github.com/repos/o/r/pulls/24/{merge}'", True, False),
        ("http GET https://api.github.com/repos/o/r/pulls/24", True, False),
        # anything else on the line, which Codex's parser wouldn't split (review round 2), and hidden `.` and `/`
        ("x=1 && " + put.format(n=21, sha=a), False, False),
        ("x=1; " + put.format(n=21, sha=a), False, False),
        ("echo =x && " + put.format(n=21, sha=a), False, False),
        *((f"gh api -X PUT repos/o/r/pulls/24/{ep} -f merge_method=squash", False, False)
          for ep in ("merge%2E", "merge%2F", "%2E/merge")),
        # the whole line in its other spellings
        (f"gh api --method PUT repos/o/r/pulls/21/merge -f merge_method=merge -F sha={a}", True, False),
        (f"  {put.format(n=21, sha=a)}  ", True, False),
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
        # Protection only grows: what main's guard refuses, this one refuses, but the canonical REST merge.
        source = baseline_source()
        corpus, baseline = diff_corpus({21: a, 22: m, 24: a}), baseline_guard(source)
        assert len(corpus) == DIFF_CASES
        if globals().get("base") is not None:  # step 1's guard_main.py (an older single-file guard has none)
            with open(os.path.join(HERE, "guard_main.py")) as f:
                if source is None or f.read() != source:
                    print(f"FAIL (main's guard): guard_main.py isn't the guard at {DIFF_BASELINE[:7]} byte for byte, "
                          "and step 1 must run that guard unchanged.")
                    failures += 1
        if baseline is None:
            print(f"FAIL (differential): can't read the guard at {DIFF_BASELINE[:7]} from git. Fetch it, then rerun.")
            failures += DIFF_CASES
        for cmd, graphql, may_allow in corpus if baseline else ():
            run = baseline.run = faking(answers(graphql))
            payload = {"tool_input": {"command": cmd}, "cwd": repo}
            new, old = decide(payload), baseline.decide(payload)
            if new is None and old is not None and not may_allow:
                print(f"FAIL (differential: main refuses, this guard allows): {cmd!r}: main says {old}")
                failures += 1
            elif may_allow and new is not None:
                print(f"FAIL (differential: the canonical REST merge should pass): {cmd!r}: {new}")
                failures += 1
    finally:
        run = real_run
    return failures


DIFF_BASELINE = "a536fa524711e1042d6fcee54cd4e849e51e3b66"  # main's guard before it learned the REST merge
DIFF_CASES = 3030
PIN_CASES = 1  # guard_main.py is the guard at DIFF_BASELINE


def baseline_source():
    """main's guard.py at DIFF_BASELINE, read from this clone's git, or None
    when the clone doesn't have that commit."""
    for where in (os.path.dirname(os.path.abspath(__file__)), os.getcwd()):
        out = subprocess.run(["git", "-C", where, "show", f"{DIFF_BASELINE}:.claude/hooks/guard.py"],
                             capture_output=True, text=True)
        if out.returncode == 0:
            return out.stdout
    return None


def baseline_guard(source):
    """That guard loaded as a module of its own, or None without its source."""
    import types

    if source is None:
        return None
    module = types.ModuleType("guard_at_" + DIFF_BASELINE[:7])
    exec(compile(source, f"guard.py@{DIFF_BASELINE[:7]}", "exec"), module.__dict__)
    return module


def diff_corpus(heads):
    """Write commands for the differential. Each merge, auto-merge,
    update-branch, ref, merges, contents and rename endpoint (on PRs 21, 22
    and 24 where it names one) in 35 spellings: fragments and queries, case,
    `.`, `..` and `//` in the middle and at the end, `\\`, `;`, spaces, and
    single, double and triple percent-encoding of letters, `/`, `.`, `?` and
    `#`. Each runs through gh api as a squash, a merge with the head's sha, a
    DELETE and a POST, and through curl with and without a port. Then:
    endpoints that climb to `graphql` (`"$EP/../graphql"`, contents and
    git/refs), GraphQL mutations by gh api and curl, curl host spellings, the
    REST merge's field and line forms, under wrappers, and gh pr merge. Items
    are (command, GraphQL served, may pass where main refused): only the
    whole-line REST merge of a covered head (PR 21 or 22) may."""
    def spellings(ep):
        head, _, last = ep.rpartition("/")
        enc = "%%%02X" % ord(last[0])
        return [("plain", ep), ("slash", "/" + ep), ("trailing", ep + "/"), ("fragment", ep + "#"),
                ("fragment2", ep + "#x"), ("query", ep + "?x=1"), ("upper", ep.upper()), ("dot", f"{head}/./{last}"),
                ("dotdot", f"{head}/x/../{last}"), ("double", f"{head}//{last}"), ("enc", f"{head}/{enc}{last[1:]}"),
                ("enc-lower", f"{head}/{enc.lower()}{last[1:]}"), ("enc2", f"{head}/%25{enc[1:]}{last[1:]}"),
                ("enc-slash", f"{head}%2F{last}"), ("enc2-slash", f"{head}%252F{last}"), ("enc-?", f"{head}%3F/{last}"),
                ("enc-#", f"{head}%23/{last}"), ("enc2-?", f"{head}%253F/{last}"), ("enc2-#", f"{head}%2523/{last}"),
                ("param", f"{ep};x"), ("param-mid", f"{head};x/{last}"), ("backslash", f"{head}\\{last}"),
                ("space", f"{ep}%20"), ("enc-dot", f"{head}/%2E/{last}"), ("enc-dotdot", f"{head}/x/%2E%2E/{last}"),
                ("trail-dot", ep + "/."), ("trail-dotdot", ep + "/.."), ("trail-x-dotdot", ep + "/x/.."),
                ("enc-slash-lower", f"{head}%2f{last}"), ("enc3-slash", f"{head}%25252F{last}"),
                ("enc-dotdot-lower", f"{head}/x/%2e%2e/{last}"), ("enc-climb", f"{ep}%2F..%2F{last}"),
                ("frag-query", ep + "#?"), ("query-frag", ep + "?#"), ("frag-climb", ep + "#/../x")]

    def quoted(text):  # its own pattern, so this self-test also runs against older checking code
        return text if re.fullmatch(r"[A-Za-z0-9_@%+=:,./-]+", text) else shlex.quote(text)

    corpus = []
    targets = [(f"repos/o/r/pulls/{n}/{tail}", n) for tail in ("merge", "ccr/auto_merge", "update-branch")
               for n in (21, 22, 24)]
    targets += [(ep, 24) for ep in ("repos/o/r/git/refs/heads/main", "repos/o/r/git/refs", "repos/o/r/merges",
                                    "repos/o/r/contents/a.txt", "repos/o/r/branches/main/rename")]
    for ep, n in targets:
        for name, spelled in spellings(ep):
            canonical = n in (21, 22) and ep.endswith(f"/{n}/merge") and name == "plain"
            q = quoted(spelled)
            url, port = (quoted(f"https://api.github.com{p}/{spelled.lstrip('/')}") for p in ("", ":443"))
            corpus += [(f"gh api -X PUT {q} -f merge_method=squash", False, False),
                       (f"gh api -X PUT {q} -f merge_method=merge -f sha={heads[n]}", False, canonical),
                       (f"gh api -X DELETE {q}", False, False), (f"gh api {q} -f x=1", False, False),
                       (f"curl -X PUT {url} -d '{{}}'", False, False), (f"curl -X PUT {port} -d '{{}}'", False, False)]
    mutation = 'mutation { mergePullRequest(input: {pullRequestId: "x"}) { clientMutationId } }'
    body = shlex.quote(json.dumps({"query": mutation}))
    # endpoints that climb to graphql: a run-time segment, or a contents or git/refs path (review round 2)
    climb = "/../../../../../../../graphql"
    for cmd in ("EP='repos/o/r/pulls/24/merge#'; gh api -X PUT \"$EP/../graphql\" -f merge_method=merge",
                "EP='repos/o/r/pulls/24/merge#'; gh api -X PUT \"$EP/../graphql\" -f merge_method=squash",
                "EP='repos/o/r/pulls/24/merge#'; gh api -X PUT \"x/$EP/../../graphql\" -f merge_method=merge",
                "gh api -X PUT \"$N/../graphql\" -f merge_method=merge", "gh api -X PUT \"$EP/../graphql\"",
                "gh api -X PUT \"$(cat ep.txt)/../graphql\" -f merge_method=merge",
                "gh api -X PUT 'repos/o/r/contents/a.txt/../../../../../graphql' -f message=m",
                "gh api -X PUT 'repos/o/r/contents/a\\..\\..\\..\\..\\..\\graphql' -f message=m",
                "gh api -X PUT 'repos/o/r/contents/a.txt/..;/..;/..;/..;/..;/graphql' -f message=m",
                f"gh api -X PATCH repos/o/r/git/refs/heads/x{climb} -f sha={heads[21]}",
                f"gh api -X DELETE repos/o/r/git/refs/heads/x{climb}",
                f"curl -X PATCH https://api.github.com/repos/o/r/git/refs/heads/x{climb} -d '{{}}'",
                # dot segments at the end (review round 2)
                "gh api -X PUT repos/o/r/contents/x/.. -f message=m", "gh api -X PUT repos/o/r/contents/.. -f message=m",
                "gh api -X DELETE repos/o/r/git/refs/../", "gh api -X PUT repos/o/r/git/refs/../",
                "gh api repos/o/r/git/refs/../ -f x=1", "curl -X DELETE https://api.github.com/repos/o/r/git/refs/../",
                # curl to a path ending in graphql, and curl host spellings
                f"curl -X POST https://api.github.com/repos/o/r/x/graphql -d {body}",
                f"curl https://api.github.com/repos/o/r/x/graphql -d {body}",
                "curl -X PUT https://u:p@api.github.com/repos/o/r/pulls/24/merge -d '{}'",
                "curl -X PUT https://api.github.com./repos/o/r/pulls/24/merge -d '{}'",
                "curl -X PUT HTTPS://api.github.com/repos/o/r/pulls/24/merge -d '{}'",
                "curl -X PUT api.github.com/repos/o/r/pulls/24/merge -d '{}'",
                "curl -X PUT //api.github.com/repos/o/r/pulls/24/merge -d '{}'",
                "curl -X PUT https://api.github.com:0443/repos/o/r/pulls/24/merge -d '{}'",
                "curl -X PUT https://x.example/api.github.com/repos/o/r/pulls/24/merge -d '{}'"):
        corpus.append((cmd, False, False))
    for g in ("graphql", "/graphql", "graphql/", "/graphql/", "GRAPHQL", "graphql#", "graphql?x=1",
              "graphql%3F", "graphql%2F", "graphql%23",
              "https://api.github.com/graphql", "https://api.github.com:443/graphql",
              "https://api.github.com:443/graphql/", "https://API.GITHUB.COM/graphql"):
        corpus.append((f"gh api {quoted(g)} -f query={shlex.quote(mutation)}", False, False))
    for u in ("https://api.github.com/graphql", "https://api.github.com/graphql/", "https://api.github.com:443/graphql",
              "https://api.github.com:443/graphql/", "api.github.com/graphql", "api.github.com:443/graphql"):
        corpus.append((f"curl -X POST {u} -d {body}", False, False))
    # the REST merge: its whole-line spellings may pass; field, method and line forms around it may not
    put = f"gh api -X PUT repos/o/r/pulls/21/merge -f merge_method=merge -f sha={heads[21]}"
    for cmd in (f"gh api --method PUT repos/o/r/pulls/21/merge -F merge_method=merge -F sha={heads[21]}",
                f"gh api -X PUT repos/o/r/pulls/22/merge -F merge_method=merge -f sha={heads[22]}",
                f"  {put}  ", put.replace(" ", "\t")):
        corpus.append((cmd, False, True))
    for cmd in (put + " -f merge_method=squash", put.replace("-f merge_method=merge", "-F merge_method=squash -f merge_method=merge"),
                put.replace("-f merge_method", "-f=merge_method"), put.replace("-f merge_method", "-Fmerge_method"),
                put.replace("merge_method=", "merge_method[]="), put + " -f commit_title=x", put + " -X PUT",
                put.replace("gh api", "gh api --"), put + " -H 'Accept: x'", put + " -p x", put + " --input body.json",
                put.replace("pulls/21", "pulls/22"), put.replace(heads[21], heads[22]),
                put.replace(f"sha={heads[21]}", "sha=@sha.txt"), "x=1 && " + put, "x=1; " + put,
                "echo =x && " + put, put + ";", "cd /tmp && " + put, put + " | cat"):
        corpus.append((cmd, False, False))
    for form in ("env {p}", "timeout 60 {p}", "GH_X=1 {p}", "bash -c {q}", "eval {q}", "x=$({p})", "( {p} )",
                 "{p} &", "{p} 2>&1", "{p} # c", "echo x | xargs {p}", "true; {p}"):
        corpus.append((form.format(p=put, q=shlex.quote(put)), False, False))
    for flags, graphql in (("24 --merge", True), ("21 --squash", True), ("21 --auto --merge", True),
                           ("21 --admin --merge", True), ("25 --merge", True), ("21 --merge", False),
                           ("24 --rebase", False)):
        corpus.append((f"gh pr merge {flags}", graphql, False))
    return corpus

if __name__ == "__main__":
    sys.exit(main())
