//! Suggestions for a word that names nothing: the closest of the words that do.

/// The candidate closest to `word` by edit distance, when it is close enough to be the word
/// that was meant: at most one edit for words of up to four characters, two for longer ones.
#[must_use]
pub fn closest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let allowed = if word.chars().count() <= 4 { 1 } else { 2 };
    candidates
        .into_iter()
        .map(|candidate| (distance(word, candidate), candidate))
        .filter(|(edits, _)| *edits <= allowed)
        .min_by_key(|(edits, _)| *edits)
        .map(|(_, candidate)| candidate)
}

/// The edit distance between `a` and `b`: insertions, deletions and substitutions, with a swap
/// of two neighbouring characters counted as one edit.
#[must_use]
pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![(0..=b.len()).collect::<Vec<usize>>()];
    for i in 1..=a.len() {
        let mut row = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let above = &rows[i - 1];
            let substitution = above[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut best = substitution.min(above[j] + 1).min(row[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            row[j] = best;
        }
        rows.push(row);
    }
    rows[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_counts_insertions_deletions_substitutions_and_swaps() {
        assert_eq!(distance("select", "select"), 0);
        assert_eq!(distance("selct", "select"), 1);
        assert_eq!(distance("sealect", "select"), 1);
        assert_eq!(distance("salect", "select"), 1);
        assert_eq!(distance("snyc", "sync"), 1);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("ab", "ba"), 1);
        assert_eq!(distance("bacdef", "abcdef"), 1);
        assert_eq!(distance("abcdfe", "abcdef"), 1);
        assert_eq!(distance("abcdefhg", "abcdefgh"), 1);
    }

    #[test]
    fn only_a_close_candidate_is_suggested() {
        assert_eq!(closest("snyc", ["status", "sync"]), Some("sync"));
        assert_eq!(closest("ires", ["iris", "eeg"]), Some("iris"));
        assert_eq!(closest("mnist", ["iris", "eeg"]), None);
        assert_eq!(closest("eegg", ["eeg", "egg"]), Some("eeg"));
        assert_eq!(
            closest("motorimagery", ["motor-imagery"]),
            Some("motor-imagery")
        );
        assert_eq!(closest("x", []), None);
        assert_eq!(closest("abcd", ["abxy"]), None);
        assert_eq!(closest("abcde", ["abxye"]), Some("abxye"));
    }
}
