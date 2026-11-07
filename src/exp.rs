//! switches are temporary: read once in `cli::run` and passed down as values.
//! remove each switch with its code when the a/b decides.
//! remove this mechanism once no switch remains.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;

/// the experiment settings for one run, keyed without `FETCHLOOM_EXP_`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Experiments(Arc<BTreeMap<String, String>>);

impl Experiments {
    /// keeps only prefixed names and UTF-8 values from the supplied variables.
    pub(crate) fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        let settings = vars
            .into_iter()
            .filter_map(|(name, value)| {
                let name = name.to_str()?.strip_prefix("FETCHLOOM_EXP_")?.to_owned();
                Some((name, value.into_string().ok()?))
            })
            .collect();
        Self(Arc::new(settings))
    }

    /// sets one unprefixed name for a test, leaving any earlier clone unchanged.
    #[cfg(test)]
    pub(crate) fn with(mut self, name: &str, value: &str) -> Self {
        Arc::make_mut(&mut self.0).insert(name.to_owned(), value.to_owned());
        self
    }

    /// whether this run has no experiment settings.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// whether `name` is present, nonempty and not `0`.
    #[allow(dead_code, reason = "switch consumers arrive during the a/b period")]
    pub(crate) fn on(&self, name: &str) -> bool {
        self.0
            .get(name)
            .is_some_and(|value| !value.is_empty() && value != "0")
    }

    /// the trimmed decimal value of `name`, when it fits in a `u64`.
    #[allow(
        dead_code,
        reason = "numeric switch consumers arrive during the a/b period"
    )]
    pub(crate) fn number(&self, name: &str) -> Option<u64> {
        self.0.get(name)?.trim().parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experiments_default_to_empty() {
        let exp = Experiments::default();
        assert!(exp.is_empty());
        assert!(!exp.on("DEMO"));
        assert_eq!(exp.number("DEMO"), None);
    }

    #[test]
    fn experiments_from_vars_keep_only_prefixed_names() {
        let exp = Experiments::from_vars(
            [
                ("FETCHLOOM_EXP_Z", "last"),
                ("FETCHLOOM_EXP_A", "first"),
                ("FETCHLOOM_EXP_COUNT", " 42 "),
                ("FETCHLOOM_EXP_DISABLED", "0"),
                ("FETCHLOOM_EXP_EMPTY", ""),
                ("FETCHLOOM_JOBS", "8"),
                ("OTHER_FETCHLOOM_EXP_HIDDEN", "1"),
                ("FETCHLOOM_EXPERIMENT_HIDDEN", "1"),
            ]
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        assert!(!exp.is_empty());
        assert!(exp.on("A"));
        assert_eq!(exp.number("COUNT"), Some(42));
        assert!(!exp.on("DISABLED"));
        assert!(!exp.on("EMPTY"));
        assert!(!exp.on("FETCHLOOM_EXP_A"));
        assert_eq!(
            exp.0.keys().map(String::as_str).collect::<Vec<_>>(),
            ["A", "COUNT", "DISABLED", "EMPTY", "Z"]
        );
    }

    #[test]
    fn experiments_keep_the_last_value_for_a_name() {
        let exp = Experiments::from_vars([
            ("FETCHLOOM_EXP_DEMO".into(), "1".into()),
            ("FETCHLOOM_EXP_DEMO".into(), "0".into()),
        ]);
        assert!(!exp.is_empty());
        assert!(!exp.on("DEMO"));
        assert_eq!(exp.number("DEMO"), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn experiments_drop_non_utf8_names_and_values() {
        use std::os::unix::ffi::OsStringExt as _;

        let exp = Experiments::from_vars([
            ("FETCHLOOM_EXP_GOOD".into(), "1".into()),
            ("FETCHLOOM_EXP_BAD".into(), OsString::from_vec(vec![0xff])),
            (
                OsString::from_vec(b"FETCHLOOM_EXP_\xff".to_vec()),
                "1".into(),
            ),
        ]);
        assert!(exp.on("GOOD"));
        assert_eq!(exp.0.len(), 1);
    }

    #[test]
    fn experiments_on_uses_presence_without_trimming() {
        for (value, expected) in [
            ("", false),
            ("0", false),
            ("1", true),
            ("false", true),
            (" 0 ", true),
            (" ", true),
        ] {
            let exp = Experiments::default().with("DEMO", value);
            assert_eq!(exp.on("DEMO"), expected, "{value:?}");
        }
    }

    #[test]
    fn experiments_numbers_are_trimmed_unsigned_decimals() {
        for (value, expected) in [
            ("", None),
            (" ", None),
            ("0", Some(0)),
            ("1", Some(1)),
            (" 42 \t\n", Some(42)),
            ("other", None),
            ("-1", None),
            ("1.5", None),
            ("1_000", None),
            ("0x10", None),
            ("1 2", None),
            ("18446744073709551615", Some(u64::MAX)),
            ("18446744073709551616", None),
        ] {
            let exp = Experiments::default().with("DEMO", value);
            assert_eq!(exp.number("DEMO"), expected, "{value:?}");
        }
    }

    #[test]
    fn experiments_clones_share_values_until_a_test_changes_one() {
        let original = Experiments::default().with("COUNT", "3");
        let shared = original.clone();
        assert!(Arc::ptr_eq(&original.0, &shared.0));
        let changed = shared.with("COUNT", "7").with("DEMO", "1");
        assert_eq!(original.number("COUNT"), Some(3));
        assert!(!original.on("DEMO"));
        assert_eq!(changed.number("COUNT"), Some(7));
        assert!(changed.on("DEMO"));
    }

    #[test]
    fn experiments_debug_lists_sorted_settings() {
        let exp = Experiments::default().with("Z", "last").with("A", "first");
        assert_eq!(
            format!("{exp:?}"),
            r#"Experiments({"A": "first", "Z": "last"})"#
        );
    }
}
