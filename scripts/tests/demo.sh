#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
mkdir "$tmp/scripts" "$tmp/bin"
cp "$root/scripts/demo.sh" "$root/scripts/init-env.sh" "$tmp/scripts/"
cp "$root/.env.example" "$tmp/"
export PATH="$tmp/bin:$PATH"
export DEMO_TEST_DIR="$tmp"
export DEMO_TEST_PASSWORD
DEMO_TEST_PASSWORD=$(openssl rand -hex 16)
cat > "$tmp/bin/docker" <<'SCRIPT'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$DEMO_TEST_DIR/commands"
case "$*" in
    *'ps --status running -q')
        [ "${PRODUCTION_RUNNING:-0}" = 0 ] || printf '%s\n' production-container
        exit 0 ;;
esac
case "$*" in *'--project-name katwork-demo --env-file '*'/.env.demo -f docker-compose.yml -f docker-compose.demo.yml '*) ;; *) exit 1 ;; esac
[ -z "${DATABASE_URL:-}" ]
case "$*" in
    *'exec -T katwork /app/katwork-server seed-demo')
        [ ! -f .seeded ] || exit 0
        printf '%s\n' "$DEMO_TEST_PASSWORD"
        touch .seeded ;;
    *'up -d') ;;
    *) exit 1 ;;
esac
SCRIPT
cat > "$tmp/bin/curl" <<'SCRIPT'
#!/bin/sh
exit 0
SCRIPT
chmod +x "$tmp/bin/docker" "$tmp/bin/curl"
KATWORK_ENV_FILE="$tmp/elsewhere" "$tmp/scripts/demo.sh" > "$tmp/out" 2>&1
[ -f "$tmp/.env.demo" ]
[ ! -f "$tmp/.env" ]
[ "$(cat "$tmp/.demo-credentials")" = "$DEMO_TEST_PASSWORD" ]
mode=$(stat -c %a "$tmp/.demo-credentials" 2>/dev/null || stat -f %Lp "$tmp/.demo-credentials")
[ "$mode" = 600 ]
if grep -qF "$DEMO_TEST_PASSWORD" "$tmp/out"; then exit 1; fi
printf '%s\n' '# Existing production settings must remain untouched.' > "$tmp/.env"
cp "$tmp/.env" "$tmp/production-settings"
"$tmp/scripts/demo.sh" > "$tmp/out" 2>&1
[ -f "$tmp/.env.demo" ]
cmp "$tmp/.env" "$tmp/production-settings"
[ "$(cat "$tmp/.demo-credentials")" = "$DEMO_TEST_PASSWORD" ]
before=$(wc -l < "$tmp/commands")
if PRODUCTION_RUNNING=1 "$tmp/scripts/demo.sh" > "$tmp/out" 2>&1; then exit 1; fi
[ "$(wc -l < "$tmp/commands")" -eq "$((before + 1))" ]
grep -q -- '--allow-running-install' "$tmp/out"
DATABASE_URL=must-not-reach-demo PRODUCTION_RUNNING=1 "$tmp/scripts/demo.sh" --allow-running-install > "$tmp/out" 2>&1
if grep -q -- 'http://localhost:3000' "$tmp/out"; then exit 1; fi
grep -q -- 'http://localhost:3001' "$tmp/out"
printf '%s\n' 'demo tests passed'
