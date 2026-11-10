//! the certificates a client trusts besides the system's, on linux and the other systems whose
//! verifier reads a certificate bundle. a container built without one would stop every download,
//! so fetchloom carries mozilla's. macos and windows ask the system's own verifier instead.

use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;

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

/// what to trust, given how many roots could be read and the variables `var` reads. a variable
/// someone set is never overruled: certificates it names that cannot be read are an error, not a
/// reason to trust other roots.
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

/// the roots a client adds to the system's: none where the system has its own, else mozilla's.
/// read once a run, since each client would read the same store again.
///
/// # Errors
///
/// `usage` when `SSL_CERT_FILE` or `SSL_CERT_DIR` names certificates that cannot be read.
pub(super) fn extra() -> Result<Vec<reqwest::Certificate>, Error> {
    static EXTRA: OnceLock<Result<Vec<reqwest::Certificate>, Error>> = OnceLock::new();
    EXTRA.get_or_init(read).clone()
}

fn read() -> Result<Vec<reqwest::Certificate>, Error> {
    let loaded = rustls_native_certs::load_native_certs();
    // the same count the verifier keeps, which skips certificates it cannot parse
    let (found, _) = rustls::RootCertStore::empty().add_parsable_certificates(loaded.certs);
    match choose(found, &|name| std::env::var_os(name)) {
        Roots::System => Ok(Vec::new()),
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
            Ok(carried())
        }
    }
}

/// mozilla's roots, as fetchloom carries them.
fn carried() -> Vec<reqwest::Certificate> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok())
        .collect()
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
        let carried = carried();
        assert!(carried.len() > 100, "{}", carried.len());
        assert_eq!(
            carried.len(),
            webpki_root_certs::TLS_SERVER_ROOT_CERTS.len()
        );
    }
}
