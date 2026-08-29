#!/usr/bin/env bash
set -euo pipefail

root=/mnt/fetchloom
images=/var/tmp/fetchloom-images
sudo mkdir -p "$root"
mkdir -p "$images"

sudo apt-get update
sudo apt-get install --yes btrfs-progs xfsprogs dosfstools nfs-kernel-server

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

for name in btrfs xfs fat small second readonly nfs-export nfs; do
  sudo mkdir -p "$root/$name"
done

sudo mount -o loop "$images/btrfs.img" "$root/btrfs"
sudo mount -o loop "$images/xfs.img" "$root/xfs"
sudo mount -o loop,umask=000 "$images/fat.img" "$root/fat"
sudo mount -o loop "$images/small.img" "$root/small"
sudo mount -o loop "$images/second.img" "$root/second"
sudo mount -o loop,ro "$images/readonly.img" "$root/readonly"
sudo chmod 1777 "$root/btrfs" "$root/xfs" "$root/small" "$root/second" "$root/nfs-export"

echo "$root/nfs-export 127.0.0.1(rw,sync,no_subtree_check,no_root_squash)" | sudo tee -a /etc/exports
sudo systemctl restart nfs-server
sudo exportfs -ra
sudo mount -t nfs -o vers=4 127.0.0.1:"$root/nfs-export" "$root/nfs"

memory=/dev/shm/fetchloom
mkdir -p "$memory"

{
  echo "FETCHLOOM_TEST_CLONE_VOLUMES=$root/btrfs:$root/xfs"
  echo "FETCHLOOM_TEST_CASE_SENSITIVE_VOLUMES=$root/btrfs:$root/xfs"
  echo "FETCHLOOM_TEST_NETWORK_VOLUMES=$root/nfs"
  echo "FETCHLOOM_TEST_MEMORY_VOLUMES=$memory"
  echo "FETCHLOOM_TEST_NO_OWNERSHIP_VOLUMES=$root/fat"
  echo "FETCHLOOM_TEST_NO_SPARSE_VOLUMES=$root/fat"
  echo "FETCHLOOM_TEST_SMALL_VOLUMES=$root/small"
  echo "FETCHLOOM_TEST_READ_ONLY_VOLUMES=$root/readonly"
  echo "FETCHLOOM_TEST_SECOND_VOLUMES=$root/second"
  echo "FETCHLOOM_TEST_OTHER_OWNER=nobody"
} >> "$GITHUB_ENV"

findmnt --noheadings --output TARGET,FSTYPE,OPTIONS --types btrfs,xfs,vfat,ext4,nfs4,nfs
