//! a link to a share on onedrive, sharepoint, box, nextcloud or owncloud, which answers with the
//! share's page rather than its file: the refusal of that page says what gives the file.

use url::Url;

use crate::error::{Error, Kind};
use crate::shown::Shown;
use crate::source::share::{cloud_download, service};

/// the refusal of the web page `url` answered with, when `url` links to a share; none for any
/// other link.
pub(in crate::cli) fn page_refused(url: &Url) -> Option<Error> {
    let browser = format!(
        "download the file from that page in a browser, then add the file it saved with `{}`",
        super::project::command("add <file>")
    );
    if let Some(service) = service(url) {
        return Some(
            Error::new(
                Kind::SourceInvalid,
                format!(
                    "{} answered with a {service} page, not the file",
                    Shown(url)
                ),
            )
            .help(browser),
        );
    }
    let download = cloud_download(url)?;
    Some(
        Error::new(
            Kind::SourceInvalid,
            format!(
                "{} answered with a web page, such as a nextcloud share's page, not the file",
                Shown(url)
            ),
        )
        .help(format!(
            "{browser}. if it is a nextcloud or owncloud share, the same link with /download gives the file: {}",
            Shown(&download)
        )),
    )
}
