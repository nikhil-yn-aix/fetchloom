use super::*;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const DATA: &str = "https://example.org/data/";

/// the folder `folder/ncei.html` lists, as ncei's server sent it.
const NCEI: &str = "https://www.ncei.noaa.gov/pub/data/ghcn/daily/";

/// where ncei's global-hourly folder sends a request: its bucket's page on S3.
const S3_PAGE: &str = "https://noaa-global-hourly-pds.s3.amazonaws.com/index.html";

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

/// how a recorded network says the listing at `link` was asked for: as a web page.
fn listed(link: &str) -> String {
    format!("{link} accept: text/html, */*;q=0.1")
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

/// reads the recorded listing `fixture` of `folder`, which must show `entries`, each a name and
/// whether it is a folder, and no link elsewhere.
fn reads_as(fixture: &str, folder: &str, entries: &[(&str, bool)]) -> Index {
    let html = String::from_utf8(fixture_bytes(fixture)).unwrap();
    let index = read_at(&html, folder).unwrap();
    assert_eq!(names(&index), entries, "{fixture}");
    assert_eq!(index.elsewhere, Vec::<String>::new(), "{fixture}");
    index
}

/// the listings apache, nginx, lighttpd, python's http.server and NCBI's file server sent, as
/// recorded, read as the files and folders they show, past the links to the folder above, the
/// sort links and a readme's links lighttpd shows below its listing; ncei's links a folder
/// without its `/` and the folder above as `..`, and lp daac's shows a notice above it.
#[test]
fn every_servers_listing_reads_as_the_entries_it_shows() {
    let gtf = "Saccharomyces_cerevisiae.R64-1-1.113";
    let (abinitio, annotation) = (format!("{gtf}.abinitio.gtf.gz"), format!("{gtf}.gtf.gz"));
    for (fixture, folder, entries) in [
        (
            "folder/apache.html",
            "https://ftp.ensembl.org/pub/release-113/gtf/saccharomyces_cerevisiae/",
            &[
                ("CHECKSUMS", false),
                ("README", false),
                (abinitio.as_str(), false),
                (annotation.as_str(), false),
            ][..],
        ),
        (
            "folder/nginx.html",
            "https://opendata.dwd.de/climate_environment/CDC/observations_germany/climate/annual/kl/",
            &[
                ("historical", true),
                ("recent", true),
                ("timeseries_overview", true),
            ],
        ),
        (
            "folder/lighttpd.html",
            "https://download.lighttpd.net/lighttpd/",
            &[
                ("security", true),
                ("snapshots-2.0.x", true),
                ("snapshots-1.4.x", true),
                ("releases-1.4.x", true),
                ("README.txt", false),
            ],
        ),
        (
            "folder/python.html",
            "http://127.0.0.1:19340/data/",
            &[("a&b c.csv", false), ("iris.csv", false), ("sub", true)],
        ),
        (
            "geo/series.html",
            "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/",
            &[
                ("matrix", true),
                ("miniml", true),
                ("soft", true),
                ("suppl", true),
            ],
        ),
        (
            "folder/ncei.html",
            NCEI,
            &[
                ("all", true),
                ("papers", true),
                ("ghcnd-countries.txt", false),
                ("ghcnd_all.tar.gz", false),
            ],
        ),
        (
            "folder/lpdaac.html",
            "https://e4ftl01.cr.usgs.gov/",
            &[("ASTT", true)],
        ),
    ] {
        reads_as(fixture, folder, entries);
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
    let ncei = String::from_utf8(fixture_bytes("folder/ncei.html")).unwrap();
    let index = read_at(&ncei, NCEI).unwrap();
    assert_eq!(index.entries[0].link.as_str(), format!("{NCEI}all/"));
}

/// apache's listings a site titled as it likes, psl's with no title and ndbc's with its own, and
/// those rclone and caddy title with the folder's path or its name, read as the entries they show.
#[test]
fn listings_titled_otherwise_read_as_the_entries_they_show() {
    let sub = [
        ("deeper", true),
        ("iris.csv", false),
        ("read me.txt", false),
    ];
    for (fixture, folder, entries) in [
        (
            "folder/psl.html",
            "https://downloads.psl.noaa.gov/Datasets/ncep.reanalysis/surface/",
            &[
                ("air.sig995.1948.nc", false),
                ("air.sig995.1949.nc", false),
                ("air.sig995.1950.nc", false),
            ][..],
        ),
        (
            "folder/ndbc.html",
            "https://www.ndbc.noaa.gov/data/realtime2/",
            &[
                ("32ST0.ocean", false),
                ("32ST0.srad", false),
                ("32ST0.txt", false),
            ],
        ),
        ("folder/rclone.html", "http://127.0.0.1:19340/sub/", &sub),
        ("folder/caddy.html", "http://127.0.0.1:19340/sub/", &sub),
    ] {
        reads_as(fixture, folder, entries);
    }
}

/// the file sizes a recorded listing of `folder` states, by name.
fn sizes(fixture: &str, folder: &str) -> Vec<(String, Option<u64>)> {
    let html = String::from_utf8(fixture_bytes(fixture)).unwrap();
    file_sizes(&read_at(&html, folder).unwrap())
}

fn file_sizes(index: &Index) -> Vec<(String, Option<u64>)> {
    index
        .entries
        .iter()
        .filter(|entry| !entry.folder)
        .map(|entry| (entry.name.clone(), entry.size))
        .collect()
}

/// a row states its file's size in bytes, as `621`, as the last word of a list's row or in a
/// table's column headed "Size". one rounded, as `9.3K` or caddy's `20 B`, states none in bytes,
/// and nor does any row of a list whose first line heads a description, as apache's does, or
/// where a row ends in words, or of a table with no size column.
#[test]
fn a_row_states_its_files_size_in_bytes_alone() {
    let owned = |sizes: &[(&str, Option<u64>)]| -> Vec<(String, Option<u64>)> {
        sizes
            .iter()
            .map(|(name, size)| ((*name).to_owned(), *size))
            .collect()
    };
    let gtf = "Saccharomyces_cerevisiae.R64-1-1.113";
    let (abinitio, annotation) = (format!("{gtf}.abinitio.gtf.gz"), format!("{gtf}.gtf.gz"));
    let sub = "http://127.0.0.1:19340/sub/";
    for (fixture, folder, stated) in [
        ("folder/data.html", DATA, owned(&[("iris.csv", Some(20))])),
        (
            "folder/apache.html",
            "https://ftp.ensembl.org/pub/release-113/gtf/saccharomyces_cerevisiae/",
            owned(&[
                ("CHECKSUMS", Some(131)),
                ("README", None),
                (abinitio.as_str(), Some(116)),
                (annotation.as_str(), None),
            ]),
        ),
        (
            "folder/ncei.html",
            NCEI,
            owned(&[
                ("ghcnd-countries.txt", Some(3659)),
                ("ghcnd_all.tar.gz", Some(3_825_956_509)),
            ]),
        ),
        (
            "folder/rclone.html",
            sub,
            owned(&[("iris.csv", Some(20)), ("read me.txt", Some(6))]),
        ),
        (
            "folder/caddy.html",
            sub,
            owned(&[("iris.csv", None), ("read me.txt", None)]),
        ),
        (
            "folder/ndbc.html",
            "https://www.ndbc.noaa.gov/data/realtime2/",
            owned(&[
                ("32ST0.ocean", None),
                ("32ST0.srad", None),
                ("32ST0.txt", None),
            ]),
        ),
    ] {
        assert_eq!(sizes(fixture, folder), stated, "{fixture}");
    }
    let described = "<html><head><title>Index of /data/</title></head><body><pre>\
Name  Last modified  Size  Description<hr><a href=\"../\">../</a>
<a href=\"a.csv\">a.csv</a>  2026-11-16 09:00   20
<a href=\"b.csv\">b.csv</a>  2026-11-16 09:00  1.3K  rows from 2020
</pre></body></html>";
    let worded = "<html><head><title>Index of /data/</title></head><body><pre>\
<a href=\"../\">../</a>
<a href=\"a.csv\">a.csv</a>  2026-11-16 09:00   20
<a href=\"b.csv\">b.csv</a>  2026-11-16 09:00  1.3K  our rows
</pre></body></html>";
    let unheaded = "<html><head><title>Index of /data/</title></head><body><table>\
<tr><th>Name</th><th>Description</th></tr>\
<tr><td><a href=\"a.csv\">a.csv</a></td><td>2020</td></tr></table></body></html>";
    for list in [described, worded] {
        assert_eq!(
            file_sizes(&read_at(list, DATA).unwrap()),
            owned(&[("a.csv", None), ("b.csv", None)]),
            "{list}"
        );
    }
    assert_eq!(
        file_sizes(&read_at(unheaded, DATA).unwrap()),
        owned(&[("a.csv", None)])
    );
}

/// a row that rounds its file's size to a unit, as apache's `9.3K`, nginx's `2.4G` or caddy's
/// `20 B`, states about as many bytes, a K counting 1024; one in bytes states no about, and nor
/// does any row of a list whose rows may end in descriptions.
#[test]
fn a_rounded_size_is_about_its_bytes() {
    let abouts = |index: &Index| -> Vec<(String, Option<u64>)> {
        index
            .entries
            .iter()
            .filter(|entry| !entry.folder)
            .map(|entry| (entry.name.clone(), entry.about))
            .collect()
    };
    let owned = |abouts: &[(&str, Option<u64>)]| -> Vec<(String, Option<u64>)> {
        abouts
            .iter()
            .map(|(name, about)| ((*name).to_owned(), *about))
            .collect()
    };
    let read_fixture = |fixture: &str, folder: &str| {
        abouts(&read_at(&String::from_utf8(fixture_bytes(fixture)).unwrap(), folder).unwrap())
    };
    let gtf = "Saccharomyces_cerevisiae.R64-1-1.113";
    assert_eq!(
        read_fixture(
            "folder/apache.html",
            "https://ftp.ensembl.org/pub/release-113/gtf/saccharomyces_cerevisiae/"
        ),
        owned(&[
            ("CHECKSUMS", None),
            ("README", Some(9 * 1024 + 307)),
            (&format!("{gtf}.abinitio.gtf.gz"), None),
            (&format!("{gtf}.gtf.gz"), Some(581 * 1024)),
        ])
    );
    assert_eq!(
        read_fixture("folder/caddy.html", "http://127.0.0.1:19340/sub/"),
        owned(&[("iris.csv", Some(20)), ("read me.txt", Some(6))])
    );
    assert_eq!(
        read_fixture(
            "folder/ndbc.html",
            "https://www.ndbc.noaa.gov/data/realtime2/"
        ),
        owned(&[
            ("32ST0.ocean", Some(82 * 1024)),
            ("32ST0.srad", Some(39 * 1024)),
            ("32ST0.txt", Some(95 * 1024)),
        ])
    );
    let rounded = "<html><head><title>Index of /data/</title></head><body><pre>\
<a href=\"../\">../</a>
<a href=\"a.gz\">a.gz</a>  13-May-2024 16:01  1.3M
<a href=\"b.gz\">b.gz</a>  13-May-2024 16:01  2.4G
<a href=\"c.gz\">c.gz</a>  13-May-2024 16:01  1.25k
<a href=\"d.txt\">d.txt</a>  13-May-2024 16:01  771
</pre></body></html>";
    let index = read_at(rounded, DATA).unwrap();
    assert_eq!(
        abouts(&index),
        owned(&[
            ("a.gz", Some((1 << 20) + 3 * (1 << 20) / 10)),
            ("b.gz", Some((2 << 30) + 4 * (1 << 30) / 10)),
            ("c.gz", Some(1024 + 25 * 1024 / 100)),
            ("d.txt", None),
        ])
    );
    assert_eq!(index.entries[3].size, Some(771));
    let described = "<html><head><title>Index of /data/</title></head><body><pre>\
Name  Last modified  Size  Description<hr><a href=\"../\">../</a>
<a href=\"b.csv\">b.csv</a>  2026-11-16 09:00  1.3K  rows from 2020
</pre></body></html>";
    assert_eq!(
        abouts(&read_at(described, DATA).unwrap()),
        owned(&[("b.csv", None)])
    );
}

/// a folder whose listing links a folder inside it without its `/`, under a name ending in `/`,
/// as ncei's do, lists that folder at its link with the `/`, never asking for it as a file.
#[test]
fn a_folder_linked_without_its_slash_is_listed_as_a_folder() {
    let sub = format!("{DATA}sub/");
    let top = "<html><head><title>Index of /data</title></head><body><table>\
<tr><td><a href=\"..\">Parent Directory</a></td></tr>\
<tr><td><a href=\"iris.csv\">iris.csv</a></td></tr>\
<tr><td><a href=\"sub\">sub/</a></td></tr></table></body></html>";
    let recorded = Recorded::default()
        .on(DATA, 200, top.as_bytes().to_vec())
        .on(&sub, 200, nginx(&sub, &["tips.csv"]));
    let listing = block_on(folder(DATA).resolve(&recorded, &[])).unwrap();
    assert_eq!(paths(&listing), ["iris.csv", "sub/tips.csv"]);
    assert_eq!(listing.warnings, Vec::<String>::new());
    assert_eq!(recorded.asked(), [listed(DATA), listed(&sub)]);
}

/// the listing is the list that holds the folder's entries, whatever lists a page shows above
/// it: a notice whose links lead elsewhere, or a trail of links to the folders above.
#[test]
fn the_listing_is_the_list_that_holds_the_entries() {
    let sub = "https://example.org/data/sub/";
    let page = r#"<html><head><title>Index of /data/sub/</title></head><body>
<ul class="trail"><li><a href="/">home</a></li><li><a href="/data/">data</a></li></ul>
<pre>please read <a href="https://example.org/terms.html">our terms</a> first</pre>
<pre><a href="../">../</a>
<a href="a.csv">a.csv</a>     16-Nov-2026 09:00     20
<a href="b.csv">b.csv</a>     16-Nov-2026 09:00     20
</pre></body></html>"#;
    let index = read_at(page, sub).unwrap();
    assert_eq!(names(&index), [("a.csv", false), ("b.csv", false)]);
    assert_eq!(index.elsewhere, Vec::<String>::new());
}

/// a link's text says the same however its server spaces or escapes it: "Parent Directory" over
/// a run of spaces, a line break, a tab or a character reference, and a folder's name ending in
/// an escaped `/`.
#[test]
fn a_links_text_says_the_same_however_it_is_spaced_or_escaped() {
    // the text closes where it is when no tag sits in it, as `links` finds it
    let says = |text: &str| {
        let list = format!("<a href=\"..\">{text}</a>");
        let lower = list.to_ascii_lowercase();
        let at = "<a href=\"..\">".len();
        let close = Some(at + lower[at..].find('<').unwrap())
            .filter(|&close| lower[close..].starts_with("</a>"));
        text_says(&list, &lower, at, close)
    };
    for text in [
        "Parent Directory",
        " Parent Directory ",
        "Parent  Directory",
        "Parent\nDirectory",
        "Parent\r\nDirectory",
        "Parent\tDirectory",
        "Parent&#32;Directory",
        "parent <b>directory</b>",
    ] {
        let read = says(text);
        assert!(read.names_parent && read.goes_up, "{text:?}");
    }
    for (text, folder) in [("sub/", true), ("sub&#47;", true), ("sub", false)] {
        assert_eq!(says(text).names_folder, folder, "{text:?}");
    }
    assert!(!says("Parent Directory.csv").goes_up);
}

/// a listing a site titled as it likes reads only by its own folder: its list's link to the
/// folder above, on the folder's origin, reads "Parent Directory", as apache writes it. that
/// link on another host, scheme or port makes the page none, and an entry on another origin is
/// none, as in any listing.
#[test]
fn a_listing_titled_otherwise_reads_only_by_its_own_folder() {
    let page = |parent: &str, entry: &str| {
        format!(
            "<html><head><title>Our files</title></head><body><table>\
<tr><td><a href=\"{parent}\">Parent Directory</a></td></tr>\
<tr><td><a href=\"{entry}\">x.csv</a></td></tr></table></body></html>"
        )
    };
    let index = read_at(&page("/", "x.csv"), DATA).unwrap();
    assert_eq!(names(&index), [("x.csv", false)]);
    for parent in [
        "https://other.example/",
        "//other.example/",
        "http://example.org/",
        "https://example.org:8443/",
        "/data/",
    ] {
        assert_eq!(
            read_at(&page(parent, "x.csv"), DATA),
            Err(Unread::NoListing),
            "{parent}"
        );
    }
    let index = read_at(&page("/", "https://other.example/data/x.csv"), DATA).unwrap();
    assert_eq!(names(&index), Vec::<(&str, bool)>::new());
    assert_eq!(index.elsewhere, ["https://other.example/data/x.csv"]);
    // the same page sent from another host after a redirect lists nothing
    for parent in ["/", "https://example.org/"] {
        let recorded = Recorded::default()
            .on(DATA, 200, page(parent, "x.csv"))
            .redirected(DATA, "https://other.example/data/");
        let error = block_on(folder(DATA).resolve(&recorded, &[])).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{parent}");
    }
}

/// a page is a listing by its words for the folder: a first heading that begins as a listing's
/// title does, a title that is the folder's path, decoded, with or without its last `/`, or its
/// name beside a link to the folder above. a title naming another folder, or the name with no
/// link above, is none.
#[test]
fn a_listing_is_known_by_its_heading_or_the_folders_path_or_name() {
    let page = |title: &str, heading: &str, above: &str| {
        format!(
            "<html><head><title>{title}</title></head><body>{heading}<ul>{above}<li><a href=\"x.csv\">x.csv</a></li></ul></body></html>"
        )
    };
    let folder = "https://example.org/my%20data/";
    let up = "<li><a href=\"..\">Up</a></li>";
    for (title, heading, above, listing) in [
        ("Lab - files", "<h1>Index of /my data/</h1>", "", true),
        ("Lab - files", "<h1>Our files</h1>", "", false),
        ("/my data", "", "", true),
        ("/my data/", "", "", true),
        ("/other", "", "", false),
        ("my data", "", up, true),
        ("my data", "", "", false),
        ("other", "", up, false),
    ] {
        let read = read_at(&page(title, heading, above), folder);
        assert_eq!(read.is_ok(), listing, "{title} {heading} {above}");
    }
    let root = page("/", "", "");
    assert_eq!(
        names(&read_at(&root, "https://example.org/").unwrap()),
        [("x.csv", false)]
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
/// as `../`, or `..` under words that go up, a column's sort link with a query, an anchor and
/// every link outside its list go unsaid; any other link, to another host, out of the folder once
/// decoded, deeper than an entry, or up under a file's name, is told as it leads.
#[test]
fn only_links_into_the_folder_are_entries() {
    let page = r##"<html><head><title>Index of /data/</title></head><body>
<a href="/about.html">about</a>
<pre><a href="../">../</a> <a href="?C=M;O=A">Last modified</a> <a href="#top">top</a>
<A HREF='iris.csv'><img src="/icons/text.gif"></A> <a class="x" href=iris.csv>iris.csv</a>
<a href="sub/">sub/</a> <a href="https://other.example/x.csv">x.csv</a>
<a href="%2e%2e/secret.csv">secret.csv</a> <a href="/data/sub/deeper.csv">deeper.csv</a>
<a href="..">..</a> <a href="..">notes.csv</a> <a href="">here</a> <a href="C:x.csv">C:x.csv</a>
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
/// their paths below it, one request a folder that asks for a web page, as caddy sends one only
/// then, each file at the size in bytes its listing states.
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
            .all(|file| file.sha256.is_none() && file.size == Some(20) && !file.named_by_server)
    );
    assert_eq!(listing.warnings, Vec::<String>::new());
    assert_eq!(
        recorded.asked(),
        [listed(DATA), listed(&format!("{DATA}sub/"))]
    );
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
        let asked: Vec<String> = asked.iter().map(|link| listed(link)).collect();
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

/// a listing that comes from outside its folder after a redirect is refused for where it came
/// from, as one its site titled from https for an http link, and so is a folder with more files
/// and folders than a run lists.
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
    let http = "http://example.org/data/";
    let titled = "<html><head><title>Our files</title></head><body><table>\
<tr><td><a href=\"/\">Parent Directory</a></td></tr>\
<tr><td><a href=\"x.csv\">x.csv</a></td></tr></table></body></html>";
    let recorded = Recorded::default()
        .on(http, 200, titled.as_bytes().to_vec())
        .redirected(http, DATA);
    let error = block_on(folder(http).resolve(&recorded, &[])).unwrap_err();
    assert_eq!(
        error.message,
        format!("{http} sends fetchloom on to {DATA}, outside the folder")
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
        (
            Recorded::default()
                .on(DATA, 200, page.to_vec())
                .redirected(DATA, S3_PAGE),
            Kind::SourceInvalid,
            format!(
                "{DATA} sends fetchloom on to {S3_PAGE}, a page of the S3 bucket noaa-global-hourly-pds, not a folder listing"
            ),
        ),
    ] {
        let error = block_on(folder(DATA).resolve(&recorded, &[])).unwrap_err();
        assert_eq!((error.kind, error.message), (kind, message.clone()));
        assert!(error.help.is_some(), "{message}");
    }
    for bucket_page in [
        S3_PAGE,
        "https://s3.amazonaws.com/noaa-global-hourly-pds/index.html",
    ] {
        let recorded = Recorded::default()
            .on(DATA, 200, page.to_vec())
            .redirected(DATA, bucket_page);
        let error = block_on(folder(DATA).resolve(&recorded, &[])).unwrap_err();
        assert!(
            error.message.contains("S3 bucket noaa-global-hourly-pds"),
            "{bucket_page}"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "fetchloom lists a bucket's folder by its s3 link: run `fetchloom add s3://noaa-global-hourly-pds/<folder>/` with the folder's path in the bucket, which that page shows"
            )
        );
    }
    // a name S3 refuses, as a server may put a shell's words in, is offered as no bucket
    for elsewhere in [
        "https://s3.amazonaws.com/$(touch%20x)/index.html",
        "https://s3.amazonaws.com/a;b/index.html",
        "https://s3.amazonaws.com/index.html",
        "https://x.s3.amazonaws.com/index.html",
    ] {
        let recorded = Recorded::default()
            .on(DATA, 200, page.to_vec())
            .redirected(DATA, elsewhere);
        let error = block_on(folder(DATA).resolve(&recorded, &[])).unwrap_err();
        assert_eq!(
            error.message,
            format!("{DATA} sends fetchloom on to {elsewhere}, outside the folder")
        );
    }
}

/// a link to a git repository's page on a forge fetchloom reads no folders of names the forge's
/// host: gitea's and forgejo's on any host, gogs's, gin's and bitbucket's on their own, and
/// sourcehut's. github's and gitlab's are no such page, nor is any other.
#[test]
fn a_forge_page_is_known_by_its_link() {
    for (link, host) in [
        (
            "https://codeberg.org/Codeberg/Documentation/src/branch/main/content",
            Some("codeberg.org"),
        ),
        (
            "https://git.lab.example/lab/survey/src/tag/v1/data/",
            Some("git.lab.example"),
        ),
        (
            "https://gin.g-node.org/robintibor/high-gamma-dataset/src/master/data/train",
            Some("gin.g-node.org"),
        ),
        (
            "https://bitbucket.org/franrruiz/data_nyt_largev_4/src/master/",
            Some("bitbucket.org"),
        ),
        (
            "https://git.sr.ht/~sircmpwn/hare/tree/master/item/docs",
            Some("git.sr.ht"),
        ),
        ("https://example.org/lab/survey/src/main/data", None),
        ("https://github.com/lab/survey/tree/main/data", None),
        ("https://example.org/data/", None),
    ] {
        assert_eq!(forge(&url(link)), host, "{link}");
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
