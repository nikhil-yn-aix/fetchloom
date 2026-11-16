//! an ftp link given to `add` or `get`. fetchloom does not speak ftp, but many ftp hosts serve the
//! same tree over https, so the link is asked once over https and taken as that link when it
//! serves.

use url::Url;

use super::Context;
use crate::error::{Error, Kind};
use crate::net::{Fetch, Probed};
use crate::shown::Shown;

/// the https link the ftp link `typed` stands for, as `ftp://ftp.ncbi.nlm.nih.gov/geo/x` gives
/// `https://ftp.ncbi.nlm.nih.gov/geo/x`: the same host and path, on https's own port, since an
/// ftp server's port is not its web server's. none for any other link.
pub(in crate::cli) fn https_of(typed: &str) -> Option<Url> {
    let ftp = Url::parse(typed.trim()).ok()?;
    if ftp.scheme() != "ftp" {
        return None;
    }
    let mut https = Url::parse("https://localhost/").ok()?;
    https.set_host(ftp.host_str()).ok()?;
    https.set_path(ftp.path());
    https.set_query(ftp.query());
    https.set_fragment(ftp.fragment());
    // a login is kept, so the link is refused as any link with one is
    https.set_username(ftp.username()).ok()?;
    https.set_password(ftp.password()).ok()?;
    Some(https)
}

/// the link `add` or `get` asks for `typed`: `typed` itself, or for an ftp link its https link,
/// once one request with no retry finds that the host serves it, which the output then says.
/// offline or in mirror-only mode no request may go, so the https link is taken as it is.
///
/// # Errors
///
/// `reference.unresolved` naming the host, when it serves no https copy, or a stop.
pub(in crate::cli) async fn over_https(context: &Context, typed: &str) -> Result<String, Error> {
    let Some(https) = https_of(typed) else {
        return Ok(typed.to_owned());
    };
    if !https.username().is_empty() || https.password().is_some() {
        return Ok(https.to_string());
    }
    if context.offline.enabled() || context.mirrors.mirrors.only {
        return Ok(https.to_string());
    }
    let net = context.net().with_retries(0);
    net.trust([https.origin()]);
    let host = crate::net::host(&https).to_owned();
    let failed = match net.probe(&Fetch::new(https.clone())).await? {
        Probed::File { .. } => {
            context.output.step(
                "Using",
                &format!("{}, since fetchloom does not speak ftp", Shown(&https)),
            );
            return Ok(https.to_string());
        }
        Probed::Answered { error, .. } | Probed::Unanswered(error) => error,
    };
    Err(Error::new(
        Kind::ReferenceUnresolved,
        format!(
            "{host} serves no https copy of {}, and fetchloom does not speak ftp",
            crate::shown::mask_query(typed.trim())
        ),
    )
    .detail(failed.message)
    .help(format!(
        "download it with an ftp client, then add the file it saved with `{}`",
        super::project::command("add <file>")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ftp_link_is_its_https_link_on_https_port() {
        for (ftp, https) in [
            (
                "ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/suppl/x.tar",
                Some("https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/suppl/x.tar"),
            ),
            (
                " FTP://ftp.example.org:2121/pub/a%20b.csv ",
                Some("https://ftp.example.org/pub/a%20b.csv"),
            ),
            ("https://example.org/a.csv", None),
            ("s3://bucket/a.csv", None),
            ("zenodo:3608990", None),
        ] {
            assert_eq!(https_of(ftp).map(String::from).as_deref(), https, "{ftp}");
        }
    }
}
