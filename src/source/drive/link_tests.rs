use super::*;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const FILE: &str = "1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb";
const FOLDER: &str = "1oow464Z4Et95RAwtRZn6dLOFb_YYpLwL";

fn reference(link: &str) -> Reference {
    Reference::parse(link).unwrap().reference
}

#[test]
fn an_open_link_uses_the_file_drive_redirects_to() {
    let open = format!("https://drive.google.com/open?id={FILE}");
    let view = format!("https://drive.google.com/file/d/{FILE}/view");
    let recorded = Recorded::default()
        .fixture(&open, "drive/open-file.html")
        .redirected(&open, &view);
    let parsed = reference(&open);
    assert_eq!(parsed.to_string(), format!("drive:open/{FILE}"));
    let listing = block_on(parsed.resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), format!("drive:{FILE}"));
    assert_eq!(listing.files.len(), 1);
    assert_eq!(
        listing.files[0].url.as_str(),
        format!("https://drive.google.com/uc?id={FILE}")
    );
    assert!(listing.files[0].named_by_server);
    assert_eq!(recorded.asked(), [open]);
}

#[test]
fn an_open_link_walks_the_folder_drive_redirects_to() {
    let open = format!("https://drive.google.com/open?id={FOLDER}");
    let recorded = Recorded::default()
        .fixture(&open, "drive/open-folder.html")
        .redirected(
            &open,
            &format!("https://drive.google.com/drive/folders/{FOLDER}"),
        )
        .fixture(
            &format!("https://drive.google.com/embeddedfolderview?id={FOLDER}"),
            "drive/folder-deep.html",
        )
        .fixture(
            "https://drive.google.com/embeddedfolderview?id=1TMZoSAu4ecs_Q3GEEWfiyrQIPtj3DJAC",
            "drive/folder-deep-1.html",
        )
        .fixture(
            "https://drive.google.com/embeddedfolderview?id=1RRfB3nFcMtg_ibfCjoU6P6DbsrXkwnPF",
            "drive/folder-deep-2.html",
        )
        .fixture(
            "https://drive.google.com/embeddedfolderview?id=1-Z2wzjaaqqB1lBiPXNGfgkICA2Pc9q_U",
            "drive/folder-deep-1-1.html",
        );
    let listing = block_on(reference(&open).resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        format!("drive:folders/{FOLDER}")
    );
    assert_eq!(listing.files.len(), 4);
    assert_eq!(recorded.asked().len(), 5);
}

#[test]
fn resource_keys_survive_reference_roundtrips() {
    for (link, canonical) in [
        (
            format!("https://drive.google.com/file/d/{FILE}/view?resourcekey=0-a%26b"),
            format!("drive:{FILE}?resourcekey=0-a%26b"),
        ),
        (
            format!("https://drive.google.com/drive/folders/{FOLDER}?resourcekey=0-root"),
            format!("drive:folders/{FOLDER}?resourcekey=0-root"),
        ),
        (
            format!("https://drive.google.com/open?id={FILE}&resourcekey=0-file"),
            format!("drive:open/{FILE}?resourcekey=0-file"),
        ),
    ] {
        let parsed = reference(&link);
        assert_eq!(parsed.to_string(), canonical);
        assert_eq!(reference(&canonical), parsed);
    }
}

#[test]
fn a_shared_files_key_reaches_the_lock_and_the_download() {
    let link = format!("https://drive.google.com/file/d/{FILE}/view?resourcekey=0-file");
    let listing = block_on(reference(&link).resolve(&Recorded::default())).unwrap();
    assert_eq!(
        listing.files[0].url.as_str(),
        format!("https://drive.google.com/uc?id={FILE}&resourcekey=0-file")
    );
    let (from, _) = crate::source::fetch_plan(&listing.files[0].url);
    assert_eq!(
        from.as_str(),
        format!(
            "https://drive.usercontent.google.com/download?id={FILE}&resourcekey=0-file&export=download&confirm=t"
        )
    );
}

#[test]
fn a_shared_folders_key_reaches_its_listing() {
    let link = format!("https://drive.google.com/drive/folders/{FOLDER}?resourcekey=0-root");
    let url = format!("https://drive.google.com/embeddedfolderview?id={FOLDER}&resourcekey=0-root");
    let recorded = Recorded::default().fixture(&url, "drive/folder-deep-1-1.html");
    let listing = block_on(reference(&link).resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 1);
    assert_eq!(recorded.asked(), [url]);
    assert_eq!(
        listing.resolved.to_string(),
        format!("drive:folders/{FOLDER}?resourcekey=0-root")
    );
}

#[test]
fn an_open_link_keeps_a_resource_key_omitted_from_the_redirect() {
    let open = format!("https://drive.google.com/open?id={FILE}&resourcekey=0-file");
    let recorded = Recorded::default()
        .fixture(&open, "drive/open-file.html")
        .redirected(
            &open,
            &format!("https://drive.google.com/file/d/{FILE}/view"),
        );
    let listing = block_on(reference(&open).resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        format!("drive:{FILE}?resourcekey=0-file")
    );
    assert_eq!(recorded.asked(), [open]);
}

#[test]
fn a_private_or_missing_open_link_explains_the_next_step() {
    let open = format!("https://drive.google.com/open?id={FILE}");
    for (status, fixture, kind) in [
        (401, "drive/folder-private.html", Kind::AuthRequired),
        (404, "drive/missing.html", Kind::ReferenceUnresolved),
    ] {
        let recorded = Recorded::default().on(&open, status, fixture_bytes(fixture));
        let error = block_on(reference(&open).resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(error.help.is_some());
    }
}

#[test]
fn an_open_link_must_end_at_a_drive_file_or_folder() {
    let open = format!("https://drive.google.com/open?id={FILE}");
    for final_url in [open.as_str(), "https://example.org/not-a-drive-file"] {
        let recorded = Recorded::default()
            .fixture(&open, "drive/open-file.html")
            .redirected(&open, final_url);
        let error = block_on(reference(&open).resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
    }
}

#[test]
fn download_plans_read_confirmation_and_refusal_pages() {
    let id = "1s52ek_4YTDRt_EOkx1FS53u-vJa0c4nu";
    let link = Url::parse(&format!("https://drive.google.com/uc?id={id}")).unwrap();
    let (from, divert) = crate::source::fetch_plan(&link);
    let divert = divert.unwrap();
    let recorded = Recorded::default().fixture(from.as_str(), "drive/confirm.html");
    let response = block_on(recorded.get(&from)).unwrap();
    let next = divert(&response.url, response.status, &response.text()).unwrap();
    assert_eq!(
        next.as_str(),
        format!(
            "https://drive.usercontent.google.com/download?id={id}&confirm=t&uuid=5ee198d6-095a-4e7d-9a9c-49f71884964a"
        )
    );
    for (status, fixture, kind) in [
        (200, "drive/quota.html", Kind::NetworkRateLimited),
        (403, "drive/quota.html", Kind::NetworkRateLimited),
        (401, "drive/folder-private.html", Kind::AuthRequired),
        (404, "drive/missing.html", Kind::ReferenceUnresolved),
    ] {
        let recorded = Recorded::default().on(from.as_str(), status, fixture_bytes(fixture));
        let response = block_on(recorded.get(&from)).unwrap();
        let error = divert(&response.url, response.status, &response.text()).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(error.help.is_some());
    }
}
