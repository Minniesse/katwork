#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
export KATWORK_ENV_FILE="$tmp/.env"
"$root/scripts/init-env.sh" > "$tmp/out"
[ "$(cat "$tmp/out")" = "$KATWORK_ENV_FILE" ]
mode=$(stat -c %a "$KATWORK_ENV_FILE" 2>/dev/null || stat -f %Lp "$KATWORK_ENV_FILE")
[ "$mode" = 600 ]
secret=$(sed -n 's/^KATWORK_SECRET=//p' "$KATWORK_ENV_FILE")
password=$(sed -n 's/^POSTGRES_PASSWORD=//p' "$KATWORK_ENV_FILE")
[ "${#secret}" -ge 32 ]
[ "${#password}" -eq 48 ]
[ "$(sed -n 's/^DATABASE_URL=//p' "$KATWORK_ENV_FILE")" = "postgres://katwork:$password@postgres:5432/katwork" ]
cp "$KATWORK_ENV_FILE" "$tmp/before"
if "$root/scripts/init-env.sh" > "$tmp/out" 2>&1; then exit 1; fi
cmp "$KATWORK_ENV_FILE" "$tmp/before"
if grep -qF "$secret" "$tmp/out"; then exit 1; fi
"$root/scripts/init-env.sh" --force --https smoke.example > "$tmp/out"
grep -qx 'KATWORK_HTTPS_HOST=smoke.example' "$KATWORK_ENV_FILE"
grep -qx 'COMPOSE_FILE=docker-compose.yml:docker-compose.https.yml' "$KATWORK_ENV_FILE"
[ "$secret" != "$(sed -n 's/^KATWORK_SECRET=//p' "$KATWORK_ENV_FILE")" ]
for host in 'https://example.test' 'bad/host' 'bad host' 'bad$host' '-bad.test' 'bad..test' "$(printf 'valid\nbad')"; do
    if KATWORK_ENV_FILE="$tmp/invalid" "$root/scripts/init-env.sh" --https "$host" > "$tmp/out" 2>&1; then exit 1; fi
    [ ! -e "$tmp/invalid" ]
done
rm "$KATWORK_ENV_FILE"
ln -s "$tmp/missing" "$KATWORK_ENV_FILE"
if "$root/scripts/init-env.sh" > "$tmp/out" 2>&1; then exit 1; fi
[ ! -e "$tmp/missing" ]
printf '%s\n' 'init-env tests passed'
