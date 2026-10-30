#!/bin/sh
# installs fetchloom and fl from a github release into ~/.local/bin, and adds that folder to
# PATH in your shell profiles when PATH lacks it.
#
#   curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
#   curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh -s -- 0.1.0
#
# the version is the first argument, else FETCHLOOM_VERSION, else the latest release.
# FETCHLOOM_DOWNLOAD_URL names a folder that holds a release's files, for testing.

set -eu

repository=https://github.com/nikhil-yn-aix/fetchloom
target=x86_64-unknown-linux-musl
bin_dir=$HOME/.local/bin

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

if command -v curl > /dev/null 2>&1; then
    download() {
        curl --proto "$protocols" --tlsv1.2 --fail --silent --show-error --location --retry 3 \
            --output "$2" "$1"
    }
elif command -v wget > /dev/null 2>&1; then
    download() {
        wget -q -O "$2" "$1"
    }
else
    fail "installing fetchloom needs curl or wget"
fi

if command -v sha256sum > /dev/null 2>&1; then
    hash_file() {
        sha256sum "$1" | cut -d ' ' -f 1
    }
elif command -v shasum > /dev/null 2>&1; then
    hash_file() {
        shasum -a 256 "$1" | cut -d ' ' -f 1
    }
else
    fail "installing fetchloom needs sha256sum or shasum to check the download"
fi

case "$(uname -s)/$(uname -m)" in
    Linux/x86_64 | Linux/amd64) ;;
    *) fail "there is no fetchloom build for $(uname -s) on $(uname -m). build it from source: $repository#install" ;;
esac

version=${1:-${FETCHLOOM_VERSION:-}}
version=${version#v}
case $version in
    '' | [0-9]*) ;;
    *) fail "$version is not a version. give one such as 0.1.0" ;;
esac

protocols='=https'
if [ -n "${FETCHLOOM_DOWNLOAD_URL:-}" ]; then
    release=${FETCHLOOM_DOWNLOAD_URL%/}
    protocols='=http,https'
elif [ -n "$version" ]; then
    release=$repository/releases/download/v$version
else
    release=$repository/releases/latest/download
fi

mkdir -p "$bin_dir"
# staged beside the destination, so moving each program into place is a rename
work=$bin_dir/.fetchloom-install-$$
rm -rf "$work"
mkdir "$work"
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

echo "downloading $release/SHA256SUMS"
download "$release/SHA256SUMS" "$work/SHA256SUMS" || fail "could not download $release/SHA256SUMS"
line=$(grep -E '^[0-9a-f]{64} [ *]fetchloom-[^ ]+-'"$target"'\.tar\.gz$' "$work/SHA256SUMS" | head -n 1)
[ -n "$line" ] || fail "$release/SHA256SUMS lists no fetchloom archive for $target"
expected=${line%% *}
asset=${line##*[ *]}
found=${asset#fetchloom-}
found=${found%-"$target".tar.gz}
if [ -n "$version" ] && [ "$found" != "$version" ]; then
    fail "$release holds fetchloom $found, not $version"
fi
if [ -z "${FETCHLOOM_DOWNLOAD_URL:-}" ]; then
    release=$repository/releases/download/v$found
fi

echo "downloading $release/$asset"
download "$release/$asset" "$work/$asset" || fail "could not download $release/$asset"
actual=$(hash_file "$work/$asset")
if [ "$actual" != "$expected" ]; then
    fail "$asset has SHA-256 $actual, but SHA256SUMS lists $expected. nothing was installed"
fi

tar -xzf "$work/$asset" -C "$work"
folder=$work/${asset%.tar.gz}
for name in fetchloom fl; do
    [ -f "$folder/$name" ] || fail "$asset holds no $name. nothing was installed"
done
for name in fetchloom fl; do
    mv -f "$folder/$name" "$bin_dir/$name"
done
installed=$("$bin_dir/fetchloom" --version)
echo "installed $installed and fl in $bin_dir"

case ":$PATH:" in
    *":$bin_dir:"* | *":$bin_dir/:"*)
        echo "next: run fetchloom to see the first steps"
        ;;
    *)
        # shellcheck disable=SC2016
        export_line='export PATH="$HOME/.local/bin:$PATH"'
        changed=''
        for profile in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
            if [ "$profile" != "$HOME/.profile" ] && [ ! -f "$profile" ]; then
                continue
            fi
            if ! grep -qsxF "$export_line" "$profile"; then
                printf '\n# added by the fetchloom installer\n%s\n' "$export_line" >> "$profile"
                changed="$changed ~/${profile##*/}"
            fi
        done
        if [ -n "$changed" ]; then
            echo "added $bin_dir to PATH in$changed"
        fi
        echo "next: open a new terminal and run fetchloom to see the first steps"
        ;;
esac
