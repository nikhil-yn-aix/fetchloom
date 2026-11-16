//! hugging face through a mirror: `HF_ENDPOINT`, the variable hugging face's own tools read, sends
//! every request for huggingface.co to the mirror it names, while data.lock keeps huggingface.co
//! links, so a teammate without the variable gets the same files from huggingface.co.

use std::ffi::OsString;

use url::Url;

use crate::error::{Error, Kind};

/// where hugging face's own links point.
const HUB: &str = "https://huggingface.co";

/// the mirror of huggingface.co that `HF_ENDPOINT` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// the mirror's address without a `/` at its end, as `https://hf-mirror.com` or
    /// `http://lab.example:8080/hf`.
    base: Url,
}

impl Endpoint {
    /// the mirror `HF_ENDPOINT` names, or none when it is unset, empty or huggingface.co itself.
    ///
    /// # Errors
    ///
    /// a value that is no http or https address of a host, or one holding a login, a query or a
    /// fragment, which no mirror's address needs.
    pub(crate) fn read(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Option<Endpoint>, Error> {
        let Some(value) = var("HF_ENDPOINT").filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let invalid = |why: &str| {
            Error::new(Kind::Usage, format!("HF_ENDPOINT {why}")).help(
                "set HF_ENDPOINT to the address of a mirror of huggingface.co, as https://hf-mirror.com, or unset it to ask huggingface.co",
            )
        };
        let text = value.to_str().ok_or_else(|| invalid("is not text"))?;
        let Ok(mut base) = Url::parse(text.trim()) else {
            return Err(invalid(&format!("is \"{text}\", which is not an address")));
        };
        if !base.username().is_empty() || base.password().is_some() {
            // the value is not shown, since it holds a password
            return Err(invalid(
                "holds a login, which fetchloom never sends from an address: sign in to the mirror with `fetchloom auth login <host> --token`",
            ));
        }
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(invalid(&format!(
                "is \"{text}\", which is not the http or https address of a mirror"
            )));
        }
        let path = base.path().trim_end_matches('/').to_owned();
        base.set_path(&path);
        let endpoint = Endpoint { base };
        Ok((endpoint.base.as_str().trim_end_matches('/') != HUB).then_some(endpoint))
    }

    /// the mirror at `base`, for tests.
    #[cfg(test)]
    pub(crate) fn at(base: &str) -> Endpoint {
        let read = Endpoint::read(&|_| Some(base.into()));
        read.ok().flatten().expect("a mirror's address")
    }

    /// where a request for `url` goes: for a link to huggingface.co, the same path and query below
    /// the mirror.
    pub(crate) fn rewrite(&self, url: &Url) -> Option<Url> {
        if url.scheme() != "https"
            || url.host_str() != Some("huggingface.co")
            || url.port_or_known_default() != Some(443)
        {
            return None;
        }
        let mut mirrored = self.base.clone();
        mirrored.set_path(&format!("{}{}", self.prefix(), url.path()));
        mirrored.set_query(url.query());
        Some(mirrored)
    }

    /// whether `url` lies below the mirror's address: its origin, and a path inside the mirror's.
    pub(crate) fn holds(&self, url: &Url) -> bool {
        url.origin() == self.base.origin()
            && url
                .path()
                .strip_prefix(self.prefix())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    /// the huggingface.co link that `url`, below the mirror, stands for.
    pub(crate) fn written(&self, url: &Url) -> Option<Url> {
        if !self.holds(url) {
            return None;
        }
        let mut written = Url::parse(HUB).ok()?;
        let rest = &url.path()[self.prefix().len()..];
        written.set_path(if rest.is_empty() { "/" } else { rest });
        written.set_query(url.query());
        Some(written)
    }

    /// the mirror's host, with its port when it names one, as `fetchloom auth login` takes it.
    pub(crate) fn host(&self) -> String {
        let host = self.base.host_str().unwrap_or_default();
        match self.base.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        }
    }

    /// whether the mirror is asked over https, the only way a key goes to it.
    pub(crate) fn is_https(&self) -> bool {
        self.base.scheme() == "https"
    }

    /// the mirror's origin, which is trusted as a mirror a person named is.
    pub(crate) fn origin(&self) -> url::Origin {
        self.base.origin()
    }

    /// the path of the mirror's address, without a `/` at its end: empty for a mirror at a root.
    fn prefix(&self) -> &str {
        self.base.path().trim_end_matches('/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(value: &str) -> Result<Option<Endpoint>, Error> {
        Endpoint::read(&|name| (name == "HF_ENDPOINT").then(|| value.into()))
    }

    #[test]
    fn a_link_to_the_hub_goes_below_the_mirror_and_back() {
        for (base, link, mirrored) in [
            (
                "https://hf-mirror.example",
                "https://huggingface.co/api/datasets/x/y/revision/main?expand=sha",
                "https://hf-mirror.example/api/datasets/x/y/revision/main?expand=sha",
            ),
            (
                "http://10.0.0.5:8080/hf/",
                "https://huggingface.co/datasets/x/y/resolve/abc/a%20b.csv",
                "http://10.0.0.5:8080/hf/datasets/x/y/resolve/abc/a%20b.csv",
            ),
        ] {
            let endpoint = Endpoint::at(base);
            let link = Url::parse(link).unwrap();
            let rewritten = endpoint.rewrite(&link).unwrap();
            assert_eq!(rewritten.as_str(), mirrored);
            assert!(endpoint.holds(&rewritten));
            assert_eq!(endpoint.written(&rewritten), Some(link));
        }
    }

    #[test]
    fn only_the_hubs_own_origin_is_rewritten() {
        let endpoint = Endpoint::at("https://hf-mirror.example/hf");
        for other in [
            "http://huggingface.co/api/datasets/x/y",
            "https://huggingface.co:8443/api/datasets/x/y",
            "https://cdn-lfs.huggingface.co/x",
            "https://hf.co/datasets/x/y",
            "https://zenodo.org/api/records/1",
        ] {
            assert_eq!(
                endpoint.rewrite(&Url::parse(other).unwrap()),
                None,
                "{other}"
            );
        }
        for outside in [
            "https://hf-mirror.example/other/api",
            "https://hf-mirror.example/hfx/api",
            "http://hf-mirror.example/hf/api",
        ] {
            let outside = Url::parse(outside).unwrap();
            assert!(!endpoint.holds(&outside), "{outside}");
            assert_eq!(endpoint.written(&outside), None, "{outside}");
        }
    }

    #[test]
    fn the_variable_names_a_mirror_or_nothing() {
        assert_eq!(read("").unwrap(), None);
        assert_eq!(read("https://huggingface.co/").unwrap(), None);
        assert_eq!(read("https://huggingface.co").unwrap(), None);
        assert_eq!(
            read("https://hf-mirror.com/").unwrap().map(|e| e.host()),
            Some("hf-mirror.com".to_owned())
        );
        assert_eq!(
            read("http://192.168.1.9:8080").unwrap().map(|e| e.host()),
            Some("192.168.1.9:8080".to_owned())
        );
        for bad in [
            "hf-mirror.com",
            "ftp://hf-mirror.com",
            "https://hf-mirror.com/?a=b",
            "https://hf-mirror.com/#top",
            "file:///srv/hf",
        ] {
            let error = read(bad).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{bad}");
            assert!(error.message.starts_with("HF_ENDPOINT "), "{bad}");
        }
        let error = read("https://me:secret@hf-mirror.com").unwrap_err();
        assert!(!error.message.contains("secret"), "{}", error.message);
        assert!(error.message.contains("auth login"), "{}", error.message);
    }
}
