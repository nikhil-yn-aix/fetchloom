//! License names as sources write them, mapped to SPDX identifiers where one fits.

use spdx::identifiers::LICENSES;

/// The SPDX identifier `text` names, in SPDX case: an identifier in any case, one with spaces
/// for hyphens such as `CC BY 4.0`, or a license's full name. Anything else comes back as
/// written.
#[must_use]
pub fn spdx(text: &str) -> String {
    let text = text.trim();
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
    }
}
