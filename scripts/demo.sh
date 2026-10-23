#!/bin/sh
set -eu
umask 077
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
if [ ! -e .env ]; then KATWORK_ENV_FILE="$root/.env" scripts/init-env.sh; fi
compose() { docker compose -f docker-compose.yml -f docker-compose.demo.yml "$@"; }
compose up -d
attempt=0
until curl --fail --silent --output /dev/null --connect-timeout 1 --max-time 2 http://localhost:3000/healthz; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 120 ] || { printf '%s\n' 'KatWork did not start; check docker compose logs.' >&2; exit 1; }
    sleep 1
done
credentials=$(mktemp "$root/.demo-credentials.tmp.XXXXXX")
trap 'rm -f "$credentials"' EXIT HUP INT TERM
compose exec -T katwork /app/katwork-server seed-demo > "$credentials"
if [ -s "$credentials" ]; then mv -f "$credentials" "$root/.demo-credentials"; fi
printf '%s\n' 'http://localhost:3000' 'Account: demo@katwork.local'
if [ -f "$root/.demo-credentials" ]; then
    printf '%s\n' 'Open .demo-credentials on this host to read the initial password.'
else
    printf '%s\n' 'Use your KATWORK_DEMO_PASSWORD, or the password saved on your first run.'
fi
