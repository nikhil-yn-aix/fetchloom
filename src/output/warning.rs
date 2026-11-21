//! what a warning is about, which `--json` names as its `kind`, so a script can act on one
//! without reading its words, which may change.

/// a warning's kind. each is named in docs/user/reference/json-output.md, which a test checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Warning {
    /// an archive stays packed: fetchloom does not unpack its format, it needs other parts, it
    /// came inside another archive, or `-o` saved it whole.
    ArchivePacked,
    /// files sit where an archive's files land, but they do not prove the list of files data.lock
    /// pins for it, so the archive is downloaded all the same.
    ArchiveUnproven,
    /// a file named as an archive is no archive fetchloom can read, so it stays as it is.
    ArchiveUnreadable,
    /// after a sign-out, another place still holds a key for the account.
    AuthKeyRemains,
    /// a key holds a character no request header can carry, so it is never sent.
    AuthKeyUnsendable,
    /// a key went into a file of fetchloom's: the machine has no credential store, or
    /// `FETCHLOOM_KEYRING=false` keeps keys out of it.
    AuthNoKeyring,
    /// after a sign-in, a variable or another file still gives the account a key, which runs use
    /// before the one saved.
    AuthOverridden,
    /// other users of the machine may read a file that holds logins or keys.
    AuthReadable,
    /// keys or credentials are set up in a way fetchloom cannot use, as the message says.
    AuthUnusable,
    /// a cached file changed, so it is downloaded again when it is needed.
    CacheChanged,
    /// the cache folder belongs to another user, or every user may write to it.
    CacheForeign,
    /// a run told `--yes` goes on though its downloads take the cache past `cache_budget`.
    CacheOverBudget,
    /// in a cache its group shares, a folder lets only its owner write to it, so the group's other
    /// members cannot add to the cache.
    #[cfg_attr(
        all(not(unix), not(test)),
        expect(
            dead_code,
            reason = "only unix gives a folder's group a write of its own"
        )
    )]
    CacheUnshared,
    /// a path in the cache, or the data.lock of a project it keeps files for, could not be read,
    /// or removed.
    CacheUnreadable,
    /// the project could not join the cache's list of projects, which gc keeps files for.
    CacheUnregistered,
    /// the cache had lost its tag, as a cleaner deleting old files takes it, and held only what
    /// fetchloom keeps there.
    CacheUntagged,
    /// a server sent a file compressed, which is kept as it came.
    DownloadEncoded,
    /// a server gave no name for a file.
    DownloadUnnamed,
    /// a link meant for a data file answered with a web page.
    DownloadWebPage,
    /// a citation `export` writes is built from data.lock alone, so it lacks what only a DOI's
    /// record gives, such as authors and a year: offline, doi.org gave nothing, or no DOI.
    ExportIncomplete,
    /// a list `export` writes leaves out files no link holds.
    ExportLeftOut,
    /// a line of the project's .gitignore hides the whole data folder from git, a person's own
    /// files there too, though each dataset folder keeps itself out of git.
    GitDataFolderIgnored,
    /// a dataset places a .gitignore of its own at the top of its folder, where fetchloom would keep
    /// the one that hides the folder from git, and no line of the project's .gitignore hides it.
    GitDatasetUnignored,
    /// git sees a data folder link fetchloom follows, which no line of the project's .gitignore
    /// hides, so a commit takes it and each clone then refuses it.
    GitDataLinkUnignored,
    /// a folder fetchloom writes in, the cache or a mirror a setting or a push names, is in a git
    /// work tree no ignore rule keeps it out of, where `git add .` takes what fetchloom writes.
    GitFolderInTree,
    /// files an import read are in the git work tree, where a commit takes or keeps them.
    GitImportInTree,
    /// git does not have lists in data.lock.d that data.lock names.
    GitLockPartUntracked,
    /// an import left out links or special files, which fetchloom never follows.
    ImportSkipped,
    /// a dataset's license does not allow some use.
    LicenseRestricted,
    /// a dataset waits for its file list, which data.lock names and data.lock.d lacks.
    LockListMissing,
    /// data.lock.d holds lists data.lock no longer names.
    LockStalePart,
    /// a merge kept one side's entry of a dataset, which the other side writes differently
    /// only in comments or spacing.
    MergeFormatting,
    /// a side a person chose in a merge left out the other side's change of a dataset.
    MergeLeftOut,
    /// after a merge, neither side's data.lock pins datasets as data.toml now asks.
    MergeUnpinned,
    /// a mirror holds a damaged copy, so it was not used.
    MirrorDamaged,
    /// a mirror is missing, unreadable or unreachable, so this run skips it.
    MirrorSkipped,
    /// the name an add chose was taken, so the dataset got a number.
    NameTaken,
    /// a later run reaches a private address only once `allowed_origins` lists it.
    NetworkPrivateOrigin,
    /// a request failed and is tried again.
    NetworkRetry,
    /// an output's folder changed since it was pinned: a file changed, added or removed, so sync
    /// left it whole and push sends the pinned files.
    OutputChanged,
    /// an output's files are in no cache or mirror this machine reaches, as in a clone of the
    /// project that made it, so sync leaves it out and places the rest.
    OutputElsewhere,
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
    /// a changed file was put back as data.lock pins it.
    PlaceRestored,
    /// a file `sync --force` moved out of the way could not go back, so it is kept under a hidden
    /// name.
    PlaceSetAside,
    /// files have names windows and fat disks cannot store.
    PlaceUnportable,
    /// a stopped add could not put data.toml and data.lock back as they were.
    ProjectNotRestored,
    /// `.fetchloom/owner` held no record fetchloom writes, so taking the project removed it.
    ProjectOwnerRemoved,
    /// the add that starts a project found an R package, whose data/ R CMD check keeps for R's own
    /// datasets.
    ProjectRPackage,
    /// a push of every dataset leaves out one only its source's digests pin, until a sync
    /// downloads it.
    PushLeftOut,
    /// a catalog did not answer a search, or its search failed.
    SearchPartial,
    /// the settings file `settings set` or `unset` changed still holds a problem every run stops
    /// at, which the change did not bring.
    SettingsBroken,
    /// `FETCHLOOM_COLORS` holds entries fetchloom cannot read.
    SettingsColorsUnread,
    /// `settings set` saved a mirror or push folder that does not exist.
    SettingsMissingFolder,
    /// a variable set here holds a setting the settings file just changed, so runs where it is
    /// set keep the variable's value.
    SettingsOverridden,
    /// a variable that changes only what a person sees holds neither true nor false.
    SettingsSwitchUnread,
    /// a variable set is one typo from one fetchloom reads.
    SettingsUnknownVariable,
    /// a file is gone from its link in data.lock, or changed there, and came from a web archive.
    SourceArchived,
    /// a file is gone from its link in data.lock and came from where the source lists it now.
    SourceMoved,
    /// what a source lists may not be all of the dataset.
    SourcePartial,
    /// a pinned file's server says it changed after data.lock pinned it, at the same size.
    SourceModified,
    /// a run told `--yes` goes on though a disk may lack the room it needs.
    SpaceShort,
    /// `FETCHLOOM_TRACE` names a file fetchloom cannot write.
    TraceUnwritable,
}

impl Warning {
    /// the kind as `--json` names it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Warning::ArchivePacked => "archive.packed",
            Warning::ArchiveUnproven => "archive.unproven",
            Warning::ArchiveUnreadable => "archive.unreadable",
            Warning::AuthKeyRemains => "auth.key_remains",
            Warning::AuthKeyUnsendable => "auth.key_unsendable",
            Warning::AuthNoKeyring => "auth.no_keyring",
            Warning::AuthOverridden => "auth.overridden",
            Warning::AuthReadable => "auth.readable",
            Warning::AuthUnusable => "auth.unusable",
            Warning::CacheChanged => "cache.changed",
            Warning::CacheForeign => "cache.foreign",
            Warning::CacheOverBudget => "cache.over_budget",
            Warning::CacheUnreadable => "cache.unreadable",
            Warning::CacheUnshared => "cache.unshared",
            Warning::CacheUnregistered => "cache.unregistered",
            Warning::CacheUntagged => "cache.untagged",
            Warning::DownloadEncoded => "download.encoded",
            Warning::DownloadUnnamed => "download.unnamed",
            Warning::DownloadWebPage => "download.web_page",
            Warning::ExportIncomplete => "export.incomplete",
            Warning::ExportLeftOut => "export.left_out",
            Warning::GitDataFolderIgnored => "git.data_folder_ignored",
            Warning::GitDatasetUnignored => "git.dataset_unignored",
            Warning::GitDataLinkUnignored => "git.data_link_unignored",
            Warning::GitFolderInTree => "git.folder_in_tree",
            Warning::GitImportInTree => "git.import_in_tree",
            Warning::GitLockPartUntracked => "git.lock_part_untracked",
            Warning::ImportSkipped => "import.skipped",
            Warning::LicenseRestricted => "license.restricted",
            Warning::LockListMissing => "lock.list_missing",
            Warning::LockStalePart => "lock.stale_part",
            Warning::MergeFormatting => "merge.formatting",
            Warning::MergeLeftOut => "merge.left_out",
            Warning::MergeUnpinned => "merge.unpinned",
            Warning::MirrorDamaged => "mirror.damaged",
            Warning::MirrorSkipped => "mirror.skipped",
            Warning::NameTaken => "name.taken",
            Warning::NetworkPrivateOrigin => "network.private_origin",
            Warning::NetworkRetry => "network.retry",
            Warning::OutputChanged => "output.changed",
            Warning::OutputElsewhere => "output.elsewhere",
            Warning::PathTilde => "path.tilde",
            Warning::PlaceAdoptedRemoved => "place.adopted_removed",
            Warning::PlaceBlocked => "place.blocked",
            Warning::PlaceCopied => "place.copied",
            Warning::PlaceExcludeUnreached => "place.exclude_unreached",
            Warning::PlaceKept => "place.kept",
            Warning::PlaceLeftBehind => "place.left_behind",
            Warning::PlaceOverwritten => "place.overwritten",
            Warning::PlacePathFolder => "place.path_folder",
            Warning::PlaceRestored => "place.restored",
            Warning::PlaceSetAside => "place.set_aside",
            Warning::PlaceUnportable => "place.unportable",
            Warning::ProjectNotRestored => "project.not_restored",
            Warning::ProjectOwnerRemoved => "project.owner_removed",
            Warning::ProjectRPackage => "project.r_package",
            Warning::PushLeftOut => "push.left_out",
            Warning::SearchPartial => "search.partial",
            Warning::SettingsBroken => "settings.broken",
            Warning::SettingsColorsUnread => "settings.colors_unread",
            Warning::SettingsMissingFolder => "settings.missing_folder",
            Warning::SettingsOverridden => "settings.overridden",
            Warning::SettingsSwitchUnread => "settings.switch_unread",
            Warning::SettingsUnknownVariable => "settings.unknown_variable",
            Warning::SourceArchived => "source.archived",
            Warning::SourceModified => "source.modified",
            Warning::SourceMoved => "source.moved",
            Warning::SourcePartial => "source.partial",
            Warning::SpaceShort => "space.short",
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

    /// the JSON reference names no kind that does not exist, so a script can look up each it meets.
    #[test]
    fn every_warning_kind_is_documented() {
        let kinds = every_kind![
            ArchivePacked,
            ArchiveUnproven,
            ArchiveUnreadable,
            AuthKeyRemains,
            AuthKeyUnsendable,
            AuthNoKeyring,
            AuthOverridden,
            AuthReadable,
            AuthUnusable,
            CacheChanged,
            CacheForeign,
            CacheOverBudget,
            CacheUnreadable,
            CacheUnregistered,
            CacheUnshared,
            CacheUntagged,
            DownloadEncoded,
            DownloadUnnamed,
            DownloadWebPage,
            ExportIncomplete,
            ExportLeftOut,
            GitDataFolderIgnored,
            GitDatasetUnignored,
            GitDataLinkUnignored,
            GitFolderInTree,
            GitImportInTree,
            GitLockPartUntracked,
            ImportSkipped,
            LicenseRestricted,
            LockListMissing,
            LockStalePart,
            MergeFormatting,
            MergeLeftOut,
            MergeUnpinned,
            MirrorDamaged,
            MirrorSkipped,
            NameTaken,
            NetworkPrivateOrigin,
            NetworkRetry,
            OutputChanged,
            OutputElsewhere,
            PathTilde,
            PlaceAdoptedRemoved,
            PlaceBlocked,
            PlaceCopied,
            PlaceExcludeUnreached,
            PlaceKept,
            PlaceLeftBehind,
            PlaceOverwritten,
            PlacePathFolder,
            PlaceRestored,
            PlaceSetAside,
            PlaceUnportable,
            ProjectNotRestored,
            ProjectOwnerRemoved,
            ProjectRPackage,
            PushLeftOut,
            SearchPartial,
            SettingsBroken,
            SettingsColorsUnread,
            SettingsMissingFolder,
            SettingsOverridden,
            SettingsSwitchUnread,
            SettingsUnknownVariable,
            SourceArchived,
            SourceModified,
            SourceMoved,
            SourcePartial,
            SpaceShort,
            TraceUnwritable,
        ];
        let mut named: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
        named.sort_unstable();
        let docs = include_str!("../../docs/user/reference/json-output.md");
        let mut documented: Vec<&str> = docs
            .lines()
            .skip_while(|line| *line != "## Warning kinds")
            .skip_while(|line| !line.starts_with("| `"))
            .take_while(|line| line.starts_with('|'))
            .filter_map(|row| row.split('`').nth(1))
            .collect();
        documented.sort_unstable();
        assert_eq!(documented, named);
    }
}
