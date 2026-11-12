//! the certificates a client trusts where the verifier reads a bundle: the system's, else the
//! mozilla roots fetchloom carries, so a container built without any still downloads.

use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;

use rustls::RootCertStore;

use crate::error::{Error, Kind};

/// where a client's trusted roots come from.
#[derive(Debug, PartialEq, Eq)]
enum Roots {
    /// the system's store, or the files `SSL_CERT_FILE` and `SSL_CERT_DIR` name, holds some.
    System,
    /// the system holds none and no variable names any, so the carried mozilla roots stand in.
    Carried,
    /// a variable names certificates, and none could be read there.
    Unreadable {
        variable: &'static str,
        value: OsString,
    },
}

/// what to trust, given how many roots could be read. a variable someone set is never overruled:
/// certificates it names that cannot be read are an error, not a reason to trust other roots.
fn choose(found: usize, var: &dyn Fn(&str) -> Option<OsString>) -> Roots {
    if found > 0 {
        return Roots::System;
    }
    for variable in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
        if let Some(value) = var(variable).filter(|value| !value.is_empty()) {
            return Roots::Unreadable { variable, value };
        }
    }
    Roots::Carried
}

/// the TLS configuration of a client, offering HTTP/2 too unless `http1_only`. every client of a
/// run shares one root store and verifier, since each of its own would read the whole store again.
///
/// # Errors
///
/// `usage` when `SSL_CERT_FILE` or `SSL_CERT_DIR` names certificates that cannot be read.
pub(super) fn tls(http1_only: bool) -> Result<rustls::ClientConfig, Error> {
    static SHARED: OnceLock<Result<rustls::ClientConfig, Error>> = OnceLock::new();
    let mut config = SHARED.get_or_init(configured).clone()?;
    config.alpn_protocols = if http1_only {
        vec![b"http/1.1".to_vec()]
    } else {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    };
    Ok(config)
}

/// the configuration reqwest would build, around the roots [`read`] finds.
fn configured() -> Result<rustls::ClientConfig, Error> {
    let roots = read()?;
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| std::sync::Arc::new(rustls::crypto::ring::default_provider()));
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(rustls::ALL_VERSIONS)
        .map_err(|err| {
            Error::new(
                Kind::NetworkUnreachable,
                format!("could not start the network client: {err}"),
            )
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(config)
}

fn read() -> Result<RootCertStore, Error> {
    let loaded = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    // parsable ones only, as the system verifier keeps them
    let (found, _) = roots.add_parsable_certificates(loaded.certs);
    match choose(found, &|name| std::env::var_os(name)) {
        Roots::System => Ok(roots),
        Roots::Unreadable { variable, value } => {
            let why = loaded
                .errors
                .first()
                .map_or_else(String::new, |error| format!(": {error}"));
            Err(Error::new(
                Kind::Usage,
                format!(
                    "{variable} names {}, which holds no certificates fetchloom can read{why}",
                    Path::new(&value).display()
                ),
            )
            .help(format!(
                "unset {variable} to use the system's certificates, or set it to a file of PEM certificates"
            )))
        }
        Roots::Carried => {
            tracing::debug!(
                "this system has no certificates, so the mozilla roots fetchloom carries are trusted"
            );
            carry(&mut roots);
            Ok(roots)
        }
    }
}

/// adds mozilla's roots, as fetchloom carries them, to `roots`: how many it took.
fn carry(roots: &mut RootCertStore) -> usize {
    let (added, _) =
        roots.add_parsable_certificates(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs = pairs.to_vec();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn roots_the_system_has_are_used_whatever_the_variables_say() {
        assert_eq!(choose(140, &env(&[])), Roots::System);
        assert_eq!(
            choose(1, &env(&[("SSL_CERT_FILE", "/etc/proxy.pem")])),
            Roots::System
        );
    }

    #[test]
    fn a_system_without_roots_trusts_the_carried_ones() {
        assert_eq!(choose(0, &env(&[])), Roots::Carried);
        // an empty variable names nothing
        assert_eq!(choose(0, &env(&[("SSL_CERT_FILE", "")])), Roots::Carried);
    }

    #[test]
    fn a_variable_naming_unreadable_roots_is_never_overruled() {
        assert_eq!(
            choose(0, &env(&[("SSL_CERT_FILE", "/nowhere.pem")])),
            Roots::Unreadable {
                variable: "SSL_CERT_FILE",
                value: OsString::from("/nowhere.pem"),
            }
        );
        assert_eq!(
            choose(0, &env(&[("SSL_CERT_DIR", "/empty")])),
            Roots::Unreadable {
                variable: "SSL_CERT_DIR",
                value: OsString::from("/empty"),
            }
        );
    }

    #[test]
    fn the_carried_roots_are_all_usable() {
        let carried = carry(&mut RootCertStore::empty());
        assert!(carried > 100, "{carried}");
        assert_eq!(carried, webpki_root_certs::TLS_SERVER_ROOT_CERTS.len());
    }
}
