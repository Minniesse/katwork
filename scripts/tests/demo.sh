#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
mkdir "$tmp/scripts" "$tmp/bin"
cp "$root/scripts/demo.sh" "$root/scripts/init-env.sh" "$tmp/scripts/"
cp "$root/.env.example" "$tmp/"
export PATH="$tmp/bin:$PATH"
export DEMO_TEST_PASSWORD
DEMO_TEST_PASSWORD=$(openssl rand -hex 16)
cat > "$tmp/bin/docker" <<'SCRIPT'
#!/bin/sh
set -eu
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
[ -f "$tmp/.env" ]
[ "$(cat "$tmp/.demo-credentials")" = "$DEMO_TEST_PASSWORD" ]
mode=$(stat -c %a "$tmp/.demo-credentials" 2>/dev/null || stat -f %Lp "$tmp/.demo-credentials")
[ "$mode" = 600 ]
if grep -qF "$DEMO_TEST_PASSWORD" "$tmp/out"; then exit 1; fi
"$tmp/scripts/demo.sh" > "$tmp/out" 2>&1
[ "$(cat "$tmp/.demo-credentials")" = "$DEMO_TEST_PASSWORD" ]
printf '%s\n' 'demo tests passed'
