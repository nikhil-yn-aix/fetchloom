#!/usr/bin/env bash
set -euo pipefail

binary="$1"
work="$2"
mode="$3"
subject=https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz

if [ "$mode" = "prepare" ]; then
  mkdir -p "$work"
  find "$work" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  export FETCHLOOM_CACHE_DIR="$work/cache"
  if ! "$binary" get "$subject" --output "$work/here" --lock "$work/fetchloom.lock" \
      --json > "$work/fetched.json" 2> "$work/fetched.err"; then
    if grep -q '"network\.\|policy.offline' "$work/fetched.err" "$work/fetched.json"; then
      echo "skip: $subject could not be reached"
      touch "$work/skipped"
      exit 0
    fi
    cat "$work/fetched.err"
    exit 1
  fi
  "$binary" plan "$subject" --output "$work/there" --lock "$work/fetchloom.lock" \
    > "$work/dataset.plan"
  "$binary" cache export "$work/bundle.tar"
  echo "prepared"
  exit 0
fi

if [ -f "$work/skipped" ]; then
  echo "skip: nothing was prepared"
  exit 0
fi

# The apply half runs as root inside a network namespace, so anything it writes
# would otherwise be undeletable by the next prepare.
trap 'chown -R "$(stat -c %u:%g "$work")" "$work"' EXIT

if getent hosts ftp.gnu.org > /dev/null 2>&1; then
  echo "the offline lane still resolves names, so it is not offline"
  exit 1
fi

export FETCHLOOM_CACHE_DIR="$work/offline-cache"
rm -rf "$FETCHLOOM_CACHE_DIR" "$work/there"
"$binary" cache import "$work/bundle.tar"
"$binary" apply "$work/dataset.plan" --output "$work/there" --offline --json \
  > "$work/applied.json"

expected=$(sed -n 's/.*"tree":"\([^"]*\)".*/\1/p' "$work/fetched.json")
produced=$(sed -n 's/.*"tree":"\([^"]*\)".*/\1/p' "$work/applied.json")
if [ -z "$expected" ] || [ "$expected" != "$produced" ]; then
  echo "the offline apply produced $produced where the connected run reported $expected"
  exit 1
fi
requests=$(sed -n 's/.*"requests":\([0-9]*\).*/\1/p' "$work/applied.json")
if [ "$requests" != "0" ]; then
  echo "the offline apply issued $requests requests"
  exit 1
fi
echo "offline: $produced"
