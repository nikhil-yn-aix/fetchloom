# Other platforms

## windows clippy from linux

CI lints windows with clippy, and code behind `#[cfg(windows)]` compiles nowhere else. clippy needs no linker, so linux can check it for the `x86_64-pc-windows-msvc` target. the build scripts of zstd, liblzma and ring compile C with a windows compiler, which linux lacks, so two stand-ins let them finish: one writes an empty object where `-o` names one, the other an empty library where `-out:` names one, as `lib.exe` would.

```sh
rustup target add x86_64-pc-windows-msvc
stand_in=$(mktemp -d)
printf '#!/bin/sh\nwhile [ $# -gt 0 ]; do [ "$1" = -o ] && : > "$2"; shift; done\n' > "$stand_in/cc"
printf '#!/bin/sh\nfor arg; do case $arg in -out:*) : > "${arg#-out:}" ;; esac; done\n' > "$stand_in/lib"
chmod +x "$stand_in/cc" "$stand_in/lib"
CC_x86_64_pc_windows_msvc="$stand_in/cc" AR_x86_64_pc_windows_msvc="$stand_in/lib" \
  cargo clippy --target x86_64-pc-windows-msvc --all-targets --locked -- -D warnings
```

it checks the rust code only: no C is compiled, nothing links and no test runs, so the windows jobs in CI remain the proof.

## WSL gate

use a linux clone whose `origin` is your windows repository's local path, not GitHub. with that clone at `~/fetchloom`, these commands test its local `main` without a push. fetch the working branch instead when checking an unmerged change.

```sh
git -C ~/fetchloom fetch origin main
git -C ~/fetchloom worktree add --detach ~/fetchloom-gate origin/main
cd ~/fetchloom-gate
export CARGO_TARGET_DIR="$HOME/fetchloom-target"
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --release --locked -- -D warnings
cargo test --locked
python3 -m unittest discover -s python
python3 -m unittest discover -s bench
```

keep build output on the linux filesystem. the separate worktree lets windows and linux checks avoid rewriting the same build files. once finished, leave the worktree and remove it with `git -C ~/fetchloom worktree remove ~/fetchloom-gate`.

## xfs loop mount

ext4 does not exercise reflinks. on linux with `xfsprogs` installed, a local image gives the placement tests an xfs destination. the mount commands need root; run cargo as your normal user. the example puts test files outside the checkout; confirm that your home folder is not itself a git repository or fetchloom project.

```sh
scratch="$HOME/fetchloom-xfs-$$"
mkdir "$scratch"
mkdir "$scratch/xfs"
truncate -s 4G "$scratch/xfs.img"
mkfs.xfs -q "$scratch/xfs.img"
sudo mount -o loop "$scratch/xfs.img" "$scratch/xfs"
sudo chown "$(id -u):$(id -g)" "$scratch/xfs"
TMPDIR="$scratch/xfs" cargo test --locked --test cli placing::
sudo umount "$scratch/xfs"
rm "$scratch/xfs.img"
rmdir "$scratch/xfs" "$scratch"
```

unmount even if tests fail. do not delete an image while it is mounted. [CI](ci.md) reruns the tests that touch the disk on xfs and btrfs, the alias tests in an ext4 folder that ignores case, on ReFS, exFAT and FAT32 on windows, and on hfs+ on macos; the full suite runs once on each default filesystem.

a fat image that root mounts with `sudo mount -o loop,umask=000` lets every user write but refuses chmod to all but root, as some shared drives do. `FETCHLOOM_TEST_NOOP_CHMOD=<mount> cargo test --locked --lib place::tests::a_writable_copy_works_where_the_mount_refuses_chmod -- --exact` checks that a writable copy still works there, as linux CI does; without the variable that test returns at once.

on nfs 4.2 from a linux server that hands out delegated timestamps (RFC 9754), as 7.0 does, the tests that fake an edit a coarse clock hides, by writing a file and putting its old time back, fail by design: the server keeps a time the client sets under a write delegation only if it is newer than the file's times when the delegation began, and stamps the file when the delegation ends. run those on a local filesystem.
