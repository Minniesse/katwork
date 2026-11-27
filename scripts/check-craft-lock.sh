#!/bin/sh
# Checks docker/craft-binaries.lock, the Craft forks' pinned commits and build features (plan Task 13, R28).
#
#   scripts/check-craft-lock.sh [lock]                  check the lock (default docker/craft-binaries.lock)
#   scripts/check-craft-lock.sh --allow-missing [lock]  the same, but list the apps without a row instead of failing
#   scripts/check-craft-lock.sh --self-test             run the check on built-in bad and good rows
#
# A row is nine whitespace-separated columns:
#   app repo commit cli_package cli_bin features web_dir web_features thai_gate
# `#` starts a comment line. The check refuses a row with another column count, an unknown app, a repo other
# than https://github.com/Minniesse/<app>, a commit that is not 40 lower-case hex characters, a duplicate app,
# a `-` thai_gate for an app on REQUIRE_THAI_GATE, and in `features` or `web_features` (each comma-separated
# item compared whole) a KatWork gate, `default`, or `<dependency>/<gate>` (`?/` too), which would turn the
# gate back on through a dependency. It also refuses a lock without a row for each of the 13 apps
# (katwork-bridge's row is optional until Task 17 adds it); --allow-missing only lists those apps, for the
# lock's PR while the fork PRs merge.
set -u

APPS="wordcraft gridcraft pdfcraft deckcraft designcraft vectorcraft figcraft photocraft lightcraft cadcraft filmcraft effectcraft soundcraft katwork-bridge"
# The features each fork's KatWork build must leave off: the plan's six gates (plugin-hosts to model-download),
# then the extra gates the fork PRs add (founder decisions), then heif and rawnind-model, which the plan keeps off.
GATES="plugin-hosts js collab plugins network model-download font-download self-update scripting heic ssip heif rawnind-model"
# Apps whose Thai gate test must be pinned (Tasks 18-22 each add theirs).
REQUIRE_THAI_GATE=""

check() {
  awk -v apps="$APPS" -v gates="$GATES" -v thai="$REQUIRE_THAI_GATE" -v allow_missing="${2:-}" -v lock="$1" '
    BEGIN {
      n = split(apps, a, " "); for (i = 1; i <= n; i++) known[a[i]] = 1
      n = split(gates, g, " "); for (i = 1; i <= n; i++) gate[g[i]] = 1
      n = split(thai, t, " "); for (i = 1; i <= n; i++) needthai[t[i]] = 1
      bad = 0
    }
    function fail(msg) { printf "%s:%d: %s\n", FILENAME, FNR, msg; bad = 1 }
    function gated(list, col,   k, m, it, last) {
      if (list == "-") return
      m = split(list, it, ",")
      for (k = 1; k <= m; k++) {
        last = it[k]; sub(/^.*\//, "", last)
        if (it[k] == "") fail(col " has an empty item")
        else if (it[k] == "default") fail(col " names default, which turns every gate back on")
        else if (it[k] in gate) fail(col " names the KatWork gate " it[k])
        else if (last in gate) fail(col " turns the KatWork gate " last " on through " it[k])
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
    END {
      missing = ""
      for (i = 1; i <= n_apps(); i++) if (a[i] != "katwork-bridge" && !(a[i] in seen)) missing = missing " " a[i]
      if (missing != "") {
        if (allow_missing == "--allow-missing") printf "check-craft-lock: no row yet for:%s\n", missing
        else { printf "%s: no row for:%s\n", lock, missing; bad = 1 }
      }
      if (!bad) printf "check-craft-lock: ok (%d rows)\n", rows; exit bad
    }
    function n_apps(   c, k) { c = 0; for (k in a) c++; return c }
  ' "$1"
}

self_test() {
  dir=$(mktemp -d "${TMPDIR:-/tmp}/craft-lock.XXXXXX") || exit 1
  trap 'rm -rf "$dir"' EXIT
  c=0123456789abcdef0123456789abcdef01234567
  ok="soundcraft https://github.com/Minniesse/soundcraft $c soundcraft-cli soundcraft-cli - apps/soundcraft-web - -"
  failed=0
  # A good row for every required app the case's own rows do not name, so each case tests only its row.
  fill() {
    for a in $APPS; do
      [ "$a" = katwork-bridge ] && continue
      printf '%s\n' "$1" | awk -v a="$a" '$1 == a { f = 1 } END { exit !f }' && continue
      echo "$a https://github.com/Minniesse/$a $c $a-cli $a-cli - apps/$a-web - -"
    done
  }
  # case_ <want 0|1> <name> <rows>; FILL=0 leaves the other apps out, MODE=--allow-missing runs that mode.
  case_() {
    { echo '# lock'; [ "${FILL:-1}" = 1 ] && fill "$3"; printf '%s\n' "$3"; } > "$dir/lock"
    REQUIRE_THAI_GATE=${THAI:-} check "$dir/lock" "${MODE:-}" > "$dir/out" 2>&1; got=$?
    if [ "$got" != "$1" ]; then
      echo "self-test FAIL: $2 (want $1, got $got)"; sed 's/^/  /' "$dir/out"; failed=1
    fi
  }
  feat() { echo "$ok" | sed "s| soundcraft-cli - | soundcraft-cli $1 |"; }
  case_ 0 "a good row" "$ok"
  case_ 0 "a good row with a trailing tab-separated layout" "$(printf '%s' "$ok" | tr ' ' '\t')"
  case_ 0 "a feature that only starts like a gate" "$(feat networking)"
  case_ 0 "a dependency feature that only ends like a gate" "$(feat soundcraft-engine/networking)"
  case_ 0 "pdfcraft keeps mcp" "pdfcraft https://github.com/Minniesse/pdfcraft $c pdfcraft-cli pdfcraft-cli mcp apps/pdfcraft-web - -"
  case_ 0 "the bridge row" "katwork-bridge https://github.com/Minniesse/katwork-bridge $c - - - - - -"
  case_ 1 "eight columns" "$(echo "$ok" | sed 's/ -$//')"
  case_ 1 "ten columns" "$ok extra"
  case_ 1 "a 39-character commit" "$(echo "$ok" | sed "s/$c/$(echo $c | cut -c1-39)/")"
  case_ 1 "an upper-case commit" "$(echo "$ok" | sed "s/$c/$(echo $c | tr a-f A-F)/")"
  case_ 1 "web_features naming plugins" "$(echo "$ok" | sed 's|apps/soundcraft-web - -|apps/soundcraft-web plugins -|')"
  case_ 1 "a gate among several features" "$(feat denoise,network)"
  # The plan's six gates, named here so that dropping one from GATES fails the self-test.
  for g in plugin-hosts js collab plugins network model-download; do
    case "$GATES " in *" $g "*|"$g "*) ;; *) echo "self-test FAIL: GATES lacks the plan's gate $g"; failed=1 ;; esac
  done
  case_ 1 "features naming plugin-hosts" "$(feat plugin-hosts)"
  case_ 1 "features naming js" "$(feat js)"
  case_ 1 "features naming collab" "$(feat collab)"
  case_ 1 "features naming plugins" "$(feat plugins)"
  case_ 1 "features naming network" "$(feat network)"
  case_ 1 "features naming model-download" "$(feat model-download)"
  # The fork PRs' extra gates and the features the KatWork build leaves off.
  for g in $GATES; do
    case_ 1 "features naming $g" "$(feat "$g")"
  done
  case_ 1 "features naming heif" "$(feat heif)"
  case_ 1 "features naming rawnind-model" "$(feat rawnind-model)"
  case_ 1 "features naming ssip" "$(feat ssip)"
  case_ 1 "features naming default" "$(feat default)"
  case_ 1 "web_features naming default" "$(echo "$ok" | sed 's|apps/soundcraft-web - -|apps/soundcraft-web default -|')"
  case_ 1 "a gate through a dependency" "$(feat soundcraft-engine/plugin-hosts)"
  case_ 1 "a gate through an optional dependency" "$(feat soundcraft-engine?/plugin-hosts)"
  case_ 1 "web_features naming a gate through a dependency" "$(echo "$ok" | sed 's|apps/soundcraft-web - -|apps/soundcraft-web soundcraft-engine/plugins -|')"
  case_ 1 "an unknown app" "$(echo "$ok" | sed 's/^soundcraft /noisecraft /')"
  case_ 1 "the upstream repo" "$(echo "$ok" | sed 's|Minniesse/soundcraft|storytold/soundcraft|')"
  case_ 1 "a duplicate app" "$(printf '%s\n%s' "$ok" "$ok")"
  case_ 1 "an empty feature item" "$(feat denoise,,)"
  THAI=soundcraft case_ 1 "a - thai_gate for an app on the test list" "$ok"
  THAI=soundcraft case_ 0 "a thai_gate for an app on the test list" "$(echo "$ok" | sed 's/ -$/ soundcraft-text::thai_gate/')"
  FILL=0 case_ 1 "a lock missing twelve apps" "$ok"
  FILL=0 case_ 1 "the empty lock" ""
  FILL=0 MODE=--allow-missing case_ 0 "the empty lock, missing rows allowed" ""
  FILL=0 MODE=--allow-missing case_ 1 "a gate, missing rows allowed" "$(feat plugins)"
  FILL=0 MODE=--allow-missing case_ 1 "eight columns, missing rows allowed" "$(echo "$ok" | sed 's/ -$//')"
  if [ "$failed" = 0 ]; then echo "check-craft-lock self-test: ok"; fi
  return "$failed"
}

case "${1:-}" in
  --self-test) self_test ;;
  --allow-missing) check "${2:-$(dirname "$0")/../docker/craft-binaries.lock}" --allow-missing ;;
  -*) echo "usage: $0 [--self-test | [--allow-missing] lock]" >&2; exit 2 ;;
  *) check "${1:-$(dirname "$0")/../docker/craft-binaries.lock}" ;;
esac
