//! License names as sources write them, mapped to SPDX identifiers where one fits.

use spdx::identifiers::LICENSES;

/// The SPDX identifier `text` names, in SPDX case: an identifier in any case, one with spaces
/// for hyphens such as `CC BY 4.0`, a license's full name, its page on spdx.org, or a Creative
/// Commons license URL. Anything else comes back as written.
#[must_use]
pub fn spdx(text: &str) -> String {
    let text = text.trim();
    let named = from_url(text);
    let text = named.as_deref().unwrap_or(text);
    let hyphenated: String = text
        .chars()
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect();
    LICENSES
        .iter()
        .find(|license| {
            license.name.eq_ignore_ascii_case(text)
                || license.name.eq_ignore_ascii_case(&hyphenated)
                || license.full_name.eq_ignore_ascii_case(text)
        })
        .map_or_else(|| text.to_owned(), |license| license.name.to_owned())
}

/// The identifier a license URL spells out, when it is an spdx.org page or a Creative Commons
/// license or dedication.
fn from_url(text: &str) -> Option<String> {
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))?;
    let rest = rest
        .strip_prefix("www.")
        .unwrap_or(rest)
        .trim_end_matches('/');
    if let Some(page) = rest.strip_prefix("spdx.org/licenses/") {
        return Some(
            page.trim_end_matches(".html")
                .trim_end_matches(".json")
                .to_owned(),
        );
    }
    let path = rest.strip_prefix("creativecommons.org/")?;
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["licenses", kind, version, ..] => Some(format!("CC-{kind}-{version}")),
        ["publicdomain", "zero", version, ..] => Some(format!("CC0-{version}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_spaced_names_and_full_names_map_to_spdx_case() {
        assert_eq!(spdx("cc-by-4.0"), "CC-BY-4.0");
        assert_eq!(spdx("CC BY 4.0"), "CC-BY-4.0");
        assert_eq!(spdx("CC0 1.0"), "CC0-1.0");
        assert_eq!(spdx("MIT License"), "MIT");
        assert_eq!(spdx(" apache-2.0 "), "Apache-2.0");
        assert_eq!(spdx("Custom terms of use"), "Custom terms of use");
        assert_eq!(
            spdx("http://creativecommons.org/publicdomain/zero/1.0"),
            "CC0-1.0"
        );
        assert_eq!(
            spdx("https://creativecommons.org/licenses/by-nc-sa/4.0/"),
            "CC-BY-NC-SA-4.0"
        );
        assert_eq!(spdx("https://spdx.org/licenses/CC0-1.0.html"), "CC0-1.0");
        assert_eq!(
            spdx("https://example.org/terms"),
            "https://example.org/terms"
        );
    }
}
