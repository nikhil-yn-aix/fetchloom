//! whether the proxy that carries a request can be reached, so a proxy that is down is named
//! rather than the site behind it, and found in seconds rather than after every retry.

use std::io;
use std::time::Duration;

use url::Url;

use super::{Proxies, Unknown, is_unknown};

/// how long a request through a proxy may go without an answer before the proxy is checked.
const GRACE: Duration = Duration::from_secs(3);
/// how long a connection to a proxy may take to open when it is checked. a proxy on this
/// machine's network opens one in well under a second, even across a slow link.
const PROBE: Duration = Duration::from_secs(5);

/// the proxy that carries a request, as messages name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Carrier {
    /// its scheme, host and port, never a login.
    pub(crate) shown: String,
    host: String,
    port: u16,
    /// the variable that names it, none when the system's proxy settings do.
    pub(crate) variable: Option<&'static str>,
}

/// what this run decided about a proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// a connection to it opened, or a request through it was answered.
    Reached,
    /// no connection to it opened, for this reason.
    Unreachable(String),
}

/// what one check of a proxy found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Probe {
    Opened,
    /// no connection opened, for this reason.
    Failed(String),
    /// looking up the proxy's name failed for now, which says nothing about the proxy.
    Unsure,
}

impl Proxies {
    /// the proxy that carries requests for `url`, none when they go straight to its host.
    pub(crate) fn carrier(&self, url: &Url) -> Option<Carrier> {
        let uri = url.as_str().parse::<http::Uri>().ok()?;
        let intercept = self.matcher.intercept(&uri)?;
        let proxy = intercept.uri();
        let port = proxy
            .port_u16()
            .unwrap_or(match proxy.scheme_str().unwrap_or("http") {
                "http" => 80,
                "https" => 443,
                _ => 1080,
            });
        // the matcher reads the variable for the link's scheme first, then ALL_PROXY
        let own = if url.scheme() == "https" {
            "HTTPS_PROXY"
        } else {
            "HTTP_PROXY"
        };
        let variable = [own, "ALL_PROXY"].into_iter().find_map(|wanted| {
            self.named
                .iter()
                .find(|(name, named)| name.eq_ignore_ascii_case(wanted) && named == proxy)
                .map(|(name, _)| *name)
        });
        Some(Carrier {
            shown: super::shown(proxy)?,
            host: proxy
                .host()?
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_ascii_lowercase(),
            port,
            variable,
        })
    }

    /// why the proxy cannot be reached, as its check found once this run, none when a
    /// connection to it opened or the check could not tell.
    pub(crate) async fn unreachable(&self, carrier: &Carrier) -> Option<String> {
        let verdict = match self.verdict(carrier) {
            Some(known) => known,
            None => match self.probe(carrier).await {
                Probe::Opened => self.decide(carrier, Verdict::Reached),
                Probe::Failed(reason) => self.decide(carrier, Verdict::Unreachable(reason)),
                Probe::Unsure => return None,
            },
        };
        match verdict {
            Verdict::Reached => None,
            Verdict::Unreachable(reason) => Some(reason),
        }
    }

    /// waits while a request through `carrier` has no answer. once it has waited [`GRACE`], the
    /// proxy is checked, and this ends only with why the proxy cannot be reached. with a proxy
    /// that answers, the request is left to end on its own.
    pub(crate) async fn watch(&self, carrier: &Carrier) -> String {
        if self.verdict(carrier).is_none() {
            tokio::time::sleep(GRACE).await;
        }
        match self.unreachable(carrier).await {
            Some(reason) => reason,
            None => std::future::pending().await,
        }
    }

    /// records that a request through `carrier` was answered, so the proxy needs no check.
    pub(crate) fn reached(&self, carrier: &Carrier) {
        self.decide(carrier, Verdict::Reached);
    }

    fn verdict(&self, carrier: &Carrier) -> Option<Verdict> {
        self.checked()
            .get(&(carrier.host.clone(), carrier.port))
            .cloned()
    }

    /// keeps the first verdict about `carrier`, and gives back the one kept.
    fn decide(&self, carrier: &Carrier, verdict: Verdict) -> Verdict {
        self.checked()
            .entry((carrier.host.clone(), carrier.port))
            .or_insert(verdict)
            .clone()
    }

    fn checked(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<(String, u16), Verdict>> {
        self.checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn probe(&self, carrier: &Carrier) -> Probe {
        #[cfg(test)]
        if let Some(probe) = &self.test_probe {
            return tokio::time::timeout(PROBE, probe(carrier.host.clone(), carrier.port))
                .await
                .unwrap_or_else(|_| timed_out());
        }
        tokio::time::timeout(PROBE, connect(&carrier.host, carrier.port))
            .await
            .unwrap_or_else(|_| timed_out())
    }
}

fn timed_out() -> Probe {
    Probe::Failed(format!(
        "no connection to it opened within {} s",
        PROBE.as_secs()
    ))
}

/// opens a connection to `host` at `port` and closes it again.
async fn connect(host: &str, port: u16) -> Probe {
    let addresses: Vec<_> = match tokio::net::lookup_host((host, port)).await {
        Ok(found) => found.collect(),
        Err(err) if is_unknown(&err) => return Probe::Failed(Unknown(host.to_owned()).to_string()),
        Err(_) => return Probe::Unsure,
    };
    let mut failed = Probe::Failed(format!("{host} has no address"));
    for address in addresses {
        match tokio::net::TcpStream::connect(address).await {
            Ok(_) => return Probe::Opened,
            Err(err) => failed = Probe::Failed(why(&err)),
        }
    }
    failed
}

/// why a connection did not open, in the same words on every system.
pub(super) fn why(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::ConnectionRefused => "it refused the connection".to_owned(),
        io::ErrorKind::NetworkUnreachable | io::ErrorKind::HostUnreachable => {
            "this machine has no route to it".to_owned()
        }
        io::ErrorKind::TimedOut => "no connection to it opened in time".to_owned(),
        _ => crate::error::said(err),
    }
}
