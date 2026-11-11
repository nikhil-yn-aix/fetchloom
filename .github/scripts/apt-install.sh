#!/usr/bin/env bash
# installs ubuntu packages with apt. when the runner's own resolver cannot find ubuntu's archive,
# as azure's could not on 2026-11-11, it asks public resolvers instead. apt verifies the archive's
# signed package lists and each package's hash, so a resolver cannot change what installs.
set -euo pipefail

attempt_limit=${CI_APT_ATTEMPT_TIMEOUT:-120s}
if [[ ! "$attempt_limit" =~ ^[1-9][0-9]*s$ ]]; then
  echo "::error::CI_APT_ATTEMPT_TIMEOUT must be a positive number of seconds ending in s"
  exit 2
fi

install() {
  local resolver=$1 result=0
  shift
  echo "::group::apt ($resolver): attempt limited to $attempt_limit"
  sudo timeout --verbose --signal=TERM --kill-after=10s "$attempt_limit" bash -c '
    set -euo pipefail
    resolver=$1
    shift
    echo "apt ($resolver): update package indexes"
    apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 update -qq
    echo "apt ($resolver): install $*"
    apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 install -y -qq "$@"
  ' bash "$resolver" "$@" || result=$?
  echo '::endgroup::'
  if [[ $result == 124 || $result == 137 ]]; then
    echo "::warning::apt ($resolver) timed out after $attempt_limit"
  elif (( result != 0 )); then
    echo "::warning::apt ($resolver) failed with exit code $result"
  fi
  return "$result"
}

if ! install runner "$@"; then
  echo "::warning::apt failed with the runner's resolver; trying public resolvers"
  printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' | sudo tee /etc/resolv.conf > /dev/null
  install public "$@"
fi
