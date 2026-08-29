#!/usr/bin/env bash
set -euo pipefail

root=/Volumes
images=/var/tmp/fetchloom-images
mkdir -p "$images"

attach() {
  name=$1
  size=$2
  format=$3
  hdiutil create -size "$size" -type SPARSE -fs "$format" -volname "$name" "$images/$name" >/dev/null
  hdiutil attach "$images/$name.sparseimage" -mountpoint "$root/$name" >/dev/null
}

attach fetchloom-apfs 1g APFS
attach fetchloom-apfs-sensitive 1g "Case-sensitive APFS"
attach fetchloom-hfs 1g "HFS+"
attach fetchloom-small 8m APFS

{
  echo "FETCHLOOM_TEST_CLONE_VOLUMES=$root/fetchloom-apfs:$root/fetchloom-apfs-sensitive"
  echo "FETCHLOOM_TEST_CASE_SENSITIVE_VOLUMES=$root/fetchloom-apfs-sensitive"
  echo "FETCHLOOM_TEST_CASE_INSENSITIVE_VOLUMES=$root/fetchloom-apfs"
  echo "FETCHLOOM_TEST_NORMALIZING_VOLUMES=$root/fetchloom-hfs"
  echo "FETCHLOOM_TEST_SMALL_VOLUMES=$root/fetchloom-small"
  echo "FETCHLOOM_TEST_SECOND_VOLUMES=$root/fetchloom-apfs"
} >> "$GITHUB_ENV"

mount
