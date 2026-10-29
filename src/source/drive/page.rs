//! the pages drive sends where a file was expected: the form that confirms a download too large
//! to scan for viruses, and the ways drive refuses one.

use url::Url;

use super::{html, is_id};
use crate::error::{Error, Kind};

/// the URL a confirmation form leads to, or why drive will not send the file.
pub(super) fn divert(page: &Url, status: u16, text: &str) -> Result<Url, Error> {
    let link = file_link(page);
    if let Some(reason) = refusal(text) {
        if reason.contains("Too many users") {
            return Err(Error::new(
                Kind::NetworkRateLimited,
                "drive refused the download: too many people downloaded this file recently",
            )
            .detail(link)
            .detail("this is a temporary limit on google's side, usually lifted within a day")
            .help("try again later, or ask the owner for another link"));
        }
        return Err(Error::new(
            Kind::SourceInvalid,
            format!("drive refused the download of {link}"),
        )
        .detail(format!("drive says: {reason}"))
        .help("open the link in a browser to see why"));
    }
    if page.host_str() == Some("accounts.google.com") || matches!(status, 401 | 403) {
        return Err(Error::new(
            Kind::AuthRequired,
            format!("{link} is not shared with anyone who has the link"),
        )
        .help("ask its owner to share it with anyone with the link. fetchloom does not sign in to google"));
    }
    match status {
        200 => form(page, text).ok_or_else(|| {
            Error::new(
                Kind::SourceInvalid,
                format!("drive sent a page instead of {link}"),
            )
            .help("open the link in a browser to see what drive shows in place of the file")
        }),
        404 | 410 => Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("drive has no file at {link}"),
        )
        .help("check the link in a browser. a file that was deleted, or shared only with some people, cannot be fetched")),
        status => Err(Error::new(
            Kind::NetworkStatus,
            format!("drive answered {status} for {link}"),
        )),
    }
}

/// the reason drive gives on a page that refuses a file, such as a download quota.
fn refusal(text: &str) -> Option<String> {
    let reason = html::between(text, "<p class=\"uc-error-subcaption\">", "</p>")
        .or_else(|| html::between(text, "<p class=\"uc-error-caption\">", "</p>"))?;
    Some(html::text(reason))
}

/// the URL the confirmation form submits to, with its hidden fields as the query.
fn form(page: &Url, text: &str) -> Option<Url> {
    let start = text.find("<form id=\"download-form\"")?;
    let form = &text[start..start + text[start..].find("</form>")?];
    let tag = &form[..form.find('>')?];
    let mut next = page.join(&html::attribute(tag, "action")?).ok()?;
    {
        let mut query = next.query_pairs_mut();
        for input in form.split("<input").skip(1) {
            let tag = &input[..input.find('>')?];
            if html::attribute(tag, "type").as_deref() == Some("hidden") {
                query.append_pair(
                    &html::attribute(tag, "name")?,
                    &html::attribute(tag, "value")?,
                );
            }
        }
    }
    (next.scheme() == "https").then_some(next)
}

/// the drive link of the file a page is about, found in the page's URL or, on a sign-in page, in
/// the URL it continues to.
fn file_link(page: &Url) -> String {
    let query = |url: &Url, name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    let id = query(page, "id").or_else(|| {
        let next = Url::parse(&query(page, "continue")?).ok()?;
        query(&next, "id")
    });
    match id.filter(|id| is_id(id)) {
        Some(id) => format!("https://drive.google.com/file/d/{id}"),
        None => "a drive file".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Kind;
    use crate::source::recorded::fixture_bytes;

    const BASNET: &str = "1s52ek_4YTDRt_EOkx1FS53u-vJa0c4nu";

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn page(fixture: &str) -> String {
        String::from_utf8(fixture_bytes(fixture)).unwrap()
    }

    /// where fetchloom asks drive for a file first.
    fn download(id: &str) -> Url {
        url(&format!(
            "https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"
        ))
    }

    #[test]
    fn the_virus_scan_form_leads_to_the_file() {
        let asked = url(&format!(
            "https://drive.usercontent.google.com/download?id={BASNET}"
        ));
        let next = divert(&asked, 200, &page("drive/confirm.html")).unwrap();
        assert_eq!(
            next.as_str(),
            format!(
                "https://drive.usercontent.google.com/download?id={BASNET}&confirm=t&uuid=5ee198d6-095a-4e7d-9a9c-49f71884964a"
            )
        );
    }

    #[test]
    fn a_file_over_its_download_quota_says_to_wait() {
        // no file was over its quota while this was written, so this page is not recorded. its
        // markup is the one gdown reads
        let text = r#"<div id="uc-text"><p class="uc-error-caption">Sorry, you can&#39;t view or download this file at this time.</p><p class="uc-error-subcaption">Too many users have viewed or downloaded this file recently. Please try accessing the file again later. If the file you are trying to access is particularly large or is shared with many people, it may take up to 24 hours to be able to view or download the file. If you still can&#39;t access a file after 24 hours, contact your domain administrator.</p></div>"#;
        for status in [200, 403] {
            let err = divert(&download(BASNET), status, text).unwrap_err();
            assert_eq!(err.kind, Kind::NetworkRateLimited, "{status}");
            assert_eq!(
                err.message,
                "drive refused the download: too many people downloaded this file recently"
            );
            assert_eq!(
                err.details,
                [
                    format!("https://drive.google.com/file/d/{BASNET}"),
                    "this is a temporary limit on google's side, usually lifted within a day"
                        .to_owned()
                ]
            );
            assert_eq!(
                err.help.as_deref(),
                Some("try again later, or ask the owner for another link")
            );
        }
    }

    #[test]
    fn other_refusals_pass_on_what_drive_says() {
        let text = r#"<p class="uc-error-caption">Sorry, this file is infected with a virus</p><p class="uc-error-subcaption">Only the owner is allowed to download infected files.</p>"#;
        let err = divert(&download(BASNET), 200, text).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(
            err.details,
            ["drive says: Only the owner is allowed to download infected files."]
        );
    }

    #[test]
    fn a_file_shared_with_some_people_names_what_its_owner_can_do() {
        // drive sends a file shared only inside one organization to its sign-in page. the page is
        // trimmed to its first kilobyte of 927
        let sign_in = url(
            "https://accounts.google.com/v3/signin/identifier?continue=https://drive.usercontent.google.com/download?id%3D15J0eCL77fHbItA249epT3i2iOx4VwJGI%26export%3Ddownload%26confirm%3Dt&followup=https://drive.usercontent.google.com/download?id%3D15J0eCL77fHbItA249epT3i2iOx4VwJGI%26export%3Ddownload%26confirm%3Dt&passive=1209600&service=wise&flowName=WebLiteSignIn&flowEntry=ServiceLogin",
        );
        let err = divert(&sign_in, 200, &page("drive/sign-in.html")).unwrap_err();
        assert_eq!(err.kind, Kind::AuthRequired);
        assert_eq!(
            err.message,
            "https://drive.google.com/file/d/15J0eCL77fHbItA249epT3i2iOx4VwJGI is not shared with anyone who has the link"
        );
        assert!(err.help.unwrap().contains("anyone with the link"));
        // the refusal drive sent for the same item on its folder view
        let refusal = page("drive/folder-private.html");
        for status in [401, 403] {
            let err = divert(&download(BASNET), status, &refusal).unwrap_err();
            assert_eq!(err.kind, Kind::AuthRequired, "{status}");
        }
    }

    #[test]
    fn a_deleted_file_is_unresolved() {
        let id = "1AbCdEfGhIjKlMnOpQrStUvWxYz012345";
        let err = divert(&download(id), 404, &page("drive/missing.html")).unwrap_err();
        assert_eq!(err.kind, Kind::ReferenceUnresolved);
        assert!(
            err.message
                .contains(&format!("https://drive.google.com/file/d/{id}"))
        );
    }

    #[test]
    fn any_other_page_is_never_taken_for_the_file() {
        let err = divert(
            &download(BASNET),
            200,
            "<html><body>something else</body></html>",
        )
        .unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
        let err = divert(&download(BASNET), 400, "<html></html>").unwrap_err();
        assert_eq!(err.kind, Kind::NetworkStatus);
    }
}
