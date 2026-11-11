//! a mirror on a web server, read over http or https by the network that reads sources: its
//! files come through the download pipeline, resumed, retried and checked like any pinned file,
//! and its marker and trees through requests whose answers are read whole, at most 64 MiB.

use url::Url;

use super::{MARKER, MARKER_LINE, Wanted, below};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Expect, Fetch, Net};

/// what a web mirror's marker showed.
pub(crate) enum Look {
    There,
    /// no marker, so the address is not a mirror's top.
    NoMarker,
    /// another answer than the marker, such as 403 from a gatekeeper.
    Answered(u16),
    /// no answer, after the network's retries.
    Failed(Error),
}

/// a tree a web mirror gave.
pub(crate) enum Tree {
    Bytes(Vec<u8>),
    /// none there, or one too large to read whole, so the archive itself is needed.
    Absent,
    Answered(u16),
    Failed(Error),
}

/// asks the web mirror at `base` for its marker, once.
///
/// # Errors
///
/// `interrupted` when a stop is asked.
pub(crate) async fn probe(net: &Net, base: &Url) -> Result<Look, Error> {
    let answer = match net.mirror_get(&joined(base, &[MARKER])).await {
        Ok(answer) => answer,
        Err(error) if error.kind == Kind::Interrupted => return Err(error),
        Err(error) => return Ok(Look::Failed(error)),
    };
    Ok(match answer.status {
        200 if answer.text().lines().next().map(str::trim) == Some(MARKER_LINE) => Look::There,
        200 | 404 | 410 => Look::NoMarker,
        status => Look::Answered(status),
    })
}

/// the download of `wanted` from the web mirror at `base`, pinned to its SHA-256 and size.
pub(crate) fn object(base: &Url, wanted: &Wanted) -> Fetch {
    Fetch {
        expect: Expect {
            sha256: Some(wanted.digest),
            md5: None,
            size: Some(wanted.size),
        },
        mirror: true,
        ..Fetch::new(joined(base, &below("objects", &wanted.digest)))
    }
}

/// the tree whose bytes hash to `tree`, as the web mirror at `base` holds it.
///
/// # Errors
///
/// `interrupted` when a stop is asked.
pub(crate) async fn read_tree(net: &Net, base: &Url, tree: &Sha256) -> Result<Tree, Error> {
    let answer = match net.mirror_get(&joined(base, &below("trees", tree))).await {
        Ok(answer) => answer,
        Err(error) if error.kind == Kind::Interrupted => return Err(error),
        // past 64 MiB
        Err(error) if error.kind == Kind::SourceInvalid => return Ok(Tree::Absent),
        Err(error) => return Ok(Tree::Failed(error)),
    };
    Ok(match answer.status {
        200 => Tree::Bytes(answer.body),
        404 | 410 => Tree::Absent,
        status => Tree::Answered(status),
    })
}

/// `parts` below the web mirror at `base`, which ends in `/`.
fn joined(base: &Url, parts: &[impl AsRef<str>]) -> Url {
    let mut url = base.clone();
    let below: Vec<&str> = parts.iter().map(AsRef::as_ref).collect();
    url.set_path(&format!("{}{}", base.path(), below.join("/")));
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mirror_file_is_named_below_the_mirror_address() {
        let base = Url::parse("https://data.corp.internal/fetchloom/").unwrap();
        let wanted = Wanted {
            digest: crate::digest::sha256(b"pets"),
            size: 4,
            label: "pets/a.png".to_owned(),
        };
        let hex = wanted.digest.hex();
        let fetch = object(&base, &wanted);
        assert_eq!(
            fetch.url.as_str(),
            format!(
                "https://data.corp.internal/fetchloom/objects/{}/{}",
                &hex[..2],
                &hex[2..]
            )
        );
        assert_eq!(fetch.expect.sha256, Some(wanted.digest));
        assert_eq!(fetch.expect.size, Some(4));
        assert_eq!(
            joined(&base, &[MARKER]).as_str(),
            "https://data.corp.internal/fetchloom/fetchloom-mirror"
        );
    }
}
