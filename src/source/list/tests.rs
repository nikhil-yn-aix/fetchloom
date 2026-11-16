use super::*;

const SHA: &str = "9cc1c345c71bcc9b486b74cbf6063fa66f4bb5e0f603a4b3c3471ec2e5e8e355";
const OTHER: &str = "e54cc4d2ce1bff65d32ca60b3e4b802e06bde1d7e7caf6f796f6bf7370e863b0";
const MD5: &str = "d41d8cd98f00b204e9800998ecf8427e";

fn sha(hex: &str) -> Option<Checksum> {
    Sha256::from_hex(hex).map(Checksum::Sha256)
}

fn md5(hex: &str) -> Option<Checksum> {
    Md5::from_hex(hex).map(Checksum::Md5)
}

fn path(path: &str) -> Named {
    Named::Path {
        path: path.to_owned(),
        link: None,
    }
}

fn entries(text: &str) -> Vec<(Named, Option<Checksum>)> {
    read("SUMS", text.as_bytes(), 100)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.named, entry.checksum))
        .collect()
}

fn refused(text: &str) -> Error {
    read("SUMS", text.as_bytes(), 100).unwrap_err()
}

/// GNU's two modes and `md5 -r`'s one space, BSD's tagged lines, pooch's registry and a list of
/// links all read as the same files with the same hashes.
#[test]
fn every_published_form_reads_as_the_same_files() {
    let same = [
        (path("iris.csv"), sha(SHA)),
        (path("sub/tips.csv"), md5(MD5)),
    ];
    for text in [
        format!("{SHA}  iris.csv\n{MD5}  sub/tips.csv\n"),
        format!("{SHA} *iris.csv\r\n{MD5} *./sub/tips.csv\r\n"),
        format!("{SHA} iris.csv\n{MD5} sub/tips.csv\n"),
        format!("SHA256 (iris.csv) = {SHA}\nMD5 (sub/tips.csv) = {MD5}\n"),
        format!(
            "SHA2-256(iris.csv)= {SHA}\nmd5 (./sub/tips.csv) = {}\n",
            MD5.to_uppercase()
        ),
        format!("# pooch\niris.csv {SHA}\n\nsub/tips.csv md5:{MD5}\n"),
        format!(
            "\u{feff}iris.csv sha256:{}\nsub/tips.csv MD5:{MD5}\n",
            SHA.to_uppercase()
        ),
    ] {
        assert_eq!(entries(&text), same, "{text}");
    }
    assert_eq!(
        entries(&format!(
            "  https://example.org/iris.csv   sha256:{SHA}  \nhttps://example.org/tips.csv md5:{MD5}\n{OTHER}  https://example.org/wine.csv\nhttps://example.org/plain.csv\nzenodo:3608990\nnames.csv {MD5} https://elsewhere.org/names.csv\n"
        )),
        [
            (
                Named::Link("https://example.org/iris.csv".to_owned()),
                sha(SHA)
            ),
            (
                Named::Link("https://example.org/tips.csv".to_owned()),
                md5(MD5)
            ),
            (
                Named::Link("https://example.org/wine.csv".to_owned()),
                sha(OTHER)
            ),
            (
                Named::Link("https://example.org/plain.csv".to_owned()),
                None
            ),
            (Named::Link("zenodo:3608990".to_owned()), None),
            (
                Named::Path {
                    path: "names.csv".to_owned(),
                    link: Some("https://elsewhere.org/names.csv".to_owned()),
                },
                md5(MD5)
            ),
        ]
    );
    assert_eq!(
        entries(&format!("{SHA}  name with spaces (1).csv\n")),
        [(path("name with spaces (1).csv"), sha(SHA))]
    );
}

/// stated hashes stay MD5 and SHA-256: any other is refused by its name, however the line gives
/// it, and a hash of the wrong length says what one has.
#[test]
fn a_hash_other_than_md5_or_sha256_is_refused_by_name() {
    let sha1 = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
    let not_md5 = format!("\"{MD5}\" on line 1 of SUMS is not a SHA-256");
    for (text, message) in [
        (
            format!("{sha1}  iris.csv"),
            "line 1 of SUMS gives a SHA-1 hash, and fetchloom checks only MD5 and SHA-256",
        ),
        (
            format!("\nSHA1 (iris.csv) = {sha1}"),
            "line 2 of SUMS gives a SHA-1 hash, and fetchloom checks only MD5 and SHA-256",
        ),
        (
            format!("iris.csv sha1:{sha1}"),
            "line 1 of SUMS gives a SHA-1 hash, and fetchloom checks only MD5 and SHA-256",
        ),
        (
            format!("https://example.org/iris.csv {sha1}"),
            "line 1 of SUMS gives a SHA-1 hash, and fetchloom checks only MD5 and SHA-256",
        ),
        (
            format!("SHA512 (iris.csv) = {SHA}{SHA}"),
            "line 1 of SUMS gives a SHA-512 hash, and fetchloom checks only MD5 and SHA-256",
        ),
        (
            "iris.csv xxh128:0123".to_owned(),
            "line 1 of SUMS gives an xxHash hash, and fetchloom checks only MD5 and SHA-256",
        ),
        (
            "iris.csv md5:0123".to_owned(),
            "\"md5:0123\" on line 1 of SUMS is not an MD5",
        ),
        (format!("SHA256 (iris.csv) = {MD5}"), not_md5.as_str()),
        (
            "https://example.org/iris.csv abc".to_owned(),
            "\"abc\" on line 1 of SUMS is not an MD5 or a SHA-256",
        ),
    ] {
        let error = refused(&text);
        assert_eq!(error.kind, Kind::Usage, "{text}");
        assert_eq!(error.message, message, "{text}");
    }
}

/// a list is anyone's text: a path that could leave the list's folder or name repository
/// metadata is refused before any request, as is a line in no known form.
#[test]
fn a_line_that_could_leave_its_folder_is_refused() {
    for (text, message) in [
        (
            format!("{SHA}  ../secret.csv"),
            "line 1 of SUMS names \"../secret.csv\", which fetchloom refuses because it climbs out of its folder",
        ),
        (
            format!("{SHA}  /etc/passwd"),
            "line 1 of SUMS names \"/etc/passwd\", which fetchloom refuses because it starts at the root of the disk",
        ),
        (
            format!("SHA256 (sub/../../x) = {SHA}"),
            "line 1 of SUMS names \"sub/../../x\", which fetchloom refuses because it climbs out of its folder",
        ),
        (
            format!("../x.csv {SHA}"),
            "line 1 of SUMS names \"../x.csv\", which fetchloom refuses because it climbs out of its folder",
        ),
        (
            format!("{SHA}  .git/config"),
            "line 1 of SUMS names \".git/config\", which fetchloom refuses because it names repository metadata",
        ),
        (
            format!("{SHA}  C:\\data\\x.csv"),
            "line 1 of SUMS names \"C:\\\\data\\\\x.csv\", which fetchloom refuses because it has a \\, which windows reads as a folder separator",
        ),
        (
            format!("\\{SHA}  a\\nb"),
            "line 1 of SUMS writes its file's name with \\ escapes, for a \\ or a line break, which no path fetchloom places may hold",
        ),
        (SHA.to_owned(), "line 1 of SUMS gives a hash and no file"),
        (
            format!("https://example.org/a.csv {SHA} https://example.org/b.csv"),
            "line 1 of SUMS holds more than a link and its hash",
        ),
        (
            format!("a.csv {SHA} b.csv"),
            "line 1 of SUMS gives \"b.csv\" where a pooch registry gives the file's link",
        ),
        (
            "fashion mnist".to_owned(),
            "line 1 of SUMS is not a link, a DOI or an id",
        ),
    ] {
        let error = refused(&text);
        assert_eq!(error.kind, Kind::Usage, "{text}");
        assert_eq!(error.message, message, "{text}");
    }
    let many = format!("{SHA}  a\n{SHA}  b\n{SHA}  c\n");
    let error = read("SUMS", many.as_bytes(), 2).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(
        error.message,
        "SUMS lists more than 2 files, the most this run lists for one dataset"
    );
}

/// a path lands below the list's folder at its origin, each part written out, so no name can
/// reach another folder, host or query; a link keeps its own place.
#[test]
fn a_list_at_a_link_places_each_path_below_its_folder() {
    let at = Url::parse("https://example.org/data/SHA256SUMS?dl=1#top").unwrap();
    let list = List::Link(at.clone());
    let text = format!(
        "{SHA}  iris.csv\n{OTHER}  sub/odd name?#%.csv\n{MD5}  https://elsewhere.org/x.csv\nhttp:x {MD5}\n"
    );
    let error = listing(
        &list,
        read("SHA256SUMS", text.as_bytes(), 10).unwrap(),
        Some(&at),
    )
    .unwrap_err();
    assert!(
        error
            .message
            .starts_with("line 4 of https://example.org/data/SHA256SUMS")
            && error
                .message
                .ends_with(" names http:x, and each line of a list is one file's link"),
        "{}",
        error.message
    );
    let text = text.lines().take(3).collect::<Vec<_>>().join("\n");
    let listed = listing(
        &list,
        read("SHA256SUMS", text.as_bytes(), 10).unwrap(),
        Some(&at),
    )
    .unwrap();
    let files: Vec<(&str, &str, Option<Sha256>, Option<Md5>)> = listed
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.url.as_str(), file.sha256, file.md5))
        .collect();
    assert_eq!(
        files,
        [
            (
                "iris.csv",
                "https://example.org/data/iris.csv",
                Sha256::from_hex(SHA),
                None
            ),
            (
                "sub/odd name?#%.csv",
                "https://example.org/data/sub/odd%20name%3F%23%25.csv",
                Sha256::from_hex(OTHER),
                None
            ),
            (
                "x.csv",
                "https://elsewhere.org/x.csv",
                None,
                Md5::from_hex(MD5)
            ),
        ]
    );
    assert!(listed.files.iter().all(|file| !file.named_by_server));
    assert_eq!(listed.resolved, Reference::List(list));
}

/// a list may repeat a line, as openbsd's own SHA256 does: a file at the same hash counts once,
/// whichever form each line has. one path with two hashes is two files of one name, still refused.
#[test]
fn a_repeated_line_counts_once_and_a_path_with_two_hashes_is_refused() {
    let repeated = format!(
        "SHA256 (install78.img) = {SHA}\nSHA256 (install78.iso) = {OTHER}\nSHA256 (install78.img) = {SHA}\n{SHA}  install78.img\n"
    );
    assert_eq!(
        entries(&repeated),
        [
            (path("install78.img"), sha(SHA)),
            (path("install78.iso"), sha(OTHER))
        ]
    );
    let at = Url::parse("https://example.org/pub/SHA256").unwrap();
    let list = List::Link(at.clone());
    let twice = format!("{SHA}  install78.img\n{OTHER}  install78.img\n");
    let listed = listing(
        &list,
        read("SHA256", twice.as_bytes(), 10).unwrap(),
        Some(&at),
    )
    .unwrap();
    let error = crate::source::check(&Reference::List(list), listed, 10).unwrap_err();
    assert_eq!(
        error.message,
        "list:https://example.org/pub/SHA256 lists \"install78.img\" twice"
    );
}

/// what unix `sum` and `cksum` write, a short sum and a size, shows nothing of a file's bytes:
/// such a line is refused by what it is. a pooch line whose path is digits stays a pooch line.
#[test]
fn a_unix_sum_line_is_refused_as_a_sum_fetchloom_cannot_check() {
    for line in [
        "08820   582 Saccharomyces_cerevisiae.R64-1-1.113.gtf.gz",
        "35632 3 README",
        "4294967295 1024 two words.txt",
    ] {
        let error = refused(&format!("# sums\n{line}\n"));
        assert_eq!(
            error.message,
            "line 2 of SUMS gives a unix `sum` checksum, a kind fetchloom cannot check: it checks MD5 and SHA-256 alone",
            "{line}"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("list each file's link instead, with its MD5 or SHA-256 if you know it")
        );
    }
    assert_eq!(
        entries(&format!("2024 {SHA}\n")),
        [(path("2024"), sha(SHA))]
    );
}

/// a list gives no path or link longer than data.lock keeps.
#[test]
fn a_list_bounds_its_paths_and_links_as_data_lock_does() {
    assert_eq!(LONGEST_PATH, crate::project::lock::lists::LONGEST_PATH);
    assert_eq!(LONGEST_LINK, crate::project::lock::lists::LONGEST_URL);
}

/// a list on this machine has no folder, so each line gives its file's link; a link that would
/// leave a secret in data.lock is refused.
#[test]
fn a_list_on_this_machine_gives_each_files_link() {
    let list = List::Project("links.txt".to_owned());
    for (text, message) in [
        (
            format!("{SHA}  iris.csv"),
            "line 1 of links.txt names \"iris.csv\", a path, and only a list at a link has a folder to read it below",
        ),
        (
            "iris".to_owned(),
            "line 1 of links.txt is not a link, a DOI or an id",
        ),
        (
            "https://example.org/x.csv?token=hunter2".to_owned(),
            "line 1 of links.txt: this link carries token, a secret that data.lock would keep and share",
        ),
        (
            "https://example.org/".to_owned(),
            "the link on line 1 of links.txt names no file",
        ),
        ("# nothing yet\n".to_owned(), "links.txt lists no files"),
        (
            "https://example.org/x\u{202e}vsc.exe".to_owned(),
            "the link on line 1 of links.txt holds a control character",
        ),
        (
            format!("https://example.org/{}.csv", "x".repeat(64 << 10)),
            "line 1 of links.txt gives a link longer than data.lock keeps",
        ),
        (
            format!("{}/x.csv {SHA} https://example.org/x.csv", "p".repeat(4096)),
            "line 1 of links.txt gives a path longer than data.lock keeps",
        ),
    ] {
        let error =
            listing(&list, read("links.txt", text.as_bytes(), 10).unwrap(), None).unwrap_err();
        assert_eq!(error.message, message, "{text}");
        assert!(!format!("{error:?}").contains("hunter2"), "{error:?}");
    }
}

/// `list:` names a link or a path inside the project, and data.toml gets back what it wrote; a
/// plain link to a checksum list names the files it lists, unless a source reads the link.
#[test]
fn a_list_is_named_by_its_link_or_its_path_in_the_project() {
    for (text, written) in [
        ("list:lists/era5.txt", "list:lists/era5.txt"),
        ("list:./links.txt", "list:links.txt"),
        (
            "list:https://example.org/data/registry.txt",
            "list:https://example.org/data/registry.txt",
        ),
        (
            "https://example.org/data/SHA256SUMS",
            "list:https://example.org/data/SHA256SUMS",
        ),
        (
            "https://physionet.org/files/mitdb/1.0.0/SHA256SUMS.txt",
            "list:https://physionet.org/files/mitdb/1.0.0/SHA256SUMS.txt",
        ),
        (
            "https://example.org/md5sums.txt",
            "list:https://example.org/md5sums.txt",
        ),
        (
            "https://ftp.ncbi.nlm.nih.gov/genomes/all/GCF/000/001/405/GCF_000001405.40_GRCh38.p14/md5checksums.txt",
            "list:https://ftp.ncbi.nlm.nih.gov/genomes/all/GCF/000/001/405/GCF_000001405.40_GRCh38.p14/md5checksums.txt",
        ),
        (
            "https://example.org/data/checksums.txt",
            "list:https://example.org/data/checksums.txt",
        ),
        (
            "https://github.com/o/r/blob/main/SHA1SUMS",
            "list:https://raw.githubusercontent.com/o/r/main/SHA1SUMS",
        ),
        (
            "https://example.org/data/sums.csv",
            "https://example.org/data/sums.csv",
        ),
        ("https://zenodo.org/records/1/files/MD5SUMS", "zenodo:1"),
    ] {
        let parsed = Reference::parse(text).unwrap();
        assert_eq!(parsed.reference.to_string(), written, "{text}");
        assert_eq!(
            Reference::parse(written).unwrap().reference,
            parsed.reference,
            "{text}"
        );
    }
    for text in [
        "list:../outside.txt",
        "list:/etc/passwd",
        "list:~/links.txt",
        "list:$HOME/links.txt",
        "list:.git/config",
        "list:C:\\links.txt",
        "list:",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    let secret = Reference::parse("list:https://example.org/SHA256SUMS?token=hunter2").unwrap();
    assert!(secret.check_shareable().is_err());
}

#[test]
fn a_dataset_of_a_list_is_named_after_the_list_or_its_folder() {
    for (text, name) in [
        (
            "https://physionet.org/files/mitdb/1.0.0/SHA256SUMS.txt",
            "mitdb-1-0-0",
        ),
        ("https://example.org/data/MD5SUMS", "data"),
        ("https://ftp.example.org/pub/yeast/CHECKSUMS", "yeast"),
        ("list:https://example.org/data/registry.txt", "registry"),
        ("list:lists/era5-urls.txt", "era5-urls"),
        ("list:SHA256SUMS", "sha256sums"),
    ] {
        let Reference::List(list) = Reference::parse(text).unwrap().reference else {
            panic!("{text} is no list");
        };
        assert_eq!(
            crate::name::reference_name(&list.naming()).as_deref(),
            Some(name),
            "{text}"
        );
    }
}

/// a list in the project is read only from a real file there, never through a link.
#[test]
fn a_list_in_the_project_is_read_from_a_real_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("lists")).unwrap();
    std::fs::write(
        root.path().join("lists/links.txt"),
        format!("https://example.org/iris.csv {SHA}\n"),
    )
    .unwrap();
    let list = List::Project("lists/links.txt".to_owned());
    let listed = list.read_in(root.path(), 10).unwrap();
    assert_eq!(listed.files.len(), 1);
    assert_eq!(listed.files[0].path, "iris.csv");
    let missing = List::Project("lists/gone.txt".to_owned())
        .read_in(root.path(), 10)
        .unwrap_err();
    assert_eq!(missing.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        missing.message,
        "the list lists/gone.txt is not in the project folder"
    );
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("links.txt"),
        "https://example.org/x.csv\n",
    )
    .unwrap();
    if crate::test_links::link_folder(outside.path(), &root.path().join("away")).is_ok() {
        let error = List::Project("away/links.txt".to_owned())
            .read_in(root.path(), 10)
            .unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{error:?}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("links.txt"),
            root.path().join("linked.txt"),
        )
        .unwrap();
        let error = List::Project("linked.txt".to_owned())
            .read_in(root.path(), 10)
            .unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo, "{error:?}");
        assert!(
            error.message.contains("is a link or a special file"),
            "{error:?}"
        );
    }
}

/// a list in the project is as long as the files it names, not its bytes: one past 64 MiB reads
/// to its last line.
#[test]
fn a_list_in_the_project_past_64_mib_reads_to_its_last_line() {
    let root = tempfile::tempdir().unwrap();
    let comment = format!("# {}\n", "-".repeat(32 << 10));
    let long = comment.repeat((64 << 20) / comment.len() + 1);
    std::fs::write(
        root.path().join("links.txt"),
        format!("https://example.org/iris.csv {SHA}\n{long}https://example.org/tips.csv\n"),
    )
    .unwrap();
    let listed = List::Project("links.txt".to_owned())
        .read_in(root.path(), 10)
        .unwrap();
    let files: Vec<&str> = listed.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(files, ["iris.csv", "tips.csv"]);
}

/// the entries of `text`, a list at `at`, given to its reader in pieces that end at `ends`.
fn in_pieces(text: &str, at: &Url, ends: &[usize]) -> Vec<Entry> {
    let mut lines = Lines::new("SHA256SUMS", 10, Some(at));
    let mut start = 0;
    for &end in ends.iter().chain([text.len()].iter()) {
        Through::piece(&mut lines, &text.as_bytes()[start..end]).unwrap();
        start = end;
    }
    lines.finish().unwrap()
}

/// a list reads the same whatever pieces its bytes come in, a line, a line break or a character
/// split between two, and a reader told to start over reads as a new one.
#[test]
fn a_list_reads_the_same_in_any_pieces() {
    let at = Url::parse("https://example.org/data/SHA256SUMS").unwrap();
    let text = format!(
        "\u{feff}# släkt\r\n{SHA}  iris.csv\r\n\r\nMD5 (sub/tips.csv) = {MD5}\n{OTHER} *dir/naïve.csv\n{SHA}  iris.csv\n{OTHER}  last.csv"
    );
    let whole = in_pieces(&text, &at, &[]);
    assert_eq!(
        whole
            .iter()
            .map(|entry| (entry.number, entry.named.clone(), entry.checksum))
            .collect::<Vec<_>>(),
        [
            (2, path("iris.csv"), sha(SHA)),
            (4, path("sub/tips.csv"), md5(MD5)),
            (5, path("dir/naïve.csv"), sha(OTHER)),
            (7, path("last.csv"), sha(OTHER)),
        ]
    );
    for end in 1..text.len() {
        assert_eq!(in_pieces(&text, &at, &[end]), whole, "pieces end at {end}");
        assert_eq!(
            in_pieces(&text, &at, &[end / 2, end]),
            whole,
            "pieces end at {} and {end}",
            end / 2
        );
        let mut lines = Lines::new("SHA256SUMS", 10, Some(&at));
        Through::piece(&mut lines, &text.as_bytes()[..end]).unwrap();
        lines.restart();
        Through::piece(&mut lines, text.as_bytes()).unwrap();
        assert_eq!(
            lines.finish().unwrap(),
            whole,
            "restarted after {end} bytes"
        );
    }
}

/// a line holds one file, so one longer than a path, a hash and a link as long as data.lock keeps
/// is refused at that line, whole or in pieces, before more of it is held.
#[test]
fn a_line_longer_than_one_file_needs_is_refused_at_that_line() {
    let longest = format!("# {}\n", "-".repeat(LONGEST_LINE - 2));
    let listed = read("SUMS", format!("{longest}{SHA}  iris.csv\n").as_bytes(), 10).unwrap();
    assert_eq!(listed.len(), 1);
    let longer = format!("{SHA}  iris.csv\n# {}\n", "-".repeat(LONGEST_LINE - 1));
    let at = Url::parse("https://example.org/SUMS").unwrap();
    let mut whole = Lines::new("SUMS", 10, Some(&at));
    let mut unended = Lines::new("SUMS", 10, Some(&at));
    let start = longer.len() - 2;
    Through::piece(&mut unended, &longer.as_bytes()[..start]).unwrap();
    for error in [
        read("SUMS", longer.as_bytes(), 10).unwrap_err(),
        Through::piece(&mut whole, longer.as_bytes()).unwrap_err(),
        Through::piece(&mut unended, &longer.as_bytes()[start..=start]).unwrap_err(),
    ] {
        assert_eq!(
            error.message,
            "line 2 of SUMS is longer than 69.0 KiB, the most one line of a list holds"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "a line names one file: data.lock keeps a path of up to 4,096 bytes and a link of up to 64 KiB"
            )
        );
    }
}

/// a repeated line is found by its hash, then by what it names: two files of one hash are both
/// kept, and a repeat of either is not.
#[test]
fn files_of_one_hash_are_told_apart_by_what_they_name() {
    let mut seen = Seen::default();
    let mut entries = Vec::new();
    let mut kept = Vec::new();
    for named in [path("a.csv"), path("b.csv"), path("a.csv"), path("b.csv")] {
        let repeats = seen.repeats_at(7, &entries, &named, sha(SHA));
        if !repeats {
            entries.push(Entry {
                number: entries.len() + 1,
                named,
                checksum: sha(SHA),
            });
        }
        kept.push(!repeats);
    }
    assert_eq!(kept, [true, true, false, false]);
}
