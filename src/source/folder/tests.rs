use super::*;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const DATA: &str = "https://example.org/data/";

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

/// a listing as nginx writes one for `folder`, linking each of `entries`.
fn nginx(folder: &str, entries: &[&str]) -> Vec<u8> {
    let path = url(folder).path().to_owned();
    let rows = entries
        .iter()
        .map(|entry| format!("<a href=\"{entry}\">{entry}</a>   16-Nov-2026 09:00   -\n"))
        .collect::<Vec<_>>()
        .concat();
    format!("<html>\n<head><title>Index of {path}</title></head>\n<body>\n<h1>Index of {path}</h1><hr><pre><a href=\"../\">../</a>\n{rows}</pre><hr></body>\n</html>\n").into_bytes()
}

fn names(index: &Index) -> Vec<(&str, bool)> {
    index
        .entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.folder))
        .collect()
}

fn read_at(html: &str, folder: &str) -> Result<Index, Unread> {
    read(html, &url(folder), &url(folder))
}

fn folder(link: &str) -> Folder {
    Folder::from_link(&url(link)).unwrap()
}

fn paths(listing: &Listing) -> Vec<&str> {
    listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

/// the folder at [`DATA`] with iris.csv in it and tips.csv in its folder sub, as nginx lists it.
fn nested() -> Recorded {
    Recorded::default()
        .fixture(DATA, "folder/data.html")
        .fixture(&format!("{DATA}sub/"), "folder/data-sub.html")
}

/// the listings apache, nginx, lighttpd, python's http.server and NCBI's file server sent, as
/// recorded, read as the files and folders they show, past the links to the folder above, the
/// sort links and a readme's links lighttpd shows below its listing.
#[test]
fn every_servers_listing_reads_as_the_entries_it_shows() {
    let gtf = "Saccharomyces_cerevisiae.R64-1-1.113";
    for (fixture, folder, entries) in [
        (
            "folder/apache.html",
            "https://ftp.ensembl.org/pub/release-113/gtf/saccharomyces_cerevisiae/",
            vec![
                ("CHECKSUMS".to_owned(), false),
                ("README".to_owned(), false),
                (format!("{gtf}.abinitio.gtf.gz"), false),
                (format!("{gtf}.gtf.gz"), false),
            ],
        ),
        (
            "folder/nginx.html",
            "https://opendata.dwd.de/climate_environment/CDC/observations_germany/climate/annual/kl/",
            vec![
                ("historical".to_owned(), true),
                ("recent".to_owned(), true),
                ("timeseries_overview".to_owned(), true),
            ],
        ),
        (
            "folder/lighttpd.html",
            "https://download.lighttpd.net/lighttpd/",
            vec![
                ("security".to_owned(), true),
                ("snapshots-2.0.x".to_owned(), true),
                ("snapshots-1.4.x".to_owned(), true),
                ("releases-1.4.x".to_owned(), true),
                ("README.txt".to_owned(), false),
            ],
        ),
        (
            "folder/python.html",
            "http://127.0.0.1:19340/data/",
            vec![
                ("a&b c.csv".to_owned(), false),
                ("iris.csv".to_owned(), false),
                ("sub".to_owned(), true),
            ],
        ),
        (
            "geo/series.html",
            "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/",
            vec![
                ("matrix".to_owned(), true),
                ("miniml".to_owned(), true),
                ("soft".to_owned(), true),
                ("suppl".to_owned(), true),
            ],
        ),
    ] {
        let html = String::from_utf8(fixture_bytes(fixture)).unwrap();
        let index = read_at(&html, folder).unwrap();
        let read: Vec<(String, bool)> = index
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.folder))
            .collect();
        assert_eq!(read, entries, "{fixture}");
        assert_eq!(index.elsewhere, Vec::<String>::new(), "{fixture}");
    }
    let python = String::from_utf8(fixture_bytes("folder/python.html")).unwrap();
    let index = read_at(&python, "http://127.0.0.1:19340/data/").unwrap();
    assert_eq!(
        index.entries[0].link.as_str(),
        "http://127.0.0.1:19340/data/a%26b%20c.csv"
    );
    assert_eq!(
        index.entries[2].link.as_str(),
        "http://127.0.0.1:19340/data/sub/"
    );
}

/// a page no server made as a listing is none, though it links files below its folder: a site's
/// own page, one that keeps a listing's title but has no list, one without the title, and one
/// cut short in its list.
#[test]
fn a_page_no_server_made_as_a_listing_is_none() {
    let listing = String::from_utf8(nginx(DATA, &["iris.csv"])).unwrap();
    let cut = &listing[..listing.find("</pre>").unwrap()];
    for page in [
        "<!DOCTYPE html><html><head><title>Our lab</title></head><body><ul><li><a href=\"data.csv\">our data</a></li></ul></body></html>",
        "<html><head><title>Index of /data/</title></head><body><a href=\"data.csv\">data.csv</a></body></html>",
        "<html><body><pre><a href=\"../\">../</a>\n<a href=\"data.csv\">data.csv</a></pre></body></html>",
        cut,
        "",
    ] {
        assert_eq!(read_at(page, DATA), Err(Unread::NoListing), "{page}");
    }
}

/// a listing's entries are its links into its folder, each once. the link to the folder above,
/// a column's sort link with a query, an anchor and every link outside its list go unsaid; any
/// other link, to another host, out of the folder once decoded, or deeper than an entry, is told
/// as it leads.
#[test]
fn only_links_into_the_folder_are_entries() {
    let page = r##"<html><head><title>Index of /data/</title></head><body>
<a href="/about.html">about</a>
<pre><a href="../">../</a> <a href="?C=M;O=A">Last modified</a> <a href="#top">top</a>
<A HREF='iris.csv'><img src="/icons/text.gif"></A> <a class="x" href=iris.csv>iris.csv</a>
<a href="sub/">sub/</a> <a href="https://other.example/x.csv">x.csv</a>
<a href="%2e%2e/secret.csv">secret.csv</a> <a href="/data/sub/deeper.csv">deeper.csv</a>
<a href="..">..</a> <a href="">here</a> <a href="C:x.csv">C:x.csv</a>
</pre><a href="https://other.example/footer.html">footer</a></body></html>"##;
    let index = read_at(page, DATA).unwrap();
    assert_eq!(names(&index), [("iris.csv", false), ("sub", true)]);
    assert_eq!(index.entries[0].link.as_str(), format!("{DATA}iris.csv"));
    assert_eq!(
        index.elsewhere,
        [
            "https://other.example/x.csv",
            "https://example.org/secret.csv",
            "https://example.org/data/sub/deeper.csv",
            "https://example.org/",
            "https://example.org/data/",
            "c:x.csv",
        ]
    );
}

/// a link to the folder's own path on another host, scheme or port is no entry: a listing could
/// otherwise have fetchloom pin files another server gives under the folder's name.
#[test]
fn the_folders_path_on_another_origin_is_no_entry() {
    let page = r#"<html><head><title>Index of /data/</title></head><body><pre>
<a href="../">../</a> <a href="iris.csv">iris.csv</a>
<a href="https://other.example/data/x.csv">x.csv</a>
<a href="http://example.org/data/y.csv">y.csv</a>
<a href="https://example.org:8443/data/z.csv">z.csv</a>
<a href="//other.example/data/w.csv">w.csv</a>
</pre></body></html>"#;
    let index = read_at(page, DATA).unwrap();
    assert_eq!(names(&index), [("iris.csv", false)]);
    assert_eq!(
        index.elsewhere,
        [
            "https://other.example/data/x.csv",
            "http://example.org/data/y.csv",
            "https://example.org:8443/data/z.csv",
            "https://other.example/data/w.csv",
        ]
    );
}

/// an entry's name with a separator, a control character or a step up once its percent escapes
/// are undone, or bytes that are no text, is no name a file may have.
#[test]
fn a_name_no_file_may_have_stops_the_listing() {
    for (href, name) in [
        ("a%2Fb.csv", "a/b.csv"),
        ("a%5Cb.csv", "a\\b.csv"),
        ("a%0Ab.csv", "a\nb.csv"),
        ("hostile%E2%80%AEvsc.exe", "hostile\u{202e}vsc.exe"),
        ("..%2Fx.csv", "../x.csv"),
        ("%FF.csv", "%FF.csv"),
    ] {
        let page = String::from_utf8(nginx(DATA, &[href])).unwrap();
        assert_eq!(
            read_at(&page, DATA),
            Err(Unread::Name(name.to_owned())),
            "{href}"
        );
    }
}

/// a listing writes `&`, `<`, `>` and quotes in a link as html does, so `a&amp;b.csv` links the
/// file `a&b.csv`, and `&#63;` is a `?` like any other, which no entry's link has.
#[test]
fn a_link_is_read_with_its_character_references_undone() {
    let page = String::from_utf8(nginx(
        DATA,
        &["a&amp;b.csv", "&lt;c&gt;&#39;d&#x27;.csv", "e&#63;f.csv"],
    ))
    .unwrap();
    let index = read_at(&page, DATA).unwrap();
    assert_eq!(names(&index), [("a&b.csv", false), ("<c>'d'.csv", false)]);
    assert_eq!(index.entries[0].link.as_str(), format!("{DATA}a&b.csv"));
    assert_eq!(
        index.entries[1].link.as_str(),
        format!("{DATA}%3Cc%3E'd'.csv")
    );
}

/// a folder's files are those of its listing and of the listings of the folders inside it, by
/// their paths below it, one request a folder.
#[test]
fn a_folder_lists_every_file_below_it() {
    let recorded = nested();
    let listing = block_on(folder(DATA).resolve(&recorded, &[])).unwrap();
    assert_eq!(listing.resolved, Reference::Folder(folder(DATA)));
    assert_eq!(listing.access, Access::Unknown);
    assert_eq!(paths(&listing), ["iris.csv", "sub/tips.csv"]);
    assert_eq!(listing.files[1].url.as_str(), format!("{DATA}sub/tips.csv"));
    assert!(
        listing
            .files
            .iter()
            .all(|file| file.sha256.is_none() && file.size.is_none() && !file.named_by_server)
    );
    assert_eq!(listing.warnings, Vec::<String>::new());
    assert_eq!(recorded.asked(), [DATA.to_owned(), format!("{DATA}sub/")]);
}

/// patterns that each say where their files lie list only the folders they reach, and the
/// listing holds the files of those folders alone; one that may match anywhere lists them all.
#[test]
fn a_pattern_lists_only_the_folders_it_reaches() {
    let sub = "https://example.org/data/sub/";
    for (pattern, asked, files, within) in [
        ("*.csv", &[DATA][..], &["iris.csv"][..], &[""][..]),
        (
            "sub/*",
            &[DATA, sub][..],
            &["sub/tips.csv"][..],
            &["sub"][..],
        ),
        (
            "**/tips.csv",
            &[DATA, sub][..],
            &["iris.csv", "sub/tips.csv"][..],
            &[][..],
        ),
    ] {
        let recorded = nested();
        let select = [Glob::new(pattern).unwrap()];
        let listing = block_on(folder(DATA).resolve(&recorded, &select)).unwrap();
        assert_eq!(recorded.asked(), asked, "{pattern}");
        assert_eq!(paths(&listing), files, "{pattern}");
        assert_eq!(listing.within, within, "{pattern}");
    }
}

/// a folder whose listing names what a folder it lies in names, as a link on the server back
/// to a folder above makes, would list without end, so it is refused at once.
#[test]
fn a_folder_that_lists_a_folder_above_it_again_is_refused() {
    let looped = format!("{DATA}loop/");
    let recorded = Recorded::default()
        .on(DATA, 200, nginx(DATA, &["a.csv", "loop/"]))
        .on(&looped, 200, nginx(&looped, &["a.csv", "loop/"]));
    let error = block_on(folder(DATA).resolve(&recorded, &[])).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(
        error.message,
        format!(
            "{DATA} never ends: its folder loop lists what the folder itself lists, as a link on the server back to a folder above does"
        )
    );
    assert_eq!(recorded.asked().len(), 2);
}

/// a listing that comes from outside its folder after a redirect is refused, and so is a folder
/// with more files and folders than a run lists.
#[test]
fn a_redirect_out_of_the_folder_and_too_many_entries_are_refused() {
    let elsewhere = "https://example.org/elsewhere/";
    let recorded = nested().redirected(&format!("{DATA}sub/"), elsewhere);
    let error = block_on(folder(DATA).resolve(&recorded, &[])).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(
        error.message,
        format!("{DATA}sub/ sends fetchloom on to {elsewhere}, outside the folder")
    );
    let error = block_on(folder(DATA).resolve(&nested().most(1), &[])).unwrap_err();
    assert!(
        error.message.contains("lists more than 1 file"),
        "{}",
        error.message
    );
    assert!(error.help.unwrap_or_default().contains("most_files"));
}

/// a link ending in `/` that answers with a file rather than a page is that file's plain link.
#[test]
fn a_folder_link_that_answers_with_a_file_is_that_file() {
    let recorded = Recorded::default().on(DATA, 200, b"sepal,petal\n5.1,1.4\n".to_vec());
    let listing = block_on(folder(DATA).resolve(&recorded, &[])).unwrap();
    assert_eq!(listing.resolved, Reference::Url(url(DATA)));
    assert_eq!(listing.files.len(), 1);
    assert!(listing.files[0].named_by_server);
}

/// what a folder's link answers with other than a listing ends in an error that says why: a
/// page that is none, a folder inside it whose page is none or not there, nothing at the link,
/// a refusal, an empty folder and a name no file may have.
#[test]
fn answers_that_are_no_listing_end_in_what_to_do() {
    let sub = format!("{DATA}sub/");
    let page = b"<!DOCTYPE html><html><head><title>Our lab</title></head><body></body></html>";
    for (recorded, kind, message) in [
        (
            Recorded::default().on(DATA, 200, page.to_vec()),
            Kind::SourceInvalid,
            format!(
                "{DATA} answered with a web page, not a file or a folder listing fetchloom reads"
            ),
        ),
        (
            nested().on(&sub, 200, page.to_vec()),
            Kind::SourceInvalid,
            format!("{DATA} lists the folder sub, whose page is no folder listing fetchloom reads"),
        ),
        (
            nested().on(&sub, 404, Vec::new()),
            Kind::SourceInvalid,
            format!("{DATA} lists the folder sub, and example.org has nothing there (404)"),
        ),
        (
            Recorded::default().on(DATA, 404, Vec::new()),
            Kind::ReferenceUnresolved,
            format!("example.org has nothing at {DATA} (404)"),
        ),
        (
            Recorded::default().on(DATA, 403, Vec::new()),
            Kind::AuthRequired,
            format!("example.org refused access to {DATA}"),
        ),
        (
            Recorded::default().on(DATA, 200, nginx(DATA, &[])),
            Kind::ReferenceUnresolved,
            format!("{DATA} lists no files"),
        ),
        (
            Recorded::default().on(DATA, 200, nginx(DATA, &["a%2Fb.csv"])),
            Kind::SourceInvalid,
            format!("{DATA} lists \"a/b.csv\", which fetchloom refuses because its name holds a /"),
        ),
    ] {
        let error = block_on(folder(DATA).resolve(&recorded, &[])).unwrap_err();
        assert_eq!((error.kind, error.message), (kind, message.clone()));
        assert!(error.help.is_some(), "{message}");
    }
}

/// a link whose path ends in `/`, without a query or a fragment, names a folder, which keeps its
/// link in data.toml; one that names a checksum list, a known source's folder or a share's page
/// does not, and a dataset that keeps the page a folder's link answers with reads it as the plain
/// link it is.
#[test]
fn a_link_ending_in_a_slash_names_a_folder() {
    for (text, is_folder) in [
        (DATA, true),
        ("https://example.org/", true),
        ("http://127.0.0.1:19340/data/", true),
        ("https://example.org/data", false),
        ("https://example.org/data/?C=M;O=A", false),
        ("https://example.org/data/#top", false),
        ("https://example.org/data/SHA256SUMS", false),
        (
            "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/",
            false,
        ),
        (
            "https://lab.example.org/owncloud/index.php/s/XyZ789/",
            false,
        ),
        ("https://lab.app.box.com/folder/123/", false),
    ] {
        let parsed = Reference::parse(text).unwrap();
        assert_eq!(
            matches!(parsed.reference, Reference::Folder(_)),
            is_folder,
            "{text}"
        );
    }
    let named = Reference::parse("https://example.org/my%20data/").unwrap();
    assert_eq!(
        named.reference.to_string(),
        "https://example.org/my%20data/"
    );
    assert_eq!(named.reference.naming(), "example.org/my data/");
    assert_eq!(
        named.reference.taking_page(true),
        Reference::Url(url("https://example.org/my%20data/"))
    );
}
