#!/usr/bin/env bash
set -euo pipefail

env_file=${1:-/tmp/fetchloom-volumes.env}
root=/mnt/fetchloom
images=/var/tmp/fetchloom-images
other=fetchloom-other
mkdir -p "$root" "$images"

# The suite needs a second account to prove what a foreign owner does. The
# container image used to create it, and nothing on a build machine will.
id "$other" >/dev/null 2>&1 || useradd --create-home --shell /bin/sh "$other"

# A loop device outlives the container that attached it, and the machine has
# eight. Release the ones a previous run left before asking for six more.
for name in btrfs xfs fat small second readonly; do
  umount "$root/$name" 2>/dev/null || true
done
umount "$root/fuse" 2>/dev/null || true
losetup -a | awk -F: -v images="$images" '$0 ~ images {print $1}' | while read -r device; do
  losetup -d "$device" 2>/dev/null || true
done

truncate -s 1G "$images/btrfs.img"
mkfs.btrfs -q "$images/btrfs.img"
truncate -s 1G "$images/xfs.img"
mkfs.xfs -q -m reflink=1 "$images/xfs.img"
truncate -s 64M "$images/fat.img"
mkfs.vfat -F 32 "$images/fat.img" >/dev/null
truncate -s 16M "$images/small.img"
mkfs.ext4 -q -F "$images/small.img"
truncate -s 64M "$images/second.img"
mkfs.ext4 -q -F "$images/second.img"
truncate -s 64M "$images/readonly.img"
mkfs.ext4 -q -F "$images/readonly.img"

for name in btrfs xfs fat small second readonly; do
  mkdir -p "$root/$name"
done

# `mount -o loop` takes whatever loop device is already there and fails when
# every one of them is taken, which is what a previous run leaves behind.
# Asking loop-control for a device creates one, so the lane does not depend on
# how many the machine happened to start with.
attach() {
  losetup --find --show "$1"
}

mount "$(attach "$images/btrfs.img")" "$root/btrfs"
mount "$(attach "$images/xfs.img")" "$root/xfs"
mount -o umask=000,uid=65534,gid=65534 "$(attach "$images/fat.img")" "$root/fat"
mount "$(attach "$images/small.img")" "$root/small"
mount "$(attach "$images/second.img")" "$root/second"
mount -o ro "$(attach "$images/readonly.img")" "$root/readonly"
chmod 1777 "$root/btrfs" "$root/xfs" "$root/small" "$root/second"

mkdir -p "$root/fuse-source" "$root/fuse"
chmod 1777 "$root/fuse-source"
bindfs -o allow_other "$root/fuse-source" "$root/fuse"

memory=/dev/shm/fetchloom
mkdir -p "$memory"
chmod 1777 "$memory"

# A cache is refused on a network volume, and the trust taxonomy rests on that
# refusal, so the row needs a real one rather than a fake. An NFS export served
# and mounted on this same machine is network backed by every answer the
# platform gives, and it is the only network filesystem a build machine can
# stand up alone. It is best effort: a runner whose kernel serves no NFS leaves
# the variable unset, and the tests that need it decline by name.
network=""
if command -v exportfs >/dev/null 2>&1 || apt-get install -y -qq nfs-kernel-server >/dev/null 2>&1; then
  served=/srv/fetchloom-nfs
  mkdir -p "$served" "$root/network"
  chmod 1777 "$served"
  umount "$root/network" 2>/dev/null || true
  grep -q "^$served " /etc/exports 2>/dev/null ||
    echo "$served 127.0.0.1(rw,sync,no_subtree_check,no_root_squash,insecure)" >> /etc/exports
  if (service nfs-kernel-server restart || systemctl restart nfs-kernel-server) >/dev/null 2>&1 &&
    exportfs -ra >/dev/null 2>&1 &&
    mount -t nfs -o vers=4,nolock 127.0.0.1:"$served" "$root/network" >/dev/null 2>&1; then
    chmod 1777 "$root/network"
    network="$root/network"
  fi
fi

{
  echo "FETCHLOOM_TEST_CLONE_VOLUMES=$root/btrfs:$root/xfs"
  echo "FETCHLOOM_TEST_CASE_SENSITIVE_VOLUMES=$root/btrfs:$root/xfs"
  echo "FETCHLOOM_TEST_FUSE_VOLUMES=$root/fuse"
  echo "FETCHLOOM_TEST_MEMORY_VOLUMES=$memory"
  echo "FETCHLOOM_TEST_NO_OWNERSHIP_VOLUMES=$root/fat"
  echo "FETCHLOOM_TEST_NO_SPARSE_VOLUMES=$root/fat"
  echo "FETCHLOOM_TEST_SMALL_VOLUMES=$root/small"
  echo "FETCHLOOM_TEST_READ_ONLY_VOLUMES=$root/readonly"
  echo "FETCHLOOM_TEST_SECOND_VOLUMES=$root/second"
  echo "FETCHLOOM_TEST_OTHER_OWNER=$other"
  if [ -n "$network" ]; then echo "FETCHLOOM_TEST_NETWORK_VOLUMES=$network"; fi
} > "$env_file"

findmnt --noheadings --output TARGET,FSTYPE,OPTIONS --types btrfs,xfs,vfat,ext4,fuse.bindfs
