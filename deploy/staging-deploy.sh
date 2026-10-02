#!/bin/sh
set -eu

commit=${1:-}
expected_hash=${2:-}
case "$commit" in
  *[!0-9a-f]*|'') echo 'invalid commit' >&2; exit 2 ;;
esac
case "$expected_hash" in
  *[!0-9a-f]*|'') echo 'invalid checksum' >&2; exit 2 ;;
esac
[ "${#commit}" -eq 40 ] || { echo 'invalid commit length' >&2; exit 2; }
[ "${#expected_hash}" -eq 64 ] || { echo 'invalid checksum length' >&2; exit 2; }

incoming=/home/vorp-deploy/incoming/vorp
binary=/opt/vorp/vorp
previous=/opt/vorp/vorp.previous
next=/opt/vorp/vorp.next
lock=/run/lock/vorp-deploy.lock

exec 9>"$lock"
flock -n 9 || { echo 'another deployment is running' >&2; exit 1; }
[ -f "$incoming" ] || { echo 'missing uploaded binary' >&2; exit 1; }
actual_hash=$(sha256sum "$incoming" | cut -d ' ' -f 1)
[ "$actual_hash" = "$expected_hash" ] || { echo 'binary checksum mismatch' >&2; exit 1; }

install -m 0755 -o root -g root "$incoming" "$next"
cp -p "$binary" "$previous"
mv -f "$next" "$binary"
if ! systemctl restart vorp; then
  cp -p "$previous" "$binary"
  systemctl restart vorp || true
  echo 'restart failed; previous binary restored' >&2
  exit 1
fi

attempt=0
while [ "$attempt" -lt 15 ]; do
  if curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
    --resolve vorp-staging.themidst.xyz:443:127.0.0.1 \
    https://vorp-staging.themidst.xyz/healthz >/dev/null 2>&1; then
    printf 'deployed commit %s (sha256 %s)\n' "$commit" "$expected_hash"
    exit 0
  fi
  attempt=$((attempt + 1))
  sleep 2
done

cp -p "$previous" "$binary"
systemctl restart vorp || true
echo 'health check failed; previous binary restored' >&2
exit 1
