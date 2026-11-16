//! links to shares on onedrive, sharepoint, box, nextcloud and owncloud, which answer with the
//! share's page rather than its file, known by their host or their path alone.

use url::Url;

/// the name of the service `url`'s host belongs to, for onedrive, sharepoint and box, whose
/// pages are not the files they share.
pub(crate) fn service(url: &Url) -> Option<&'static str> {
    let host = crate::net::host(url).to_ascii_lowercase();
    let within = |domain: &str| {
        host == domain
            || host
                .strip_suffix(domain)
                .is_some_and(|below| below.ends_with('.'))
    };
    if ["1drv.ms", "onedrive.live.com", "onedrive.com"]
        .into_iter()
        .any(within)
    {
        Some("OneDrive")
    } else if within("sharepoint.com") {
        Some("SharePoint")
    } else if within("box.com") {
        Some("Box")
    } else {
        None
    }
}

/// where a nextcloud or owncloud share at `url`, a path that ends `/s/<token>`, gives its file:
/// the same path with `/download`, without the link's query, as `?path=` for a folder within the
/// share, since the share's own download holds that folder too.
pub(crate) fn cloud_download(url: &Url) -> Option<Url> {
    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect();
    let [.., "s", token] = segments.as_slice() else {
        return None;
    };
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return None;
    }
    let mut download = url.clone();
    download.set_query(None);
    download.set_fragment(None);
    download
        .path_segments_mut()
        .ok()?
        .pop_if_empty()
        .push("download");
    Some(download)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_is_known_by_its_host_or_its_path() {
        for (link, named, download) in [
            ("https://1drv.ms/f/c/ab12/Ecd34", Some("OneDrive"), None),
            ("https://onedrive.live.com/?id=AB12", Some("OneDrive"), None),
            (
                "https://contoso-my.sharepoint.com/:u:/g/personal/ada/EAbC",
                Some("SharePoint"),
                None,
            ),
            ("https://lab.app.box.com/v/iris", Some("Box"), None),
            (
                "https://uni.sciebo.de/s/HGdUkoNlW1Ub0Gx?path=%2Fsub#top",
                None,
                Some("https://uni.sciebo.de/s/HGdUkoNlW1Ub0Gx/download"),
            ),
            (
                "https://surfdrive.surf.nl/files/index.php/s/Ab12_-x/",
                None,
                Some("https://surfdrive.surf.nl/files/index.php/s/Ab12_-x/download"),
            ),
            // a host that only ends in a service's name, and paths of no share
            (
                "https://notbox.com/s/abc",
                None,
                Some("https://notbox.com/s/abc/download"),
            ),
            ("https://data.example.org/s/", None, None),
            ("https://data.example.org/s/a.b", None, None),
            ("https://data.example.org/files/iris", None, None),
        ] {
            let url = Url::parse(link).unwrap();
            assert_eq!(service(&url), named, "{link}");
            assert_eq!(
                cloud_download(&url).map(String::from).as_deref(),
                download,
                "{link}"
            );
        }
    }
}
