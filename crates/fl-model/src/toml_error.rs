use std::ops::Range;

/// A TOML reading error in fetchloom's words: a message, where it points and what may help.
pub(crate) struct Explained {
    pub(crate) message: String,
    pub(crate) span: Option<Range<usize>>,
    pub(crate) help: Option<String>,
}

/// Explains an error from reading a TOML file into one of the model's types.
pub(crate) fn explain_error(err: &toml::de::Error) -> Explained {
    let (message, help) = explain(err.message());
    Explained {
        message,
        span: err.span(),
        help,
    }
}

fn explain(message: &str) -> (String, Option<String>) {
    if let Some(key) = message.strip_prefix("missing field ") {
        return (format!("missing key {key}"), None);
    }
    let Some(rest) = message.strip_prefix("unknown field `") else {
        return (message.to_owned(), None);
    };
    let Some((key, expected)) = rest.split_once('`') else {
        return (message.to_owned(), None);
    };
    let valid: Vec<&str> = expected.split('`').skip(1).step_by(2).collect();
    let (message, help) = unknown_key(key, &valid);
    (message, Some(help))
}

/// The message for a key outside `valid`, and help naming the closest valid key.
pub(crate) fn unknown_key(key: &str, valid: &[&str]) -> (String, String) {
    let help = valid
        .iter()
        .min_by_key(|candidate| distance(key, candidate))
        .map_or_else(
            || "this table takes no keys".to_owned(),
            |closest| format!("the closest valid key is `{closest}`"),
        );
    (format!("unknown key `{key}`"), help)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, a_char) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, b_char) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(a_char != *b_char);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_closest_valid_key() {
        let (message, help) =
            explain("unknown field `selct`, expected one of `ref`, `select`, `n`");
        assert_eq!(message, "unknown key `selct`");
        assert_eq!(help.unwrap(), "the closest valid key is `select`");
        let (_, help) = explain("unknown field `nme`, expected `datasets` or `name`");
        assert_eq!(help.unwrap(), "the closest valid key is `name`");
        let (_, help) = explain("unknown field `x`, expected `ref`");
        assert_eq!(help.unwrap(), "the closest valid key is `ref`");
    }

    #[test]
    fn says_when_a_table_takes_no_keys() {
        let (message, help) = explain("unknown field `x`, there are no fields");
        assert_eq!(message, "unknown key `x`");
        assert_eq!(help.unwrap(), "this table takes no keys");
    }

    #[test]
    fn keeps_other_messages() {
        let (message, help) = explain("missing field `ref`");
        assert_eq!(message, "missing key `ref`");
        assert!(help.is_none());
        let (message, help) = explain("duplicate key");
        assert_eq!(message, "duplicate key");
        assert!(help.is_none());
    }

    #[test]
    fn edit_distance_counts_insertions_deletions_and_substitutions() {
        assert_eq!(distance("select", "select"), 0);
        assert_eq!(distance("selct", "select"), 1);
        assert_eq!(distance("sealect", "select"), 1);
        assert_eq!(distance("salect", "select"), 1);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
    }
}
