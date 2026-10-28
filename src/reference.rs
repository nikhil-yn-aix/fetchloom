//! references: where data comes from, as a person writes it.

use url::Url;

use crate::error::{Error, Kind};

pub(crate) enum Reference {
    Url(Url),
}

impl Reference {
    pub(crate) fn parse(text: &str) -> Result<Reference, Error> {
        let text = text.trim();
        let lower = text.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!("fetchloom cannot read \"{text}\" as a link"),
            )
            .help("paste a link that starts with https://"));
        }
        let url = Url::parse(text).map_err(|err| {
            Error::new(
                Kind::ReferenceInvalid,
                format!("\"{text}\" is not a valid link: {err}"),
            )
            .help("copy the link again from the browser's address bar")
        })?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                "this link carries a user name or password, which would end up in files and logs",
            )
            .help("remove the login part from the link"));
        }
        Ok(Reference::Url(url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_parse_and_other_text_is_refused() {
        assert!(matches!(
            Reference::parse(" https://zenodo.org/x "),
            Ok(Reference::Url(_))
        ));
        assert!(matches!(
            Reference::parse("HTTP://example.org/x"),
            Ok(Reference::Url(_))
        ));
        assert!(Reference::parse("zenodo is great").is_err());
        assert!(Reference::parse("https://").is_err());
        assert!(Reference::parse("https://user:secret@example.org/x").is_err());
    }
}
