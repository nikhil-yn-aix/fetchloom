#!/usr/bin/env bash
# builds the lab fixtures in $LAB_DATA (default /home/fl/lab-data) and prints their sha256.
# every byte is AES-128-CTR keystream keyed by the seed and the fixture name, so any machine
# builds the same files. fixtures that exist are kept, so running it again only prints.
set -euo pipefail

data=${LAB_DATA:-/home/fl/lab-data}
seed=fetchloom-lab-1
stamp=@1704067200
export LC_ALL=C TZ=UTC
umask 022

stream() { # name bytes
  local key
  key=$(printf '%s/%s' "$seed" "$1" | sha256sum | head -c 32)
  head -c "$2" /dev/zero | openssl enc -aes-128-ctr -nosalt -K "$key" -iv 00000000000000000000000000000000
}

need() { # true when $1 is missing, after clearing a partial build
  [[ -e $1 ]] && return 1
  rm -rf "$1.part"
}

seal() { # fixes modes and times so archives and etags never change, then publishes $1.part
  chmod -R u=rwX,go=rX "$1.part"
  find "$1.part" -exec touch -h -d "$stamp" {} +
  mv "$1.part" "$1"
}

stage() { # name bytes: a member in .stage for the archives
  mkdir -p .stage
  stream "$1" "$2" > ".stage/$1"
  chmod 644 ".stage/$1"
  touch -d "$stamp" ".stage/$1"
}

mkdir -p "$data"
cd "$data"
rm -rf .stage

for size in 1m 10m 100m 1g; do
  if need "file-$size.bin"; then
    stream "file-$size.bin" "${size^^}" > "file-$size.bin.part"
    seal "file-$size.bin"
  fi
done

if need files-10k; then
  for dir in $(seq -w 0 99); do
    mkdir -p "files-10k.part/$dir"
    stream "files-10k/$dir" 409600 | split -b 4096 -d -a 2 --additional-suffix=.bin - "files-10k.part/$dir/"
  done
  seal files-10k
fi

if need many-1k; then
  mkdir many-1k.part
  stream many-1k 102400000 | split -b 102400 -d -a 3 --additional-suffix=.bin - many-1k.part/
  seal many-1k
fi

if need files-10k.zip; then
  find files-10k -type f | sort | zip -0 -X -D -q -@ files-10k.zip.part
  seal files-10k.zip
fi

if need files-10k.tar.zst; then
  tar --sort=name --format=gnu --owner=0 --group=0 --numeric-owner -cf - files-10k | zstd -q -T1 > files-10k.tar.zst.part
  seal files-10k.tar.zst
fi

if need big-members.zip; then
  for i in 0 1 2 3; do stage "member-$i.bin" 128M; done
  (cd .stage && zip -0 -X -q ../big-members.zip.part member-*.bin)
  rm -rf .stage
  seal big-members.zip
fi

if need big.tar.gz; then
  stage big.bin 512M
  tar --format=gnu --owner=0 --group=0 --numeric-owner -C .stage -cf - big.bin | gzip -n > big.tar.gz.part
  rm -rf .stage
  seal big.tar.gz
fi

sha256sum file-1m.bin file-10m.bin file-100m.bin file-1g.bin files-10k.zip files-10k.tar.zst big-members.zip big.tar.gz
for dir in files-10k many-1k; do
  sum=$(cd "$dir" && find . -type f | sort | xargs sha256sum | sha256sum)
  echo "${sum%% *}  $dir/"
done
