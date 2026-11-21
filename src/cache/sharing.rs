//! what the group a cache is shared with gets of what a run makes there. a lab's cache is a
//! setgid folder its group may write, and the usual `umask 022` would give each folder a run makes
//! there to its owner alone, so the rest of the group could not add to it. so each folder and
//! file a run makes in such a cache gives the group what it gives its owner, as far as the
//! cache's root gives the group, whatever the umask, as git's `core.sharedRepository = group`
//! does: folders the group may add to, files it may read, and write where their owner may. an
//! object, read-only for its owner, stays read-only. nothing is ever given to users outside the
//! group.

use std::fs::File;
use std::path::Path;

/// how a cache is shared, as its root said when the cache was opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sharing {
    /// the group the root shares the cache with; none for a cache that is no group's, whose
    /// folders and files keep the modes the umask gives them.
    #[cfg(unix)]
    group: Option<Group>,
}

/// a group a cache is shared with: its id, and the bits the cache's root gives it, which are the
/// most it gets of anything a run makes, with the id of the user this run makes things as.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Group {
    id: u32,
    bits: u32,
    user: u32,
}

impl Sharing {
    /// how the cache at `root` is shared: with the root's group when the root is setgid and its
    /// group may write it, as the clusters guide makes a lab's cache, and this user is in that
    /// group. a cache folder fetchloom made itself is its owner's alone, and no group's.
    pub(crate) fn of(root: &Path) -> Sharing {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let user = super::made_as();
            let group = std::fs::metadata(root)
                .ok()
                .and_then(|found| shared_with(found.mode(), found.gid(), &in_group))
                .map(|(id, bits)| Group { id, bits, user });
            Sharing { group }
        }
        #[cfg(not(unix))]
        {
            let _ = root;
            Sharing::default()
        }
    }

    /// whether a group shares the cache.
    pub(crate) fn is_shared(self) -> bool {
        #[cfg(unix)]
        {
            self.group.is_some()
        }
        #[cfg(not(unix))]
        {
            let _ = self;
            false
        }
    }

    /// gives the group the cache is shared with what `handle`, of a folder or file this run just
    /// made in the cache, gives its owner, as far as the root gives the group. one that another
    /// user owns, or that another group has, as below a folder someone took the setgid bit from,
    /// gets nothing. a failure leaves it as the umask made it, which the next run warns of for a
    /// folder.
    pub(crate) fn share(self, handle: &File) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let Some(group) = self.group else {
                return;
            };
            let shared = handle.metadata().and_then(|found| {
                let mode = found.mode() & 0o7777;
                let wanted = given(mode, group.bits);
                if wanted == mode || found.uid() != group.user || found.gid() != group.id {
                    return Ok(());
                }
                handle.set_permissions(std::fs::Permissions::from_mode(wanted))
            });
            if let Err(err) = shared {
                tracing::debug!(%err, "could not give the group that shares the cache its part");
            }
        }
        #[cfg(not(unix))]
        let _ = (self, handle);
    }

    /// [`Sharing::share`] of the folder `path` this run just made below the cache's `root`,
    /// reached from the root without following a link.
    pub(crate) fn share_folder(self, root: &Path, path: &Path) {
        #[cfg(unix)]
        if self.is_shared()
            && let Ok(below) = path.strip_prefix(root)
            && let Ok(folder) = super::folder::walk(root, below, None)
        {
            self.share(&folder);
        }
        #[cfg(not(unix))]
        let _ = (self, root, path);
    }

    /// [`Sharing::share`] of the plain file at `path` this run just made, opened without
    /// following a link.
    pub(crate) fn share_file(self, path: &Path) {
        if self.is_shared()
            && let Ok(file) = crate::volume::read_unfollowed(path)
        {
            self.share(&file);
        }
    }

    /// makes the folders missing from `root` down to the folder `below` it, as a folder mirror a
    /// group shares needs them, opening each without following a link and giving the group what
    /// [`Sharing::share`] gives of each one made. nothing for a folder no group shares.
    ///
    /// # Errors
    ///
    /// a folder that could not be made or opened, or a link or a file in a folder's place.
    #[cfg(unix)]
    pub(crate) fn make_folders(self, root: &Path, below: &Path) -> std::io::Result<()> {
        if !self.is_shared() {
            return Ok(());
        }
        super::folder::walk(root, below, Some(self)).map(drop)
    }

    #[cfg(not(unix))]
    #[expect(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "no group shares a folder there"
    )]
    pub(crate) fn make_folders(self, root: &Path, below: &Path) -> std::io::Result<()> {
        let _ = (root, below);
        Ok(())
    }

    /// how the group this sharing shares `what` with, as "the cache", may write `folder` there
    /// again, which refused a write and lets the group no write: its owner gives the group its
    /// write. none when no group shares it, or the group may write the folder.
    pub(crate) fn opening(self, folder: &Path, what: &str) -> Option<String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let found = std::fs::symlink_metadata(folder).ok()?;
            if !self.is_shared() || !found.is_dir() || found.mode() & 0o020 != 0 {
                return None;
            }
            let shown = crate::suggested::shell_folder_path(folder);
            Some(if found.uid() == super::run_as() {
                format!(
                    "the folder is yours, and a group shares {what}: give yourself and the group its write with `chmod ug+rwx {shown}`"
                )
            } else {
                format!(
                    "a group shares {what}, but only the folder's owner may write to it: they give the group its write with `chmod g+rwx {shown}`"
                )
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (self, folder, what);
            None
        }
    }
}

/// whether a group shares the cache whose root has `mode`: the root is setgid and its group may
/// write it, as the clusters guide makes a lab's cache.
#[cfg(unix)]
pub(super) fn group_shares(mode: u32) -> bool {
    mode & 0o2020 == 0o2020
}

/// the group a cache's root of `mode` and group `id` shares the cache with, and the root's bits
/// for it, when [`group_shares`] says a group does and `member` says this user is in it.
#[cfg(unix)]
pub(super) fn shared_with(mode: u32, id: u32, member: &dyn Fn(u32) -> bool) -> Option<(u32, u32)> {
    (group_shares(mode) && member(id)).then_some((id, mode & 0o070))
}

/// `mode` with the group given each bit its owner has, as far as `bits`, the root's bits for the
/// group, go. what others have stays as it is.
#[cfg(unix)]
fn given(mode: u32, bits: u32) -> u32 {
    mode | (((mode & 0o700) >> 3) & bits)
}

/// whether this process is in the group `id`, as its own group or one of its others.
#[cfg(unix)]
pub(super) fn in_group(id: u32) -> bool {
    rustix::process::getegid().as_raw() == id
        || rustix::process::getgroups()
            .unwrap_or_default()
            .iter()
            .any(|group| group.as_raw() == id)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// a root that is setgid and its group may write shares the cache with that group, when this
    /// user is in it, and gives it no more than its own bits for the group; what it gives others
    /// gives others nothing.
    #[test]
    fn a_setgid_root_its_group_may_write_shares_the_cache() {
        let member = |id: u32| id == 20;
        let shared = |mode| shared_with(mode, 20, &member).map(|(_, bits)| bits);
        assert_eq!(shared(0o42770), Some(0o070));
        assert_eq!(
            shared(0o42777),
            Some(0o070),
            "others' bits are not the group's"
        );
        assert_eq!(shared(0o42730), Some(0o030));
        assert_eq!(shared(0o42750), None, "the group may not write");
        assert_eq!(shared(0o40770), None, "not setgid");
        assert_eq!(shared(0o40700), None, "a cache fetchloom made");
        assert_eq!(
            shared_with(0o42770, 30, &member),
            None,
            "not this user's group"
        );
    }

    /// the group gets each bit the owner has, as far as the root's bits go: a folder the group
    /// may add to, a file it may read, and write where the owner may, an object read-only for
    /// everyone; what others have stays as the umask gave it.
    #[test]
    fn the_group_gets_what_the_owner_has_as_far_as_the_root_goes() {
        let cases = [
            (0o2755, 0o070, 0o2775),
            (0o2700, 0o070, 0o2770),
            (0o644, 0o070, 0o664),
            (0o600, 0o070, 0o660),
            (0o444, 0o070, 0o444),
            (0o400, 0o070, 0o440),
            (0o604, 0o070, 0o664),
            (0o2700, 0o030, 0o2730),
            (0o600, 0o030, 0o620),
        ];
        for (mode, bits, wanted) in cases {
            assert_eq!(given(mode, bits), wanted, "{mode:o} with {bits:o}");
        }
    }
}
