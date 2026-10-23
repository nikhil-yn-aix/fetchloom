//! Which licenses restrict what a dataset may be used for.

/// Whether `license` restricts use: no commercial use, no derivatives, or terms of its own
/// rather than a known open license. `license` is an SPDX id or the text a source states.
#[must_use]
pub fn restricts(license: &str) -> bool {
    let lower = license.to_ascii_lowercase();
    let segments = || lower.split(['-', '_', ' ', '.']);
    segments().any(|segment| segment == "nc" || segment == "nd")
        || lower.starts_with("licenseref-")
        || [
            "noncommercial",
            "non-commercial",
            "noderivatives",
            "no derivatives",
            "proprietary",
            "custom",
            "restricted",
            "other",
        ]
        .iter()
        .any(|word| lower.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_commercial_no_derivatives_and_custom_terms_restrict() {
        for license in [
            "CC-BY-NC-4.0",
            "CC-BY-NC-SA-4.0",
            "cc-by-nd-4.0",
            "CC BY-NC 3.0",
            "LicenseRef-data-use-agreement",
            "Proprietary",
            "other",
            "Custom terms of use",
            "Creative Commons Attribution Non-Commercial",
        ] {
            assert!(restricts(license), "{license}");
        }
    }

    #[test]
    fn open_licenses_do_not() {
        for license in [
            "CC-BY-4.0",
            "CC0-1.0",
            "MIT",
            "ODbL-1.0",
            "Apache-2.0",
            "cc-by-sa-4.0",
        ] {
            assert!(!restricts(license), "{license}");
        }
    }
}
