#!/usr/bin/env bash
# installs ubuntu packages with apt. when the runner's own resolver cannot find ubuntu's archive,
# as azure's could not on 2026-11-11, it asks public resolvers instead. apt verifies the archive's
# signed package lists and each package's hash, so a resolver cannot change what installs.
set -euo pipefail

install() {
  sudo apt-get -o Acquire::Retries=3 update -qq &&
    sudo apt-get -o Acquire::Retries=3 install -y -qq "$@" > /dev/null
}

if ! install "$@"; then
  echo "::warning::apt failed with the runner's resolver; trying public resolvers"
  printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' | sudo tee /etc/resolv.conf > /dev/null
  install "$@"
fi
