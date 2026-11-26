#!/bin/sh
# Checks docker/craft-binaries.lock, the Craft forks' pinned commits and build features (plan Task 13, R28).
#
#   scripts/check-craft-lock.sh [lock]     check the lock (default docker/craft-binaries.lock)
#   scripts/check-craft-lock.sh --self-test run the check on built-in bad and good rows
#
# A row is nine whitespace-separated columns:
#   app repo commit cli_package cli_bin features web_dir web_features thai_gate
# `#` starts a comment line. The check refuses a row with another column count, an unknown app, a repo other
# than https://github.com/Minniesse/<app>, a commit that is not 40 lower-case hex characters, a duplicate app,
# a KatWork gate in `features` or `web_features` (each comma-separated item is compared whole), and a `-`
# thai_gate for an app on REQUIRE_THAI_GATE.
set -u

APPS="wordcraft gridcraft pdfcraft deckcraft designcraft vectorcraft figcraft photocraft lightcraft cadcraft filmcraft effectcraft soundcraft katwork-bridge"
# The features each fork's KatWork build must leave off (Task 13; all after model-download are the fork PRs' extra gates, R44 and the founder rulings).
GATES="plugin-hosts js collab plugins network model-download font-download self-update scripting heic"
# Apps whose Thai gate test must be pinned (Tasks 18-22 each add theirs).
REQUIRE_THAI_GATE=""

check() {
  awk -v apps="$APPS" -v gates="$GATES" -v thai="$REQUIRE_THAI_GATE" '
    BEGIN {
      n = split(apps, a, " "); for (i = 1; i <= n; i++) known[a[i]] = 1
      n = split(gates, g, " "); for (i = 1; i <= n; i++) gate[g[i]] = 1
      n = split(thai, t, " "); for (i = 1; i <= n; i++) needthai[t[i]] = 1
      bad = 0
    }
    function fail(msg) { printf "%s:%d: %s\n", FILENAME, FNR, msg; bad = 1 }
    function gated(list, col,   k, m, it) {
      if (list == "-") return
      m = split(list, it, ",")
      for (k = 1; k <= m; k++) {
        if (it[k] == "") fail(col " has an empty item")
        else if (it[k] in gate) fail(col " names the KatWork gate " it[k])
      }
    }
    /^[ \t]*(#|$)/ { next }
    {
      if (NF != 9) { fail("expected 9 columns, found " NF); next }
      app = $1
      if (!(app in known)) fail("unknown app " app)
      if ($2 != "https://github.com/Minniesse/" app) fail("repo must be https://github.com/Minniesse/" app)
      if ($3 !~ /^[0-9a-f]+$/ || length($3) != 40) fail("commit must be 40 lower-case hex characters")
      if (app in seen) fail("duplicate row for " app)
      seen[app] = 1
      gated($6, "features"); gated($8, "web_features")
      if ((app in needthai) && $9 == "-") fail(app " needs its thai_gate")
      rows++
    }
    END { if (!bad) printf "check-craft-lock: ok (%d rows)\n", rows; exit bad }
  ' "$1"
}

self_test() {
  dir=$(mktemp -d "${TMPDIR:-/tmp}/craft-lock.XXXXXX") || exit 1
  trap 'rm -rf "$dir"' EXIT
  c=0123456789abcdef0123456789abcdef01234567
  ok="soundcraft https://github.com/Minniesse/soundcraft $c soundcraft-cli soundcraft-cli - apps/soundcraft-web - -"
  failed=0
  # case <want 0|1> <name> <row>
  case_() {
    printf '# lock\n%s\n' "$3" > "$dir/lock"
    REQUIRE_THAI_GATE=${THAI:-} check "$dir/lock" > "$dir/out" 2>&1; got=$?
    if [ "$got" != "$1" ]; then
      echo "self-test FAIL: $2 (want $1, got $got)"; sed 's/^/  /' "$dir/out"; failed=1
    fi
  }
  case_ 0 "a good row" "$ok"
  case_ 0 "a good row with a trailing tab-separated layout" "$(printf '%s' "$ok" | tr ' ' '\t')"
  case_ 0 "a feature that only starts like a gate" "$(echo "$ok" | sed 's/ soundcraft-cli - / soundcraft-cli networking /')"
  case_ 0 "pdfcraft keeps mcp" "pdfcraft https://github.com/Minniesse/pdfcraft $c pdfcraft-cli pdfcraft-cli mcp apps/pdfcraft-web - -"
  case_ 0 "the bridge row" "katwork-bridge https://github.com/Minniesse/katwork-bridge $c - - - - - -"
  case_ 1 "eight columns" "$(echo "$ok" | sed 's/ -$//')"
  case_ 1 "ten columns" "$ok extra"
  case_ 1 "a 39-character commit" "$(echo "$ok" | sed "s/$c/$(echo $c | cut -c1-39)/")"
  case_ 1 "an upper-case commit" "$(echo "$ok" | sed "s/$c/$(echo $c | tr a-f A-F)/")"
  case_ 1 "web_features naming plugins" "$(echo "$ok" | sed 's|apps/soundcraft-web - -|apps/soundcraft-web plugins -|')"
  case_ 1 "a gate among several features" "$(echo "$ok" | sed 's/ soundcraft-cli - / soundcraft-cli denoise,network /')"
  for g in $GATES; do
    case_ 1 "features naming $g" "$(echo "$ok" | sed "s/ soundcraft-cli - / soundcraft-cli $g /")"
  done
  case_ 1 "an unknown app" "$(echo "$ok" | sed 's/^soundcraft /noisecraft /')"
  case_ 1 "the upstream repo" "$(echo "$ok" | sed 's|Minniesse/soundcraft|storytold/soundcraft|')"
  case_ 1 "a duplicate app" "$(printf '%s\n%s' "$ok" "$ok")"
  case_ 1 "an empty feature item" "$(echo "$ok" | sed 's/ soundcraft-cli - / soundcraft-cli denoise,, /')"
  THAI=soundcraft case_ 1 "a - thai_gate for an app on the test list" "$ok"
  THAI=soundcraft case_ 0 "a thai_gate for an app on the test list" "$(echo "$ok" | sed 's/ -$/ soundcraft-text::thai_gate/')"
  if [ "$failed" = 0 ]; then echo "check-craft-lock self-test: ok"; fi
  return "$failed"
}

case "${1:-}" in
  --self-test) self_test ;;
  -*) echo "usage: $0 [--self-test | lock]" >&2; exit 2 ;;
  *) check "${1:-$(dirname "$0")/../docker/craft-binaries.lock}" ;;
esac
