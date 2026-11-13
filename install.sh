#!/bin/sh
# installs fetchloom from a github release into ~/.local/bin, with fl as a short name for it
# there unless another fl would be hidden, and adds that folder to PATH in your shell profiles
# when PATH lacks it.
#
#   curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
#   curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh -s -- 0.1.0
#
# the version is the first argument, else FETCHLOOM_VERSION, else the latest release.
# FETCHLOOM_NO_MODIFY_PATH=1 leaves your shell profiles alone.
# FETCHLOOM_DOWNLOAD_URL names a folder that holds a release's files, for testing.

set -eu

repository=https://github.com/nikhil-yn-aix/fetchloom
bin_dir=$HOME/.local/bin

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

check_destination() {
    if [ -d "$bin_dir/fetchloom" ]; then
        fail "$bin_dir/fetchloom is a directory or a link to one. nothing was installed"
    fi
}

if command -v curl > /dev/null 2>&1; then
    download() {
        curl --proto "$protocols" --tlsv1.2 --fail --silent --show-error --location --retry 3 \
            --output "$2" "$1"
    }
elif command -v wget > /dev/null 2>&1; then
    # gnu wget takes the options rustup's installer passes. busybox wget knows neither, and a
    # download it cannot hold to https lets anyone on the way swap the archive and its sum alike
    case $(wget --help 2>&1 || true) in
        *--https-only*--secure-protocol* | *--secure-protocol*--https-only*) ;;
        *) fail "this wget cannot hold the download to https and tls 1.2, since it lacks --https-only or --secure-protocol, so nothing was installed. install curl, then run the installer again" ;;
    esac
    download() {
        if [ "$protocols" = '=https' ]; then
            wget --https-only --secure-protocol=TLSv1_2 -q -O "$2" "$1"
        else
            wget --secure-protocol=TLSv1_2 -q -O "$2" "$1"
        fi
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

system=$(uname -s)
machine=$(uname -m)
# a shell under rosetta reports x86_64 on apple silicon, where the arm64 build runs natively
if [ "$system/$machine" = Darwin/x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2> /dev/null)" = 1 ]; then
    machine=arm64
fi
case "$system/$machine" in
    Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
    Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    Darwin/arm64 | Darwin/aarch64) target=aarch64-apple-darwin ;;
    *) fail "there is no fetchloom build for $system on $machine. supported machines: linux on x86_64, amd64, aarch64 or arm64, and macos on x86_64 or arm64. build it from source: $repository#install" ;;
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

check_destination
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
[ -f "$folder/fetchloom" ] || fail "$asset holds no fetchloom. nothing was installed"
# run the staged program, which sits in the destination folder, before it replaces anything: a
# noexec disk or a build this machine cannot run fails here, leaving a working fetchloom in place
installed=$("$folder/fetchloom" --version) ||
    fail "the downloaded fetchloom does not run on this machine. nothing was installed"
check_destination
mv -f "$folder/fetchloom" "$bin_dir/fetchloom"
echo "installed $installed in $bin_dir"

# fl is a link beside fetchloom, so it runs whatever version is installed. it is made only where
# no other fl would be hidden or replaced, and a link made before is kept
short=$bin_dir/fl
ours=''
if [ -L "$short" ] && [ "$short" -ef "$bin_dir/fetchloom" ]; then
    ours=1
fi
other=$(command -v fl 2> /dev/null || true)
if [ -n "$other" ] && [ "$other" -ef "$bin_dir/fetchloom" ]; then
    other=''
fi
if [ -z "$ours" ] && { [ -e "$short" ] || [ -L "$short" ]; }; then
    echo "left ~/.local/bin/fl alone: it is not a link to fetchloom. to replace it with one: ln -sf fetchloom ~/.local/bin/fl"
elif [ -n "$other" ]; then
    echo "left fl alone: it runs $other here. to use fl for fetchloom in your shell, add alias fl=fetchloom to its startup file"
elif [ -n "$ours" ] || ln -s fetchloom "$short"; then
    echo "fl runs fetchloom too"
else
    echo "warning: fl was not made; fetchloom itself is installed" >&2
fi

case ":$PATH:" in
    *":$bin_dir:"* | *":$bin_dir/:"*)
        echo "next: run fetchloom to see the first steps"
        ;;
    *)
        if [ "${FETCHLOOM_NO_MODIFY_PATH:-}" = 1 ]; then
            echo "$bin_dir is not on PATH, and FETCHLOOM_NO_MODIFY_PATH=1 left your shell profiles alone"
            echo "next: add $bin_dir to PATH, then run fetchloom to see the first steps"
            exit 0
        fi
        # shellcheck disable=SC2016
        export_line='export PATH="$HOME/.local/bin:$PATH"'
        changed=''
        # zsh, the default shell on macos, never reads ~/.profile, so macos gets a ~/.zshrc
        always="$HOME/.profile"
        if [ "$system" = Darwin ]; then
            always="$always $HOME/.zshrc"
        fi
        for profile in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
            case " $always " in
                *" $profile "*) ;;
                *) [ -f "$profile" ] || continue ;;
            esac
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
