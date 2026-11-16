#!/usr/bin/env bash
# runs tests on a disk image of the filesystem $1 names: hfs, every test that touches a disk, or
# exfat, which macos gives neither links nor a rename that refuses a taken name, the placing ones
set -euo pipefail
: "${RUNNER_TEMP:?RUNNER_TEMP must name the runner scratch folder}"

name=${1:?name hfs or exfat}
# hfs+ keeps no sparse file, so a test that needs one takes the build's folder, never a skip
case "$name" in
  hfs) format=HFS+ size=4g volume=fetchloom scope=--disk require=sparse-files ;;
  exfat) format=ExFAT size=64m volume=FLX scope=--placing require= ;;
  *) echo "::error::no disk image recipe for $name"; exit 2 ;;
esac
image="$RUNNER_TEMP/$name.dmg"
mount="$RUNNER_TEMP/$name"
receipt="$RUNNER_TEMP/$name-attach.plist"
device="$mount"

detach() {
  local attempt
  for attempt in 1 2 3; do
    echo "$name: detach $device, attempt $attempt of 3"
    if hdiutil detach "$device"; then
      return 0
    fi
    if (( attempt < 3 )); then
      sleep 2
    fi
  done
  echo "::warning::$name device $device is busy; forcing its detach"
  hdiutil detach -force "$device"
}

cleanup() {
  local result=$?
  trap - EXIT
  if detach; then
    rm -f "$image" "$receipt"
  else
    echo "::error::could not detach the owned $name device $device"
    if (( result == 0 )); then
      result=1
    fi
  fi
  exit "$result"
}

hdiutil create -quiet -size "$size" -fs "$format" -volname "$volume" "$image"
hdiutil attach -plist -nobrowse -mountpoint "$mount" "$image" > "$receipt"
trap cleanup EXIT
device=$(python3 - "$receipt" "$mount" <<'PY'
import plistlib, re, sys
with open(sys.argv[1], 'rb') as stream:
    entities = plistlib.load(stream)['system-entities']
devices = [entry['dev-entry'] for entry in entities if re.fullmatch(r'/dev/disk\d+', entry.get('dev-entry', ''))]
if len(devices) != 1 or not any(entry.get('mount-point') == sys.argv[2] for entry in entities):
    raise SystemExit('hdiutil did not report one owned device and the requested mount')
print(devices[0])
PY
)
sudo mdutil -i off "$mount"
echo "$name: mounted $device at $mount with indexing disabled"
diskutil info "$mount"
TMPDIR="$mount" FETCHLOOM_TEST_REQUIRE="$require" python3 python/ci_tests.py run "$name" "$scope"
