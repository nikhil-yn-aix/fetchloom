//! Deciding which rung a transfer starts on, given what is on disk and what
//! the source says now.

use crate::digest::ContentDigest;
use crate::error::{Error, ErrorKind};
use crate::redact::SafeUrl;
use crate::resume::ResumeRung;
use crate::seam::source::{SourceIdentity, SourceMetadata, Validator};
use crate::source_record::SourceRecord;

use super::Transferred;

pub struct Prior {
    pub digest: ContentDigest,
    pub validator: Validator,
}

pub fn rung_for(
    recorded: Option<&SourceRecord>,
    now: &SourceMetadata,
    on_disk: u64,
    verified: u64,
) -> Result<(ResumeRung, u64), Error> {
    if verified > 0 {
        return Ok((ResumeRung::Outboard, verified));
    }
    if on_disk == 0 {
        return Ok((rung_of(&now.identity), 0));
    }
    let Some(recorded) = recorded else {
        return Ok((ResumeRung::NoValidator, 0));
    };
    if rung_of(&recorded.identity) == ResumeRung::ImmutableIdentity
        && rung_of(&now.identity) == ResumeRung::ImmutableIdentity
        && recorded.identity != now.identity
    {
        return Err(Error::new(
            ErrorKind::SourceIdentityChanged,
            "fetch this object from a source whose immutable identity is immutable, because this \
             one served a different one for the same location and so the identity it promised does \
             not hold",
        ));
    }
    if !now.supports_ranges || !recorded.identifies_the_same_bytes_as(&now.identity) {
        return Ok((ResumeRung::NoValidator, 0));
    }
    Ok((rung_of(&now.identity), on_disk))
}

fn rung_of(identity: &SourceIdentity) -> ResumeRung {
    match identity {
        SourceIdentity::ContentAddress(_) | SourceIdentity::ImmutableVersion(_) => {
            ResumeRung::ImmutableIdentity
        }
        SourceIdentity::StrongValidator(_) => ResumeRung::StrongValidator,
        SourceIdentity::WeakValidator(_) => ResumeRung::WeakValidator,
        SourceIdentity::None => ResumeRung::NoValidator,
    }
}

pub(super) fn held(digest: ContentDigest) -> Transferred {
    Transferred {
        digest,
        interop: None,
        bytes_transferred: 0,
        bytes_kept: 0,
        rung: ResumeRung::Outboard,
        attempts: 0,
        validator: Validator::default(),
        served: SafeUrl::new(""),
        chosen: None,
    }
}

pub(super) fn validator_for(metadata: &SourceMetadata) -> Validator {
    Validator {
        etag: validator_of(&metadata.identity),
        last_modified: metadata.last_modified.clone(),
    }
}

pub(super) fn record_of(metadata: &SourceMetadata, rung: ResumeRung, written: u64) -> SourceRecord {
    SourceRecord {
        location: metadata.location.clone(),
        host: metadata.host.as_str().to_owned(),
        size: metadata.size,
        etag: validator_of(&metadata.identity),
        identity: metadata.identity.clone(),
        last_modified: metadata.last_modified.clone(),
        accepts_ranges: metadata.supports_ranges,
        written,
        rung,
    }
}

fn validator_of(identity: &SourceIdentity) -> Option<String> {
    match identity {
        SourceIdentity::StrongValidator(tag) | SourceIdentity::WeakValidator(tag) => {
            Some(tag.clone())
        }
        _ => None,
    }
}
