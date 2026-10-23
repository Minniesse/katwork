#!/bin/sh
set -eu
umask 077
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
case "${1:-}" in
    '')
        running=$(DATABASE_URL=unused KATWORK_SECRET=unused POSTGRES_PASSWORD=unused docker compose -f docker-compose.yml ps --status running -q)
        if [ -n "$running" ]; then
            printf '%s\n' 'An install is running. Stop it first, or pass --allow-running-install to run a separate demo on port 3001.' >&2
            exit 1
        fi ;;
    --allow-running-install) shift ;;
    *) printf '%s\n' 'Usage: scripts/demo.sh [--allow-running-install]' >&2; exit 1 ;;
esac
[ "$#" -eq 0 ] || exit 1
unset DATABASE_URL POSTGRES_PASSWORD KATWORK_SECRET COMPOSE_PROFILES
if [ ! -e .env.demo ]; then KATWORK_ENV_FILE="$root/.env.demo" scripts/init-env.sh; fi
compose() { docker compose --project-name katwork-demo --env-file "$root/.env.demo" -f docker-compose.yml -f docker-compose.demo.yml "$@"; }
compose up -d
attempt=0
until curl --fail --silent --output /dev/null --connect-timeout 1 --max-time 2 http://localhost:3001/healthz; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 120 ] || { printf '%s\n' 'KatWork did not start; check docker compose logs.' >&2; exit 1; }
    sleep 1
done
credentials=$(mktemp "$root/.demo-credentials.tmp.XXXXXX")
trap 'rm -f "$credentials"' EXIT HUP INT TERM
compose exec -T katwork /app/katwork-server seed-demo > "$credentials"
if [ -s "$credentials" ]; then mv -f "$credentials" "$root/.demo-credentials"; fi
printf '%s\n' 'http://localhost:3001' 'Account: demo@katwork.local'
if [ -f "$root/.demo-credentials" ]; then
    printf '%s\n' 'Use KATWORK_DEMO_PASSWORD when set; otherwise open .demo-credentials for the initial password.'
else
    printf '%s\n' 'Use your KATWORK_DEMO_PASSWORD, or the password saved on your first run.'
fi
