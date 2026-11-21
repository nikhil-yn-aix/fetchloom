//! lists of files as one dataset: a `SHA256SUMS`, an `MD5SUMS`, a pooch registry or a list of
//! links, at a link or in the project, read by one reader.

use std::fs;

use crate::support::{Result, Routes, Sandbox, file, json, md5_hex, sha256_hex, text};

const IRIS: &[u8] = b"sepal,petal\n5.1,1.4\n";
const TIPS: &[u8] = b"bill,tip\n16.99,1.01\n";

/// a server with iris.csv and sub/tips.csv in its `/data` folder, beside `lists`, each a list
/// file's path below `/data` and its text.
fn served(lists: &[(&str, String)]) -> Result<Routes> {
    let mut routes = vec![
        ("/data/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/data/sub/tips.csv".to_owned(), file(TIPS, "\"t1\"")),
    ];
    for (name, text) in lists {
        routes.push((format!("/data/{name}"), file(text.as_bytes(), "\"l1\"")));
    }
    Routes::start(routes)
}

fn gnu() -> String {
    format!(
        "{}  iris.csv\n{}  sub/tips.csv\n",
        sha256_hex(IRIS),
        sha256_hex(TIPS)
    )
}

/// the files data.lock pins for `name`: each file's path, SHA-256 and link, as written.
pub(crate) fn pinned(lock: &str, name: &str) -> Vec<String> {
    let entry = lock
        .split("[[dataset]]")
        .find(|entry| entry.contains(&format!("name = \"{name}\"")))
        .unwrap_or_default();
    entry
        .lines()
        .filter(|line| line.trim_start().starts_with("{ path"))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// `add` of a link to a SHA256SUMS pins every file it lists at the list's folder plus its path,
/// as one dataset; a sync on another machine gets the same bytes from data.lock alone, and
/// `--select` narrows the list as it narrows any dataset.
#[test]
fn a_checksum_list_at_a_link_pins_every_file_it_lists() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&[("SHA256SUMS", gnu())])?;
    let list = format!("{}/data/SHA256SUMS", server.url);
    let output = sandbox.run(&["--json", "add", &list])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["result"]["datasets"],
        serde_json::json!(["data"])
    );
    let work = sandbox.work();
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        format!("[datasets.data]\nref = \"list:{list}\"\n")
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!("resolved = \"list:{list}\"")),
        "{lock}"
    );
    assert_eq!(
        pinned(&lock, "data"),
        [
            format!(
                "{{ path = \"iris.csv\", size = {}, sha256 = \"{}\", url = \"{}/data/iris.csv\" }},",
                IRIS.len(),
                sha256_hex(IRIS),
                server.url
            ),
            format!(
                "{{ path = \"sub/tips.csv\", size = {}, sha256 = \"{}\", url = \"{}/data/sub/tips.csv\" }},",
                TIPS.len(),
                sha256_hex(TIPS),
                server.url
            ),
        ]
    );
    assert_eq!(fs::read(work.join("data/data/iris.csv"))?, IRIS);
    assert_eq!(fs::read(work.join("data/data/sub/tips.csv"))?, TIPS);

    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    sandbox.allow(&server.url);
    let asked = server.targets().len();
    let synced = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(clone.join("data/data/iris.csv"))?, IRIS);
    assert_eq!(fs::read(clone.join("data/data/sub/tips.csv"))?, TIPS);
    assert!(
        !server.targets()[asked..].contains(&"/data/SHA256SUMS".to_owned()),
        "a sync of a pinned list needs only data.lock: {:?}",
        server.targets()
    );

    let narrowed = sandbox.run(&["add", &list, "--as", "tips", "--select", "sub/*"])?;
    assert_eq!(
        narrowed.status.code(),
        Some(0),
        "{}",
        text(&narrowed.stderr)?
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    let tips = pinned(&lock, "tips");
    assert_eq!(tips.len(), 1, "{lock}");
    assert!(tips[0].starts_with("{ path = \"sub/tips.csv\""), "{lock}");
    Ok(())
}

/// files a list states one SHA-256 for share their bytes, which the add downloads once: its dry
/// run counts that one download, as get's does, and the add places every file.
#[test]
fn files_a_list_gives_one_sha256_are_one_download() -> Result {
    let sandbox = Sandbox::new()?;
    let sums = format!("{}  copy/iris.csv\n{}", sha256_hex(IRIS), gnu());
    let copy = String::from_utf8(IRIS.to_vec())?;
    let server = served(&[("SHA256SUMS", sums), ("copy/iris.csv", copy)])?;
    let list = format!("{}/data/SHA256SUMS", server.url);
    let got = json(&sandbox.run(&["--json", "get", &list, "--dry-run"])?)?;
    let changes = &got["result"]["changes"];
    assert_eq!(changes["to_download"]["files"], 2, "{got}");
    assert_eq!(
        changes["download"].as_array().map(Vec::len),
        Some(2),
        "{got}"
    );
    let dry = json(&sandbox.run(&["--json", "add", &list, "--dry-run"])?)?;
    let changes = &dry["result"]["changes"];
    assert_eq!(changes["to_download"]["files"], 2, "{dry}");
    // a SHA256SUMS states no sizes
    assert_eq!(changes["to_download"]["sizes_unknown"], 2, "{dry}");
    assert_eq!(
        changes["download"].as_array().map(Vec::len),
        Some(2),
        "{dry}"
    );
    let told = text(&sandbox.run(&["add", &list, "--dry-run"])?.stderr)?;
    assert_eq!(told.matches("Would fetch").count(), 2, "{told}");
    let added = sandbox.run(&["add", &list])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Downloaded 2 files"), "{stderr}");
    let folder = sandbox.work().join("data/data");
    for (path, bytes) in [
        ("copy/iris.csv", IRIS),
        ("iris.csv", IRIS),
        ("sub/tips.csv", TIPS),
    ] {
        assert_eq!(fs::read(folder.join(path))?, bytes, "{path}");
    }
    Ok(())
}

/// an MD5SUMS line checks the first download: other bytes are refused, naming the file, and the
/// project stays as it was.
#[test]
fn a_file_whose_md5_differs_from_its_list_line_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let sums = format!(
        "{}  iris.csv\n{}  sub/tips.csv\n",
        md5_hex(IRIS),
        md5_hex(IRIS)
    );
    let server = served(&[("MD5SUMS", sums)])?;
    let output = sandbox.run(&["--json", "add", &format!("{}/data/MD5SUMS", server.url)])?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch", "{document}");
    assert!(
        error.to_string().contains("tips.csv"),
        "the error names the file: {document}"
    );
    let work = sandbox.work();
    assert!(!work.join("data.toml").exists());
    assert!(!work.join("data.lock").exists());
    assert!(!work.join("data/data/sub/tips.csv").exists());
    Ok(())
}

/// a line that climbs out of the list's folder, or starts at the root of a disk, stops the add
/// before any file is asked for.
#[test]
fn a_list_line_that_leaves_its_folder_is_refused_before_any_download() -> Result {
    for (line, reason) in [
        ("../secret.csv", "it climbs out of its folder"),
        ("/etc/passwd", "it starts at the root of the disk"),
    ] {
        let sandbox = Sandbox::new()?;
        let sums = format!(
            "{}  iris.csv\n{}  {line}\n",
            sha256_hex(IRIS),
            sha256_hex(TIPS)
        );
        let server = served(&[("SHA256SUMS", sums)])?;
        let list = format!("{}/data/SHA256SUMS", server.url);
        let output = sandbox.run(&["--json", "add", &list])?;
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        assert_eq!(
            document["errors"][0]["message"],
            format!("line 2 of {list} names \"{line}\", which fetchloom refuses because {reason}")
        );
        assert_eq!(server.targets(), ["/data/SHA256SUMS"], "{line}");
        assert!(!sandbox.work().join("data.toml").exists());
    }
    Ok(())
}

/// ensembl's `CHECKSUMS` holds what unix `sum` writes, a 16-bit sum and a size for each file,
/// which shows nothing of the bytes. by its name or with `list:`, it is refused as a list of a
/// kind of sum fetchloom cannot check, never pinned as one small file, and the help names the
/// folder's own link.
#[test]
fn a_list_of_unix_sums_is_refused_and_names_its_folder() -> Result {
    let sums = "08820   582 Saccharomyces_cerevisiae.R64-1-1.113.gtf.gz\n35632     3 README\n";
    let server = served(&[("CHECKSUMS", sums.to_owned())])?;
    let link = format!("{}/data/CHECKSUMS", server.url);
    for typed in [link.clone(), format!("list:{link}")] {
        let sandbox = Sandbox::new()?;
        let output = sandbox.run(&["--json", "add", &typed])?;
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        let error = &document["errors"][0];
        assert_eq!(
            error["message"],
            format!(
                "line 1 of {link} gives a unix `sum` checksum, a kind fetchloom cannot check: it checks MD5 and SHA-256 alone"
            ),
            "{typed}"
        );
        assert_eq!(
            error["help"],
            format!(
                "add the folder's link instead, as in `fetchloom add {}/data/`, or each file's link",
                server.url
            ),
            "{typed}"
        );
        assert!(!sandbox.work().join("data.toml").exists(), "{typed}");
    }
    Ok(())
}

/// a pooch registry, a BSD list and the GNU form of the same files pin the same files.
#[test]
fn every_list_form_pins_the_same_files() -> Result {
    let sandbox = Sandbox::new()?;
    let bsd = format!(
        "SHA256 (iris.csv) = {}\nSHA256 (sub/tips.csv) = {}\n",
        sha256_hex(IRIS),
        sha256_hex(TIPS)
    );
    let pooch = format!(
        "# pooch registry\niris.csv sha256:{}\nsub/tips.csv md5:{}\n",
        sha256_hex(IRIS),
        md5_hex(TIPS)
    );
    let server = served(&[
        ("SHA256SUMS", gnu()),
        ("CHECKSUMS.bsd", bsd),
        ("registry.txt", pooch),
    ])?;
    for (list, name) in [
        (format!("{}/data/SHA256SUMS", server.url), "gnu"),
        (format!("list:{}/data/CHECKSUMS.bsd", server.url), "bsd"),
        (format!("list:{}/data/registry.txt", server.url), "pooch"),
    ] {
        let output = sandbox.run(&["add", &list, "--as", name])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let gnu = pinned(&lock, "gnu");
    assert_eq!(gnu.len(), 2, "{lock}");
    assert_eq!(pinned(&lock, "bsd"), gnu, "{lock}");
    // the registry states tips.csv's MD5, which data.lock keeps beside the SHA-256 it learned
    let pooch: Vec<String> = pinned(&lock, "pooch")
        .into_iter()
        .map(|line| line.replace(&format!(" md5 = \"{}\",", md5_hex(TIPS)), ""))
        .collect();
    assert_eq!(pooch, gnu, "{lock}");
    Ok(())
}

/// `add -i` adds the files a list on this machine names as one dataset, its `md5:` line checked.
/// data.toml names the list by its path in the project, so a clone without data.lock reads it
/// again, and `--exclude` chooses among its files as among any dataset's.
#[test]
fn a_list_in_the_project_is_one_dataset_a_clone_locks_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&[])?;
    // the hosts come from a file, so a person allows them as data.toml's would be
    sandbox.allow(&server.url);
    let work = sandbox.work();
    fs::create_dir(work.join("lists"))?;
    let list = format!(
        "# inputs\n{0}/data/iris.csv md5:{1}\n{0}/data/sub/tips.csv sha256:{2}\n",
        server.url,
        md5_hex(IRIS),
        sha256_hex(TIPS)
    );
    fs::write(work.join("lists/links.txt"), &list)?;
    let output = sandbox.run(&["--json", "add", "-i", "lists/links.txt", "--as", "flowers"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["result"]["datasets"],
        serde_json::json!(["flowers"])
    );
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.flowers]\nref = \"list:lists/links.txt\"\n"
    );
    assert_eq!(fs::read(work.join("data/flowers/iris.csv"))?, IRIS);
    assert_eq!(fs::read(work.join("data/flowers/tips.csv"))?, TIPS);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    let pins = pinned(&lock, "flowers");
    assert_eq!(pins.len(), 2, "{lock}");
    assert!(pins[0].contains(&md5_hex(IRIS)), "{lock}");

    let clone = sandbox.folder("clone")?;
    fs::create_dir(clone.join("lists"))?;
    fs::copy(work.join("data.toml"), clone.join("data.toml"))?;
    fs::write(clone.join("lists/links.txt"), &list)?;
    let synced = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("clone-cache")?)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(clone.join("data/flowers/tips.csv"))?, TIPS);
    let relocked = fs::read_to_string(clone.join("data.lock"))?;
    assert_eq!(pinned(&relocked, "flowers"), pins, "{relocked}");

    let output = sandbox.run(&[
        "add",
        "-i",
        "lists/links.txt",
        "--as",
        "tips",
        "--exclude",
        "iris.csv",
    ])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert_eq!(pinned(&lock, "tips"), pins[1..], "{lock}");
    #[cfg(target_os = "linux")]
    if let Some(screen) =
        sandbox.on_a_terminal(&["add", "-i", "lists/links.txt", "--as", "more"])?
    {
        assert!(
            screen.contains(
                "next: commit data.toml, data.lock and lists/links.txt; anyone who clones runs `fetchloom sync`"
            ),
            "{screen}"
        );
    }
    Ok(())
}

/// a list must stay where data.toml can name it for whoever clones the project: one from stdin or
/// from outside the project is refused before anything changes.
#[test]
fn a_list_outside_the_project_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let elsewhere = sandbox.folder("elsewhere")?;
    fs::write(elsewhere.join("links.txt"), "https://example.org/x.csv\n")?;
    for (list, message) in [
        (
            "../elsewhere/links.txt",
            "../elsewhere/links.txt is outside the project folder",
        ),
        (
            "-",
            "add -i reads a list from a file in the project, and stdin is none",
        ),
    ] {
        let output = sandbox.run(&["--json", "add", "-i", list])?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        assert_eq!(json(&output)?["errors"][0]["message"], message);
        assert!(!sandbox.work().join("data.toml").exists());
        assert!(!sandbox.work().join(".fetchloom").exists());
    }
    Ok(())
}

/// a list line that would leave a secret in data.lock stops the add before any request.
#[test]
fn a_list_with_a_secret_makes_no_request_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&[])?;
    sandbox.allow(&server.url);
    let work = sandbox.work();
    fs::write(
        work.join("links.txt"),
        format!(
            "{0}/data/iris.csv\n{0}/data/sub/tips.csv?token=hunter2\n",
            server.url
        ),
    )?;
    let output = sandbox.run(&["--json", "add", "-i", "links.txt"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["message"],
        "line 2 of links.txt: this link carries token, a secret that data.lock would keep and share"
    );
    assert!(!document.to_string().contains("hunter2"), "{document}");
    assert_eq!(server.targets(), Vec::<String>::new());
    assert!(!work.join("data.toml").exists());
    assert!(!work.join(".fetchloom").exists());
    Ok(())
}

/// a file of a list with nothing at its link is fixed in the list: while others arrived, the help
/// leaves it out by its path, and when none did, it says to fix each link on its line.
#[test]
fn a_file_of_a_list_with_nothing_at_its_link_is_fixed_in_the_list() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&[])?;
    sandbox.allow(&server.url);
    let work = sandbox.work();
    for (missing, help) in [
        (
            "iris.csv\nmissing.csv",
            "the other files are in the cache. to add links without it, run the add again with --exclude \"missing.csv\"",
        ),
        (
            "gone.csv\nmissing.csv",
            "check each link in a browser. if it moved, put the new link on its line of links.txt",
        ),
    ] {
        let lines: Vec<String> = missing
            .lines()
            .map(|file| format!("{}/data/{file}", server.url))
            .collect();
        fs::write(work.join("links.txt"), lines.join("\n"))?;
        let output = sandbox.run(&["--json", "add", "-i", "links.txt"])?;
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        assert_eq!(document["errors"][0]["help"], help, "{document}");
        assert!(!work.join("data.toml").exists());
    }
    Ok(())
}

/// `get -i` reads its list with the same reader, and a line's MD5 is checked as the bytes arrive:
/// other bytes keep their file out, naming the line.
#[test]
fn get_checks_the_md5_a_list_line_states() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&[])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("links.txt"),
        format!(
            "{0}/data/iris.csv md5:{1}\n{1}  {0}/data/sub/tips.csv\n",
            server.url,
            md5_hex(IRIS)
        ),
    )?;
    let output = sandbox.run(&["--json", "get", "-i", "links.txt", "--dir", "out"])?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let items = &document["result"]["items"];
    assert_eq!(items[0]["status"], "ok", "{items}");
    assert_eq!(items[1]["error"]["kind"], "integrity.mismatch", "{items}");
    assert_eq!(
        items[1]["error"]["message"],
        "tips.csv does not match the MD5 on line 2 of links.txt"
    );
    let out = sandbox.work().join("out");
    assert_eq!(fs::read(out.join("iris").join("iris.csv"))?, IRIS);
    assert!(
        !out.join("tips").exists(),
        "a wrong hash keeps its file out"
    );
    Ok(())
}

/// `get` of a checksum list's link fetches every file it lists into one folder, named as `add`
/// names its dataset, after the list's folder.
#[test]
fn get_of_a_checksum_list_fetches_every_file_it_lists() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&[("SHA256SUMS", gnu())])?;
    let output = sandbox.run(&["get", &format!("{}/data/SHA256SUMS", server.url)])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let folder = sandbox.work().join("data");
    assert_eq!(fs::read(folder.join("iris.csv"))?, IRIS);
    assert_eq!(fs::read(folder.join("sub").join("tips.csv"))?, TIPS);
    Ok(())
}

/// comment lines that take a list past `bytes`, each 32 KiB, so a test of a long list needs no
/// thousands of files.
fn padding(bytes: usize) -> String {
    let comment = format!("# {}\n", "-".repeat(32 << 10));
    comment.repeat(bytes / comment.len() + 1)
}

/// a list is as long as the files it names, not its bytes: one past 64 MiB pins the files
/// before and after its long middle, at a link and read from stdin by `get -i -`.
#[test]
fn a_list_past_64_mib_is_read_to_its_last_line() -> Result {
    use std::io::Write as _;
    use std::process::Stdio;

    let sandbox = Sandbox::new()?;
    let long = padding(64 << 20);
    let sums = format!(
        "{}  iris.csv\n{long}{}  sub/tips.csv\n",
        sha256_hex(IRIS),
        sha256_hex(TIPS)
    );
    let server = served(&[("SHA256SUMS", sums)])?;
    let output = sandbox.run(&["add", &format!("{}/data/SHA256SUMS", server.url)])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    assert_eq!(fs::read(work.join("data/data/iris.csv"))?, IRIS);
    assert_eq!(fs::read(work.join("data/data/sub/tips.csv"))?, TIPS);

    sandbox.allow(&server.url);
    let links = format!(
        "{0}/data/iris.csv {1}\n{long}{0}/data/sub/tips.csv {2}\n",
        server.url,
        sha256_hex(IRIS),
        sha256_hex(TIPS)
    );
    let mut child = sandbox
        .command(&["get", "-i", "-", "--dir", "out"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().ok_or("stdin was not piped")?;
    let feeding = std::thread::spawn(move || input.write_all(links.as_bytes()));
    let output = child.wait_with_output()?;
    let fed = feeding
        .join()
        .map_err(|_| "the thread feeding stdin panicked")?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    fed?;
    let out = work.join("out");
    assert_eq!(fs::read(out.join("iris").join("iris.csv"))?, IRIS);
    assert_eq!(fs::read(out.join("tips").join("tips.csv"))?, TIPS);
    Ok(())
}

/// one rule wherever a list is read: past the most files a dataset may hold in this run, a list
/// at a link, in the project and for `get -i` is refused before any file is asked for, naming
/// the list, the limit and what to do.
#[test]
fn a_list_past_the_most_files_of_a_dataset_is_refused_by_name() -> Result {
    let sandbox = Sandbox::new()?;
    let three = format!("{}{}  wine.csv\n", gnu(), sha256_hex(b"wine"));
    let server = served(&[("SHA256SUMS", three)])?;
    sandbox.allow(&server.url);
    let link = format!("{}/data/SHA256SUMS", server.url);
    fs::create_dir(sandbox.work().join("lists"))?;
    fs::write(
        sandbox.work().join("lists/links.txt"),
        format!(
            "{0}/data/iris.csv\n{0}/data/sub/tips.csv\n{0}/data/wine.csv\n",
            server.url
        ),
    )?;
    let raise = "or raise the limit with `fetchloom settings set most_files <count>` or FETCHLOOM_MOST_FILES; each listed file takes about 2 KiB of memory";
    for (args, named, instead) in [
        (
            vec!["add", link.as_str()],
            link.as_str(),
            "use the list of a narrower folder if the site has one",
        ),
        (
            vec!["add", "-i", "lists/links.txt"],
            "lists/links.txt",
            "split it into lists of fewer files, each a dataset of its own",
        ),
        (
            vec!["get", "-i", "lists/links.txt", "--dir", "out"],
            "lists/links.txt",
            "split it into lists of fewer files, each a dataset of its own",
        ),
    ] {
        let output = sandbox
            .command(&[&["--json"][..], &args].concat())
            .env("FETCHLOOM_MOST_FILES", "2")
            .output()?;
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        let error = &document["errors"][0];
        assert_eq!(error["kind"], "source.invalid", "{args:?}");
        assert_eq!(
            error["message"],
            format!("{named} lists more than 2 files, the most this run lists for one dataset"),
            "{args:?}"
        );
        assert_eq!(error["help"], format!("{instead}, {raise}"), "{args:?}");
    }
    let asked = server.targets();
    assert!(
        asked.iter().all(|target| target == "/data/SHA256SUMS"),
        "{asked:?}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

/// a list whose connection breaks partway is asked for again and read from its start, so the
/// files it pins are those of its whole text, each once.
#[test]
fn a_list_cut_off_partway_is_read_again_from_its_start() -> Result {
    let sandbox = Sandbox::new()?;
    let sums = gnu();
    let server = served(&[("SHA256SUMS", sums.clone())])?;
    let length = sums.len().to_string();
    server.turns(
        "/data/SHA256SUMS",
        vec![crate::support::reply(
            "200 OK",
            &[("Content-Length", &length)],
            &sums.as_bytes()[..sums.len() - 10],
        )],
    );
    let list = format!("{}/data/SHA256SUMS", server.url);
    let output = sandbox.run(&["add", &list])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let pinned = pinned(&lock, "data");
    assert_eq!(pinned.len(), 2, "{lock}");
    assert!(pinned[0].starts_with("{ path = \"iris.csv\""), "{lock}");
    assert!(pinned[1].starts_with("{ path = \"sub/tips.csv\""), "{lock}");
    let asked = server.targets();
    assert_eq!(
        asked
            .iter()
            .filter(|target| *target == "/data/SHA256SUMS")
            .count(),
        2,
        "{asked:?}"
    );
    Ok(())
}

/// a list that is not there, as a typo in its name gives, is a usage error with help, as a path
/// `add` is given that names nothing is, and changes nothing.
#[test]
fn a_list_that_is_not_there_is_a_usage_error() -> Result {
    let sandbox = Sandbox::new()?;
    for command in ["add", "get"] {
        let output = sandbox.run(&["--json", command, "-i", "linsk.txt"])?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(2), "{command}: {document}");
        assert_eq!(
            document["errors"][0]["kind"], "usage",
            "{command}: {document}"
        );
        assert!(
            document["errors"][0]["help"].is_string(),
            "{command}: {document}"
        );
        assert!(!sandbox.work().join("data.toml").exists());
        assert!(!sandbox.work().join(".fetchloom").exists());
    }
    Ok(())
}
