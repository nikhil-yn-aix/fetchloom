#!/usr/bin/env bash
set -euo pipefail

env_file=/tmp/fetchloom-volumes.env
bash /workspace/verify/volumes-linux.sh "$env_file"
set -a
. "$env_file"
set +a

export FETCHLOOM_VERIFY=1
export FETCHLOOM_VERIFY_VOLUMES=1

for target in "$@"; do
  echo "target $target"
  cargo clippy --workspace --all-targets --target "$target"
  cargo test --workspace --target "$target"
  cargo run -p xtask -- network "/target/$target/debug/fetchloom"
done
