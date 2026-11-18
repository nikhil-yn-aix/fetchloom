#!/usr/bin/env bash
# installs ubuntu packages with apt. when the runner's own resolver cannot find ubuntu's archive,
# as azure's could not on 2026-11-11, or azure's mirror serves its packages at a crawl, it asks
# public resolvers and the mirrors after azure's instead. apt verifies the archive's signed package
# lists and each package's hash, so neither a resolver nor a mirror can change what installs. a
# lock of apt's or dpkg's that another process holds, as the dpkg an attempt cut short leaves
# running does, is waited for within the attempt rather than failed on, and the wait is told.
set -euo pipefail

# each attempt's bound, which covers its waits for locks as well as its work
attempt_limit=${CI_APT_ATTEMPT_TIMEOUT:-240s}
if [[ ! "$attempt_limit" =~ ^[1-9][0-9]*s$ ]]; then
  echo "::error::CI_APT_ATTEMPT_TIMEOUT must be a positive number of seconds ending in s"
  exit 2
fi
# the mirrors apt takes the archive from, in order, as the runner image lists them
mirrors=${CI_APT_MIRRORS:-/etc/apt/apt-mirrors.txt}

# waits while another process holds one of the lock files named, for as many seconds as `left`
# has, which it counts down, and says whose lock it waited for and how long. apt-get update never
# waits for its own lock, and a wait apt makes for dpkg's is silent at -qq.
wait_for() {
  local holder first="" waited=0
  while (( left > 0 )); do
    holder=$(lslocks --noheadings --raw --output PID,COMMAND,PATH 2> /dev/null |
      awk -v wanted="$*" '
        BEGIN { split(wanted, files, " "); for (i in files) lock[files[i]] = 1 }
        $3 in lock { print $3 ", held by " $2 " (pid " $1 ")"; exit }
      ') || holder=""
    [[ -n $holder ]] || break
    if [[ -z $first ]]; then
      first=$holder
      echo "apt ($resolver): waiting up to ${left}s for $first"
    fi
    sleep 1
    waited=$((waited + 1))
    left=$((left - 1))
  done
  if (( waited > 0 )); then
    echo "::notice::apt ($resolver) waited ${waited}s for $first"
  fi
}

# one attempt, as root, of `left` seconds: the package indexes, then the packages, each once the
# locks it takes are free; apt itself waits out what is left, should another process take one
# meanwhile.
attempt() {
  local resolver=$1 left=$2
  shift 2
  local options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
  wait_for /var/lib/apt/lists/lock
  echo "apt ($resolver): update package indexes"
  apt-get "${options[@]}" -o "DPkg::Lock::Timeout=$left" update -qq
  wait_for /var/lib/dpkg/lock-frontend /var/lib/dpkg/lock
  echo "apt ($resolver): install $*"
  apt-get "${options[@]}" -o "DPkg::Lock::Timeout=$left" install -y -qq "$@"
}

install() {
  local resolver=$1 result=0
  shift
  echo "::group::apt ($resolver): attempt limited to $attempt_limit"
  sudo timeout --verbose --signal=TERM --kill-after=10s "$attempt_limit" \
    bash -c "set -euo pipefail; $(declare -f wait_for attempt); attempt \"\$@\"" \
    bash "$resolver" "${attempt_limit%s}" "$@" || result=$?
  echo '::endgroup::'
  if [[ $result == 124 || $result == 137 ]]; then
    echo "::warning::apt ($resolver) timed out after $attempt_limit"
  elif (( result != 0 )); then
    echo "::warning::apt ($resolver) failed with exit code $result"
  fi
  return "$result"
}

if ! install runner "$@"; then
  echo "::warning::apt failed with the runner's resolver and mirror; trying public resolvers and the mirrors after azure's"
  printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' | sudo tee /etc/resolv.conf > /dev/null
  # no apt setting fails a transfer over for being slow, only for stalling, and azure's mirror can
  # crawl for many minutes (actions/runner-images#14823)
  if [[ -f $mirrors ]]; then
    sudo sed -i '/azure\.archive\.ubuntu\.com/d' "$mirrors"
  fi
  install public "$@"
fi
