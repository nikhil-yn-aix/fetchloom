//! what a warning is about, which `--json` names as its `kind`, so a script can act on one
//! without reading its words, which may change.

/// a warning's kind. each is named in docs/user/reference/json-output.md, which a test checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Warning {
    /// an archive stays packed: fetchloom does not unpack its format, it needs other parts, it
    /// came inside another archive, or `-o` saved it whole.
    ArchivePacked,
    /// a file named as an archive is no archive fetchloom can read, so it stays as it is.
    ArchiveUnreadable,
    /// after a sign-out, another place still holds a key for the account.
    AuthKeyRemains,
    /// a key holds a character no request header can carry, so it is never sent.
    AuthKeyUnsendable,
    /// the machine has no credential store, so a key went into a file of fetchloom's.
    AuthNoKeyring,
    /// other users of the machine may read a file that holds logins or keys.
    AuthReadable,
    /// a recipe changed since it was built.
    BuildChanged,
    /// the pinned result of a recipe is on neither this machine nor a mirror, so it is left out.
    BuildLeftOut,
    /// a recipe is not built yet.
    BuildUnbuilt,
    /// whether a recipe changed since its build could not be told.
    BuildUnchecked,
    /// a cached file changed, so it is downloaded again when it is needed.
    CacheChanged,
    /// a path in the cache could not be read, or removed.
    CacheUnreadable,
    /// the project could not join the cache's list of projects, which gc keeps files for.
    CacheUnregistered,
    /// a server sent a file compressed, which is kept as it came.
    DownloadEncoded,
    /// a server gave no name for a file.
    DownloadUnnamed,
    /// a link meant for a data file answered with a web page.
    DownloadWebPage,
    /// a list `export` writes leaves out files no link holds.
    ExportLeftOut,
    /// git does not have code a recipe runs.
    GitCodeUntracked,
    /// git does not have lists in data.lock.d that data.lock names.
    GitLockPartUntracked,
    /// an import left out links or special files, which fetchloom never follows.
    ImportSkipped,
    /// a dataset's license does not allow some use.
    LicenseRestricted,
    /// data.lock.d holds lists data.lock no longer names.
    LockStalePart,
    /// a merge kept one side's entry of a dataset, which the other side writes differently
    /// only in comments or spacing.
    MergeFormatting,
    /// after a merge, neither side's data.lock pins datasets as data.toml now asks.
    MergeUnpinned,
    /// a mirror holds a damaged copy, so it was not used.
    MirrorDamaged,
    /// a mirror is missing, unreadable or unreachable, so this run skips it.
    MirrorSkipped,
    /// `cache gc` could not read copies on a mirror.
    MirrorUnreadable,
    /// the name an add chose was taken, so the dataset got a number.
    NameTaken,
    /// a later run reaches a private address only once `allowed_origins` lists it.
    NetworkPrivateOrigin,
    /// a request failed and is tried again.
    NetworkRetry,
    /// a path starts with a folder named `~`, which is not the home folder there.
    PathTilde,
    /// a removal deletes files that were already there when fetchloom adopted them.
    PlaceAdoptedRemoved,
    /// something in the way keeps files out of the data folder.
    PlaceBlocked,
    /// files were copied, not linked to the cache.
    PlaceCopied,
    /// an exclude names files inside an archive, which only a members table reaches.
    PlaceExcludeUnreached,
    /// a file differs from data.lock and stays as it is.
    PlaceKept,
    /// files stay in a folder no dataset uses now.
    PlaceLeftBehind,
    /// a dry run of `sync --force` would replace a file sync keeps.
    PlaceOverwritten,
    /// a dataset's `path` makes a folder named like the one file it holds.
    PlacePathFolder,
    /// files fetchloom placed were deleted or moved, so they were put back.
    PlacePutBack,
    /// a changed file was put back as data.lock pins it.
    PlaceRestored,
    /// files have names windows and fat disks cannot store.
    PlaceUnportable,
    /// a stopped add could not put data.toml and data.lock back as they were.
    ProjectNotRestored,
    /// a catalog did not answer a search, or its search failed.
    SearchPartial,
    /// `FETCHLOOM_COLORS` holds entries fetchloom cannot read.
    SettingsColorsUnread,
    /// a variable set is one typo from one fetchloom reads.
    SettingsUnknownVariable,
    /// a file is gone from its link in data.lock and came from where the source lists it now.
    SourceMoved,
    /// what a source lists may not be all of the dataset.
    SourcePartial,
    /// `FETCHLOOM_TRACE` names a file fetchloom cannot write.
    TraceUnwritable,
}

impl Warning {
    /// the kind as `--json` names it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Warning::ArchivePacked => "archive.packed",
            Warning::ArchiveUnreadable => "archive.unreadable",
            Warning::AuthKeyRemains => "auth.key_remains",
            Warning::AuthKeyUnsendable => "auth.key_unsendable",
            Warning::AuthNoKeyring => "auth.no_keyring",
            Warning::AuthReadable => "auth.readable",
            Warning::BuildChanged => "build.changed",
            Warning::BuildLeftOut => "build.left_out",
            Warning::BuildUnbuilt => "build.unbuilt",
            Warning::BuildUnchecked => "build.unchecked",
            Warning::CacheChanged => "cache.changed",
            Warning::CacheUnreadable => "cache.unreadable",
            Warning::CacheUnregistered => "cache.unregistered",
            Warning::DownloadEncoded => "download.encoded",
            Warning::DownloadUnnamed => "download.unnamed",
            Warning::DownloadWebPage => "download.web_page",
            Warning::ExportLeftOut => "export.left_out",
            Warning::GitCodeUntracked => "git.code_untracked",
            Warning::GitLockPartUntracked => "git.lock_part_untracked",
            Warning::ImportSkipped => "import.skipped",
            Warning::LicenseRestricted => "license.restricted",
            Warning::LockStalePart => "lock.stale_part",
            Warning::MergeFormatting => "merge.formatting",
            Warning::MergeUnpinned => "merge.unpinned",
            Warning::MirrorDamaged => "mirror.damaged",
            Warning::MirrorSkipped => "mirror.skipped",
            Warning::MirrorUnreadable => "mirror.unreadable",
            Warning::NameTaken => "name.taken",
            Warning::NetworkPrivateOrigin => "network.private_origin",
            Warning::NetworkRetry => "network.retry",
            Warning::PathTilde => "path.tilde",
            Warning::PlaceAdoptedRemoved => "place.adopted_removed",
            Warning::PlaceBlocked => "place.blocked",
            Warning::PlaceCopied => "place.copied",
            Warning::PlaceExcludeUnreached => "place.exclude_unreached",
            Warning::PlaceKept => "place.kept",
            Warning::PlaceLeftBehind => "place.left_behind",
            Warning::PlaceOverwritten => "place.overwritten",
            Warning::PlacePathFolder => "place.path_folder",
            Warning::PlacePutBack => "place.put_back",
            Warning::PlaceRestored => "place.restored",
            Warning::PlaceUnportable => "place.unportable",
            Warning::ProjectNotRestored => "project.not_restored",
            Warning::SearchPartial => "search.partial",
            Warning::SettingsColorsUnread => "settings.colors_unread",
            Warning::SettingsUnknownVariable => "settings.unknown_variable",
            Warning::SourceMoved => "source.moved",
            Warning::SourcePartial => "source.partial",
            Warning::TraceUnwritable => "trace.unwritable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Warning;

    /// every kind, listed once. a kind missing from the list fails to compile, since the match
    /// in it must name them all.
    macro_rules! every_kind {
        ($($kind:ident),* $(,)?) => {{
            fn listed(kind: Warning) {
                match kind {
                    $(Warning::$kind)|* => {}
                }
            }
            let _ = listed;
            [$(Warning::$kind),*]
        }};
    }

    /// the JSON reference names every kind of warning, and no kind that does not exist, so a
    /// script can look up each one it meets.
    #[test]
    fn every_warning_kind_is_documented() {
        let kinds = every_kind![
            ArchivePacked,
            ArchiveUnreadable,
            AuthKeyRemains,
            AuthKeyUnsendable,
            AuthNoKeyring,
            AuthReadable,
            BuildChanged,
            BuildLeftOut,
            BuildUnbuilt,
            BuildUnchecked,
            CacheChanged,
            CacheUnreadable,
            CacheUnregistered,
            DownloadEncoded,
            DownloadUnnamed,
            DownloadWebPage,
            ExportLeftOut,
            GitCodeUntracked,
            GitLockPartUntracked,
            ImportSkipped,
            LicenseRestricted,
            LockStalePart,
            MergeFormatting,
            MergeUnpinned,
            MirrorDamaged,
            MirrorSkipped,
            MirrorUnreadable,
            NameTaken,
            NetworkPrivateOrigin,
            NetworkRetry,
            PathTilde,
            PlaceAdoptedRemoved,
            PlaceBlocked,
            PlaceCopied,
            PlaceExcludeUnreached,
            PlaceKept,
            PlaceLeftBehind,
            PlaceOverwritten,
            PlacePathFolder,
            PlacePutBack,
            PlaceRestored,
            PlaceUnportable,
            ProjectNotRestored,
            SearchPartial,
            SettingsColorsUnread,
            SettingsUnknownVariable,
            SourceMoved,
            SourcePartial,
            TraceUnwritable,
        ];
        let mut named: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
        named.sort_unstable();
        let docs = include_str!("../../docs/user/reference/json-output.md");
        let mut documented: Vec<&str> = docs
            .lines()
            .skip_while(|line| *line != "## warning kinds")
            .skip_while(|line| !line.starts_with("| `"))
            .take_while(|line| line.starts_with('|'))
            .filter_map(|row| row.split('`').nth(1))
            .collect();
        documented.sort_unstable();
        assert_eq!(documented, named);
    }
}
