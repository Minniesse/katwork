#!/usr/bin/env bash
# Private, throwaway PostgreSQL 17 cluster for this worktree's tests.
#
#   scripts/test-db.sh start   start the cluster (or reuse a running one) and print its DATABASE_URL
#   scripts/test-db.sh url     print the DATABASE_URL of the running cluster; exit 1 if none
#   scripts/test-db.sh stop    stop the cluster and delete its directory
#
# Each worktree gets its own cluster, port and data directory, so parallel
# workers never share a server and `#[sqlx::test]` database names can't collide.
# The cluster listens on 127.0.0.1 only, trusts local connections (as CI does),
# and runs with fsync off: it is for tests, never for data you want to keep.
#
# Environment overrides:
#   KATWORK_PG_BIN        directory holding initdb, pg_ctl and createdb
#                         (default: the first PostgreSQL 17 install found, then PATH)
#   KATWORK_TEST_DB_ROOT  parent directory for cluster directories
#                         (default: katwork-test-db in the user temp dir, $TMPDIR)
#   KATWORK_TEST_DB_PORT  first port to try (default: derived from the worktree path)
set -euo pipefail

die() { echo "test-db: $*" >&2; exit 1; }

repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
# On macOS, ask for the per-user temp dir directly so hooks, sandboxes and
# shells with a different or missing $TMPDIR still find the same cluster.
tmp_base=$(getconf DARWIN_USER_TEMP_DIR 2>/dev/null || true)
tmp_base=${tmp_base:-${TMPDIR:-/tmp}}
base_dir=${KATWORK_TEST_DB_ROOT:-${tmp_base%/}/katwork-test-db}
worktree_hash=$(printf '%s' "$repo_root" | cksum | awk '{print $1}')
cluster_dir="$base_dir/$(basename "$repo_root")-$worktree_hash"
data_dir="$cluster_dir/data"
url_file="$cluster_dir/url"

find_pg_bin() {
  if [ -n "${KATWORK_PG_BIN:-}" ]; then
    [ -x "$KATWORK_PG_BIN/pg_ctl" ] || die "KATWORK_PG_BIN=$KATWORK_PG_BIN has no pg_ctl"
    echo "$KATWORK_PG_BIN"
    return
  fi
  local dir
  for dir in /opt/homebrew/opt/postgresql@17/bin /usr/local/opt/postgresql@17/bin /usr/lib/postgresql/17/bin; do
    if [ -x "$dir/pg_ctl" ]; then echo "$dir"; return; fi
  done
  if command -v pg_ctl >/dev/null 2>&1; then
    dirname "$(command -v pg_ctl)"
    return
  fi
  die "PostgreSQL 17 not found; set KATWORK_PG_BIN to the directory holding initdb and pg_ctl"
}

is_running() {
  local pid
  [ -f "$url_file" ] && [ -f "$data_dir/postmaster.pid" ] || return 1
  pid=$(head -n 1 "$data_dir/postmaster.pid")
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}

cmd_url() {
  is_running || die "no test database for this worktree; run scripts/test-db.sh start"
  cat "$url_file"
}

cmd_start() {
  if is_running; then cat "$url_file"; return; fi

  local pg_bin version port attempt
  pg_bin=$(find_pg_bin)
  version=$("$pg_bin/pg_ctl" --version | awk '{for (i = 1; i <= NF; i++) if ($i ~ /^[0-9]/) { print $i; exit }}')
  case "$version" in
    17|17.*) ;;
    *) echo "test-db: warning: using PostgreSQL $version from $pg_bin; CI uses 17" >&2 ;;
  esac

  cmd_stop # clears a half-started or stale cluster
  mkdir -p "$cluster_dir"
  "$pg_bin/initdb" -D "$data_dir" -U katwork --auth=trust --encoding=UTF8 --locale=C \
    --locale-provider=builtin --builtin-locale=C.UTF-8 --no-instructions \
    >"$cluster_dir/initdb.log" 2>&1 || die "initdb failed; see $cluster_dir/initdb.log"
  cat >>"$data_dir/postgresql.conf" <<'CONF'
# scripts/test-db.sh: throwaway test cluster
listen_addresses = '127.0.0.1'
unix_socket_directories = ''
fsync = off
synchronous_commit = off
full_page_writes = off
CONF

  port=${KATWORK_TEST_DB_PORT:-$((20000 + worktree_hash % 20000))}
  for attempt in 1 2 3 4 5 6 7 8 9 10; do
    if "$pg_bin/pg_ctl" -D "$data_dir" -l "$cluster_dir/postgres.log" -o "-p $port" -w -t 30 start \
      >/dev/null 2>&1; then
      if ! "$pg_bin/createdb" -h 127.0.0.1 -p "$port" -U katwork katwork; then
        "$pg_bin/pg_ctl" -D "$data_dir" -m immediate stop >/dev/null 2>&1 || true
        die "createdb failed; see $cluster_dir/postgres.log"
      fi
      echo "postgres://katwork@127.0.0.1:$port/katwork" >"$url_file"
      cat "$url_file"
      return
    fi
    grep -q -i -E 'could not bind|address already in use' "$cluster_dir/postgres.log" 2>/dev/null \
      || die "postgres failed to start; see $cluster_dir/postgres.log"
    : >"$cluster_dir/postgres.log"
    port=$((port + 1))
  done
  die "no free port after $attempt attempts; set KATWORK_TEST_DB_PORT"
}

cmd_stop() {
  local pg_ctl
  if [ -f "$data_dir/postmaster.pid" ]; then
    pg_ctl="$(find_pg_bin)/pg_ctl"
    if "$pg_ctl" -D "$data_dir" status >/dev/null 2>&1; then
      "$pg_ctl" -D "$data_dir" -m fast -w stop >/dev/null 2>&1 \
        || die "pg_ctl stop failed; the cluster is in $cluster_dir"
    fi
  fi
  remove_cluster_dir
}

remove_cluster_dir() {
  case "$cluster_dir" in
    "$base_dir"/?*) rm -rf "$cluster_dir" ;;
    *) die "refusing to remove $cluster_dir" ;;
  esac
}

# Serialize start and stop within one worktree (parallel subagents, say).
lock_dir="$cluster_dir.lock"
with_lock() {
  local tries=0 holder
  mkdir -p "$base_dir"
  until mkdir "$lock_dir" 2>/dev/null; do
    holder=$(cat "$lock_dir/pid" 2>/dev/null || true)
    if [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; then
      rm -rf "$lock_dir" # its holder died
      continue
    fi
    tries=$((tries + 1))
    [ "$tries" -lt 120 ] || die "timed out waiting for $lock_dir; remove it if no start or stop is running"
    sleep 0.5
  done
  echo $$ >"$lock_dir/pid"
  trap 'rm -rf "$lock_dir"' EXIT
  "$@"
}

case "${1:-}" in
  start) with_lock cmd_start ;;
  url) cmd_url ;;
  stop) with_lock cmd_stop ;;
  -h|--help|help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0" ;;
  *) echo "usage: scripts/test-db.sh start|url|stop|--help" >&2; exit 2 ;;
esac
