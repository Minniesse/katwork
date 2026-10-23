#!/usr/bin/env bash
# SessionStart hook, shared by Claude Code and Codex.
#
# Claude Code: appends to $CLAUDE_ENV_FILE, which runs before every Bash
# command, so later commands find cargo and pick up DATABASE_URL from this
# worktree's private test cluster (scripts/test-db.sh) whenever it is running.
#
# Codex has no env file. When cargo is missing from PATH, this prints one line
# that Codex adds to the model's context instead.
#
# KATWORK_RUST_BIN overrides the directory searched for cargo. The defaults are
# Homebrew's keg-only rustup and rustup's own install location.
set -u

root=$(cd "$(dirname "$0")/../.." && pwd -P)

rust_bin=""
if [ -n "${KATWORK_RUST_BIN:-}" ] || ! command -v cargo >/dev/null 2>&1; then
  for dir in "${KATWORK_RUST_BIN:-}" /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin "$HOME/.cargo/bin"; do
    if [ -n "$dir" ] && [ -x "$dir/cargo" ]; then
      rust_bin=$dir
      break
    fi
  done
fi

if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
  # The DATABASE_URL line runs before each command and follows the command's
  # own worktree, so a review in a scratch worktree of this repo gets that
  # worktree's cluster. Other repositories are left alone.
  common=$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)
  {
    if [ -n "$rust_bin" ]; then
      printf 'export PATH=%q:"$PATH"\n' "$rust_bin"
    fi
    if [ -n "$common" ]; then
      printf '%s\n' 'if _katwork_top=$(git rev-parse --show-toplevel 2>/dev/null) \' \
        "  && [ \"\$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)\" = $(printf %q "$common") ] \\" \
        '  && _katwork_db=$("$_katwork_top/scripts/test-db.sh" url 2>/dev/null); then' \
        '  export DATABASE_URL="$_katwork_db"' \
        'fi' \
        'unset _katwork_top _katwork_db'
    fi
  } >>"$CLAUDE_ENV_FILE"
elif [ -n "$rust_bin" ]; then
  echo "cargo is not on PATH in this session. It is in $rust_bin: run Rust commands as PATH=$rust_bin:\$PATH cargo ... (setup: docs/agents/environment.md)."
fi
exit 0
