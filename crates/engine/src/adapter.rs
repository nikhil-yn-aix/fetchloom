//! The adapter conformance suite every source implementation is judged by.

use std::io::Read;

use crate::credential::Credential;
use crate::error::ErrorKind;
use crate::redact::SafeUrl;
use crate::resume::ResumeRung;
use crate::seam::source::{ByteRange, Served, Source, SourceIdentity, SourceMetadata};
use crate::source_record::SourceRecord;
use crate::transfer::rung_for;

pub struct Fixture<'a> {
    pub location: &'a str,
    pub bytes: &'a [u8],
    pub supports_ranges: bool,
    pub exposes_identity: bool,
    pub container: Option<&'a str>,
    pub entries: &'a [&'a str],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub check: &'static str,
    pub expected: String,
    pub found: String,
}

fn finding(check: &'static str, expected: impl Into<String>, found: impl Into<String>) -> Finding {
    Finding {
        check,
        expected: expected.into(),
        found: found.into(),
    }
}

#[must_use]
pub fn judge<S: Source>(source: &S, fixture: &Fixture<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let credential: Option<&Credential> = None;

    let probed = probe_or_report(source, fixture, credential, &mut findings);
    let served = fetch_or_report(source, fixture, credential, &mut findings);

    if let (Some(probed), Some(served)) = (&probed, &served) {
        check_probe_agrees_with_fetch(probed, &served.metadata, &mut findings);
    }
    if let Some(served) = served {
        check_whole_fetch(served, fixture, &mut findings);
    }

    if fixture.supports_ranges {
        check_range_support_present(source, fixture, credential, &mut findings);
    } else {
        check_range_support_absent(source, fixture, credential, &mut findings);
    }

    if fixture.exposes_identity {
        check_immutable_identity_present(source, fixture, credential, &mut findings);
    } else if let Some(probed) = &probed {
        check_immutable_identity_absent(probed, &mut findings);
    }

    if let Some(container) = fixture.container {
        check_listing_supported(
            source,
            container,
            fixture.entries,
            credential,
            &mut findings,
        );
    } else {
        check_listing_not_supported(source, fixture.location, credential, &mut findings);
    }

    if let Some(probed) = &probed {
        check_degraded_trust_behavior(probed, fixture, &mut findings);
    }

    findings
}

fn probe_or_report<S: Source>(
    source: &S,
    fixture: &Fixture<'_>,
    credential: Option<&Credential>,
    findings: &mut Vec<Finding>,
) -> Option<SourceMetadata> {
    match source.probe(fixture.location, credential) {
        Ok(metadata) => Some(metadata),
        Err(error) => {
            findings.push(finding(
                "probe_agrees_with_fetch",
                "a probe of the fixture's location to succeed",
                format!("the probe failed: {error}"),
            ));
            None
        }
    }
}

fn fetch_or_report<S: Source>(
    source: &S,
    fixture: &Fixture<'_>,
    credential: Option<&Credential>,
    findings: &mut Vec<Finding>,
) -> Option<Served<S::Body>> {
    match source.fetch(fixture.location, None, credential) {
        Ok(served) => Some(served),
        Err(error) => {
            findings.push(finding(
                "whole_fetch_serves_exact_bytes",
                "a fetch of the fixture's whole location to succeed",
                format!("the fetch failed: {error}"),
            ));
            None
        }
    }
}

fn check_probe_agrees_with_fetch(
    probed: &SourceMetadata,
    fetched: &SourceMetadata,
    findings: &mut Vec<Finding>,
) {
    if probed.size != fetched.size {
        findings.push(finding(
            "probe_agrees_with_fetch",
            format!(
                "a fetch reporting size {:?}, the same as the probe",
                probed.size
            ),
            format!("a fetch reporting size {:?}", fetched.size),
        ));
    }
    if probed.identity != fetched.identity {
        findings.push(finding(
            "probe_agrees_with_fetch",
            format!(
                "a fetch reporting identity {:?}, the same as the probe",
                probed.identity
            ),
            format!("a fetch reporting identity {:?}", fetched.identity),
        ));
    }
    if probed.supports_ranges != fetched.supports_ranges {
        findings.push(finding(
            "probe_agrees_with_fetch",
            format!(
                "a fetch reporting supports_ranges {}, the same as the probe",
                probed.supports_ranges
            ),
            format!(
                "a fetch reporting supports_ranges {}",
                fetched.supports_ranges
            ),
        ));
    }
}

fn check_whole_fetch<B: Read>(
    served: Served<B>,
    fixture: &Fixture<'_>,
    findings: &mut Vec<Finding>,
) {
    let mut body = served.body;
    let mut bytes = Vec::new();
    match body.read_to_end(&mut bytes) {
        Ok(_) if bytes == fixture.bytes => {}
        Ok(_) => findings.push(finding(
            "whole_fetch_serves_exact_bytes",
            format!("{} bytes matching the fixture exactly", fixture.bytes.len()),
            format!("{} bytes that do not match the fixture", bytes.len()),
        )),
        Err(error) => findings.push(finding(
            "whole_fetch_serves_exact_bytes",
            "the whole object read to completion",
            format!("reading the body failed: {error}"),
        )),
    }
}

fn middle_span(length: usize) -> ByteRange {
    let total = u64::try_from(length).unwrap_or(u64::MAX);
    let quarter = total / 4;
    let start = quarter;
    let end = (total - quarter).max(start + 1).min(total);
    ByteRange { start, end }
}

fn check_range_support_present<S: Source>(
    source: &S,
    fixture: &Fixture<'_>,
    credential: Option<&Credential>,
    findings: &mut Vec<Finding>,
) {
    if let Ok(metadata) = source.probe(fixture.location, credential)
        && !metadata.supports_ranges
    {
        findings.push(finding(
            "range_support_present",
            "supports_ranges: true, because the fixture's source supports ranges",
            "supports_ranges: false",
        ));
    }

    let range = middle_span(fixture.bytes.len());
    match source.fetch(fixture.location, Some(range), credential) {
        Ok(served) => {
            let mut body = served.body;
            let mut bytes = Vec::new();
            match body.read_to_end(&mut bytes) {
                Ok(_) => {
                    let start = usize::try_from(range.start).unwrap_or(usize::MAX);
                    let end = usize::try_from(range.end).unwrap_or(usize::MAX);
                    let expected = fixture.bytes.get(start..end).unwrap_or(&[]);
                    if bytes != expected {
                        findings.push(finding(
                            "range_support_present",
                            format!(
                                "exactly the {} requested bytes and no others",
                                expected.len()
                            ),
                            format!("{} bytes that are not the requested span", bytes.len()),
                        ));
                    }
                }
                Err(error) => findings.push(finding(
                    "range_support_present",
                    "the requested span read to completion",
                    format!("reading the ranged body failed: {error}"),
                )),
            }
        }
        Err(error) => findings.push(finding(
            "range_support_present",
            "a fetch of a middle span to succeed, because the fixture's source supports ranges",
            format!("the ranged fetch failed: {error}"),
        )),
    }
}

fn check_range_support_absent<S: Source>(
    source: &S,
    fixture: &Fixture<'_>,
    credential: Option<&Credential>,
    findings: &mut Vec<Finding>,
) {
    if let Ok(metadata) = source.probe(fixture.location, credential)
        && metadata.supports_ranges
    {
        findings.push(finding(
            "range_support_absent",
            "supports_ranges: false, because the fixture's source does not support ranges",
            "supports_ranges: true",
        ));
    }

    let range = middle_span(fixture.bytes.len());
    match source.fetch(fixture.location, Some(range), credential) {
        Ok(served) => {
            let mut body = served.body;
            let mut bytes = Vec::new();
            let _ = body.read_to_end(&mut bytes);
            findings.push(finding(
                "range_support_absent",
                format!(
                    "{}, because a source with no range support cannot serve a span",
                    ErrorKind::SourceUnsupportedRange.label()
                ),
                format!(
                    "a served span of {} bytes rather than a failure, as if the object were quietly returned whole",
                    bytes.len()
                ),
            ));
        }
        Err(error) if error.kind() != ErrorKind::SourceUnsupportedRange => {
            findings.push(finding(
                "range_support_absent",
                ErrorKind::SourceUnsupportedRange.label(),
                error.kind().label(),
            ));
        }
        Err(_) => {}
    }
}

fn check_immutable_identity_present<S: Source>(
    source: &S,
    fixture: &Fixture<'_>,
    credential: Option<&Credential>,
    findings: &mut Vec<Finding>,
) {
    let first = source.probe(fixture.location, credential);
    let second = source.probe(fixture.location, credential);
    let (Ok(one), Ok(two)) = (first, second) else {
        return;
    };
    if one.identity == SourceIdentity::None {
        findings.push(finding(
            "immutable_identity_present",
            "an identity other than SourceIdentity::None, because the fixture's source exposes one",
            "SourceIdentity::None",
        ));
        return;
    }
    if one.identity != two.identity {
        findings.push(finding(
            "immutable_identity_present",
            format!(
                "the same identity on a second probe of the same location, {:?}",
                one.identity
            ),
            format!(
                "a different identity on the second probe, {:?}",
                two.identity
            ),
        ));
    }
}

fn check_immutable_identity_absent(probed: &SourceMetadata, findings: &mut Vec<Finding>) {
    if probed.identity != SourceIdentity::None {
        findings.push(finding(
            "immutable_identity_absent",
            "SourceIdentity::None, because the fixture's source exposes no identity",
            format!("{:?}", probed.identity),
        ));
    }
}

fn check_listing_supported<S: Source>(
    source: &S,
    container: &str,
    entries: &[&str],
    credential: Option<&Credential>,
    findings: &mut Vec<Finding>,
) {
    match source.list(container, credential) {
        Ok(listed) => {
            let mut found: Vec<&str> = listed
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect();
            found.sort_unstable();
            let mut expected: Vec<&str> = entries.to_vec();
            expected.sort_unstable();
            if found != expected {
                findings.push(finding(
                    "listing_supported",
                    format!("exactly {entries:?}"),
                    format!("{found:?}"),
                ));
            }
            for entry in &listed.entries {
                let escapes =
                    entry.path.starts_with('/') || entry.path.split('/').any(|part| part == "..");
                if escapes {
                    findings.push(finding(
                        "listing_supported",
                        "every returned entry at or below the listed prefix",
                        format!("an entry outside the prefix: {}", entry.path),
                    ));
                }
            }
        }
        Err(error) => findings.push(finding(
            "listing_supported",
            "a listing of the fixture's container to succeed",
            format!("the listing failed: {error}"),
        )),
    }
}

fn check_listing_not_supported<S: Source>(
    source: &S,
    location: &str,
    credential: Option<&Credential>,
    findings: &mut Vec<Finding>,
) {
    if let Ok(listed) = source.list(location, credential) {
        findings.push(finding(
            "listing_not_supported",
            "listing the object's own location to fail, because the fixture names no container",
            format!(
                "a listing that returned {} entries instead of failing",
                listed.entries.len()
            ),
        ));
    }
}

fn record_matching(
    location: &str,
    probed: &SourceMetadata,
    rung: ResumeRung,
    written: u64,
) -> SourceRecord {
    SourceRecord {
        location: SafeUrl::new(location),
        host: probed.host.as_str().to_owned(),
        size: probed.size,
        identity: probed.identity.clone(),
        etag: None,
        last_modified: probed.last_modified.clone(),
        accepts_ranges: probed.supports_ranges,
        written,
        rung,
    }
}

fn check_degraded_trust_behavior(
    probed: &SourceMetadata,
    fixture: &Fixture<'_>,
    findings: &mut Vec<Finding>,
) {
    let Ok((initial_rung, _)) = rung_for(None, probed, 0, 0) else {
        return;
    };

    if !fixture.exposes_identity && initial_rung != ResumeRung::NoValidator {
        findings.push(finding(
            "degraded_trust_behavior",
            "no rung above the restart rung, because the fixture's source exposes no identity",
            format!("rung {}", initial_rung.number()),
        ));
    }

    let on_disk = u64::try_from(fixture.bytes.len().div_ceil(2))
        .unwrap_or(u64::MAX)
        .max(1);
    let record = record_matching(fixture.location, probed, initial_rung, on_disk);
    let Ok((resumed_rung, kept)) = rung_for(Some(&record), probed, on_disk, 0) else {
        return;
    };

    if !fixture.supports_ranges && kept != 0 {
        findings.push(finding(
            "degraded_trust_behavior",
            "no resume at all, because the fixture's source does not support ranges",
            format!(
                "a resume keeping {kept} bytes on rung {}",
                resumed_rung.number()
            ),
        ));
    }

    if fixture.supports_ranges
        && fixture.exposes_identity
        && (kept != on_disk || resumed_rung != initial_rung)
    {
        findings.push(finding(
            "degraded_trust_behavior",
            format!(
                "a resume keeping {on_disk} bytes on rung {}, because the fixture's source supports ranges and exposes identity",
                initial_rung.number()
            ),
            format!("a resume keeping {kept} bytes on rung {}", resumed_rung.number()),
        ));
    }
}
