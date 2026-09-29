//! Filters: which files of a listing a dataset keeps.

use std::fmt;
use std::str::FromStr;

use globset::{Candidate, Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Deserializer};

use crate::canonical::Encoder;
use crate::error::ErrorKind;
use crate::path::DataPath;
use crate::text;
use crate::units::{Size, count};

/// A glob matched against the whole path of a file inside a dataset.
///
/// `*` and `?` never cross `/`, `**` spans any number of whole components including none, `[...]`
/// and `[!...]` are classes, `{a,b}` are alternatives and `\` escapes, on every platform. Matching
/// is case sensitive.
#[derive(Clone, Debug)]
pub struct Pattern {
    text: String,
    glob: Glob,
}

impl Pattern {
    /// The glob as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl FromStr for Pattern {
    type Err = FilterError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let glob = GlobBuilder::new(text)
            .literal_separator(true)
            .backslash_escape(true)
            .case_insensitive(false)
            .empty_alternates(false)
            .allow_unclosed_class(false)
            .build()
            .map_err(|err| FilterError::Glob {
                pattern: text.to_owned(),
                reason: err.kind().to_string(),
            })?;
        Ok(Self {
            text: text.to_owned(),
            glob,
        })
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        text::deserialize(deserializer)
    }
}

/// A deterministic sample: every machine picks the same files for the same seed.
///
/// Each path gets a value from BLAKE3 under the context `fetchloom sample v1` over the seed and
/// the path; the first eight bytes, little endian, decide.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
#[serde(try_from = "SampleFields")]
pub enum Sample {
    /// Keep the `n` files with the lowest values.
    Count {
        /// How many files to keep.
        n: u64,
        /// The seed.
        seed: u64,
    },
    /// Keep each file whose value falls below `fraction` of the range.
    Fraction {
        /// The share of files to keep, above 0 and at most 1.
        fraction: f64,
        /// The seed.
        seed: u64,
    },
}

impl Sample {
    fn checked(self) -> Result<Self, FilterError> {
        match self {
            Self::Count { n: 0, .. } => Err(FilterError::EmptySample),
            Self::Fraction { fraction, .. } if !(fraction > 0.0 && fraction <= 1.0) => {
                Err(FilterError::Fraction(fraction))
            }
            _ => Ok(self),
        }
    }

    fn value(seed: u64, path: &DataPath) -> u64 {
        let hash = Encoder::new("fetchloom sample v1")
            .u64(seed)
            .str(path.as_str())
            .finish();
        let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = hash;
        u64::from_le_bytes([b0, b1, b2, b3, b4, b5, b6, b7])
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SampleFields {
    n: Option<u64>,
    fraction: Option<f64>,
    #[serde(default)]
    seed: u64,
}

impl TryFrom<SampleFields> for Sample {
    type Error = FilterError;

    fn try_from(fields: SampleFields) -> Result<Self, Self::Error> {
        let sample = match fields {
            SampleFields {
                n: Some(n),
                fraction: None,
                seed,
            } => Self::Count { n, seed },
            SampleFields {
                n: None,
                fraction: Some(fraction),
                seed,
            } => Self::Fraction { fraction, seed },
            SampleFields { n: Some(_), .. } => return Err(FilterError::SampleBoth),
            SampleFields { .. } => return Err(FilterError::SampleNeither),
        };
        sample.checked()
    }
}

/// The filters of a dataset, compiled: select, exclude, a size limit and a sample, applied in
/// that order.
#[derive(Clone, Debug)]
pub struct Filters {
    select: Vec<Pattern>,
    exclude: Vec<Pattern>,
    selected: GlobSet,
    excluded: GlobSet,
    max_file_size: Option<Size>,
    sample: Option<Sample>,
}

impl Filters {
    /// Compiles the filters. An empty `select` keeps every file.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] for a sample of zero files or a fraction outside (0, 1], or when
    /// the patterns cannot be combined.
    pub fn new(
        select: Vec<Pattern>,
        exclude: Vec<Pattern>,
        max_file_size: Option<Size>,
        sample: Option<Sample>,
    ) -> Result<Self, FilterError> {
        let sample = sample.map(Sample::checked).transpose()?;
        Ok(Self {
            selected: set(&select)?,
            excluded: set(&exclude)?,
            select,
            exclude,
            max_file_size,
            sample,
        })
    }

    /// Applies the filters to a listing of paths and sizes, where a size may be unknown.
    ///
    /// A file of unknown size passes the size limit. A pattern is unmatched when it matches no
    /// path of the whole listing, whatever the other filters did.
    pub fn apply<'p>(
        &self,
        files: impl IntoIterator<Item = (&'p DataPath, Option<u64>)>,
    ) -> Selection<'_> {
        let mut select_hits = vec![false; self.select.len()];
        let mut exclude_hits = vec![false; self.exclude.len()];
        let mut indices = Vec::new();
        let mut survivors = Vec::new();
        let mut examples = Vec::new();
        let mut considered = 0;
        for (index, (path, size)) in files.into_iter().enumerate() {
            considered += 1;
            if examples.len() < 3 {
                examples.push(path.clone());
            }
            let candidate = Candidate::from_bytes(path.as_str());
            let selected = hit(&self.selected, &candidate, &mut indices, &mut select_hits);
            let excluded = hit(&self.excluded, &candidate, &mut indices, &mut exclude_hits);
            let fits = match (self.max_file_size, size) {
                (Some(limit), Some(size)) => size <= limit.bytes(),
                _ => true,
            };
            if (self.select.is_empty() || selected) && !excluded && fits {
                survivors.push((index, path));
            }
        }
        let unmatched = self
            .select
            .iter()
            .zip(&select_hits)
            .chain(self.exclude.iter().zip(&exclude_hits))
            .filter(|(_, hit)| !**hit)
            .map(|(pattern, _)| pattern)
            .collect();
        Selection {
            kept: self.sample_of(survivors),
            unmatched,
            considered,
            examples,
        }
    }

    fn sample_of(&self, survivors: Vec<(usize, &DataPath)>) -> Vec<usize> {
        let mut kept: Vec<usize> = match self.sample {
            None => return survivors.into_iter().map(|(index, _)| index).collect(),
            Some(Sample::Fraction { fraction, seed }) => {
                let limit = share_of_range(fraction);
                survivors
                    .into_iter()
                    .filter(|(_, path)| u128::from(Sample::value(seed, path)) < limit)
                    .map(|(index, _)| index)
                    .collect()
            }
            Some(Sample::Count { n, seed }) => {
                let mut ranked: Vec<(u64, &DataPath, usize)> = survivors
                    .into_iter()
                    .map(|(index, path)| (Sample::value(seed, path), path, index))
                    .collect();
                ranked.sort_unstable();
                ranked.truncate(usize::try_from(n).unwrap_or(usize::MAX));
                ranked.into_iter().map(|(_, _, index)| index).collect()
            }
        };
        kept.sort_unstable();
        kept
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a fraction in (0, 1] times 2^64 is a positive integer part at most 2^64"
)]
fn share_of_range(fraction: f64) -> u128 {
    (fraction * 18_446_744_073_709_551_616.0) as u128
}

fn set(patterns: &[Pattern]) -> Result<GlobSet, FilterError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(pattern.glob.clone());
    }
    builder
        .build()
        .map_err(|err| FilterError::Set(err.to_string()))
}

fn hit(
    set: &GlobSet,
    candidate: &Candidate<'_>,
    indices: &mut Vec<usize>,
    hits: &mut [bool],
) -> bool {
    set.matches_candidate_into(candidate, indices);
    for &index in indices.iter() {
        hits[index] = true;
    }
    !indices.is_empty()
}

/// What the filters kept, and which patterns matched nothing.
#[derive(Debug)]
pub struct Selection<'f> {
    kept: Vec<usize>,
    unmatched: Vec<&'f Pattern>,
    considered: usize,
    examples: Vec<DataPath>,
}

impl Selection<'_> {
    /// The indices of the kept files in listing order.
    #[must_use]
    pub fn kept(&self) -> &[usize] {
        &self.kept
    }

    /// The select and exclude patterns that matched no file of the listing.
    #[must_use]
    pub fn unmatched(&self) -> &[&Pattern] {
        &self.unmatched
    }

    /// The kept indices, or an error when any pattern matched nothing, since a filter that
    /// silently matches nothing looks like success.
    ///
    /// # Errors
    ///
    /// Returns [`Unmatched`] naming every pattern that matched nothing.
    pub fn refuse_unmatched(self) -> Result<Vec<usize>, Unmatched> {
        if self.unmatched.is_empty() {
            return Ok(self.kept);
        }
        Err(Unmatched {
            patterns: self.unmatched.iter().map(|p| p.text.clone()).collect(),
            considered: self.considered,
            examples: self.examples,
        })
    }
}

/// Patterns that matched no file of a listing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct Unmatched {
    /// The patterns, in the order they were written.
    pub patterns: Vec<String>,
    /// How many files the listing held.
    pub considered: usize,
    /// Up to three paths of the listing, to show what paths look like.
    pub examples: Vec<DataPath>,
}

impl Unmatched {
    /// The kind this error is reported as.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        ErrorKind::SelectionEmpty
    }
}

impl fmt::Display for Unmatched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_list(f, &self.patterns)?;
        write!(
            f,
            " matched none of {}",
            count(self.considered as u64, "file", "files")
        )?;
        if !self.examples.is_empty() {
            f.write_str(", which look like ")?;
            write_list(f, &self.examples)?;
        }
        Ok(())
    }
}

fn write_list(f: &mut fmt::Formatter<'_>, items: &[impl fmt::Display]) -> fmt::Result {
    for (i, item) in items.iter().enumerate() {
        let separator = match i {
            0 => "",
            _ if i + 1 == items.len() => " and ",
            _ => ", ",
        };
        write!(f, "{separator}`{item}`")?;
    }
    Ok(())
}

/// Why filters cannot be built.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FilterError {
    /// A pattern is not a valid glob.
    #[error("`{pattern}` is not a valid glob: {reason}")]
    Glob {
        /// The pattern as written.
        pattern: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The patterns could not be combined into one matcher.
    #[error("the patterns cannot be combined: {0}")]
    Set(String),
    /// A sample with both `n` and `fraction`.
    #[error("a sample takes either `n` or `fraction`, not both")]
    SampleBoth,
    /// A sample with neither `n` nor `fraction`.
    #[error("a sample needs `n` or `fraction`")]
    SampleNeither,
    /// A sample of zero files.
    #[error("a sample needs at least one file")]
    EmptySample,
    /// A sample fraction outside (0, 1].
    #[error("a sample fraction must be above 0 and at most 1, got {0}")]
    Fraction(f64),
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn path(text: &str) -> DataPath {
        DataPath::new(text).unwrap()
    }

    fn patterns(texts: &[&str]) -> Vec<Pattern> {
        texts.iter().map(|text| text.parse().unwrap()).collect()
    }

    fn select(texts: &[&str]) -> Filters {
        Filters::new(patterns(texts), Vec::new(), None, None).unwrap()
    }

    fn kept(filters: &Filters, files: &[(DataPath, Option<u64>)]) -> Vec<usize> {
        filters
            .apply(files.iter().map(|(path, size)| (path, *size)))
            .kept()
            .to_vec()
    }

    const DIALECT: &[(&str, &str, bool)] = &[
        ("*.edf", "a.edf", true),
        ("*.edf", "sub-01/eeg/x.edf", false),
        ("*.edf", ".edf", true),
        ("*", "a", true),
        ("*", ".hidden", true),
        ("*", "a/b", false),
        ("?", "a", true),
        ("?", "ab", false),
        ("a?c", "a/c", false),
        ("**/*.edf", "a.edf", true),
        ("**/*.edf", "sub-01/eeg/x.edf", true),
        ("**/*.edf", ".hidden/a.edf", true),
        ("**/*.edf", "x/y/z.EDF", false),
        ("sub-*/eeg/*.edf", "sub-01/eeg/x.edf", true),
        ("sub-*/eeg/*.edf", "sub-01/x/eeg/x.edf", false),
        ("sub-0?/**", "sub-01/eeg/x.edf", true),
        ("sub-0?/**", "sub-01", false),
        ("**/derivatives/**", "derivatives/x", true),
        ("**/derivatives/**", "sub/derivatives/y/z", true),
        ("**/derivatives/**", "derivatives", false),
        ("a/**/b", "a/b", true),
        ("a/**/b", "a/x/b", true),
        ("a/**/b", "a/x/y/b", true),
        ("a/**/b", "a/x/y/c", false),
        ("a/**", "a/b", true),
        ("a/**", "a/c/d", true),
        ("a/**", "a", false),
        ("**", "a", true),
        ("**", "x/y/z.edf", true),
        ("**/x", "x", true),
        ("**/x", "derivatives/x", true),
        ("**/x", "ax", false),
        ("[abc].txt", "a.txt", true),
        ("[abc].txt", "d.txt", false),
        ("[!a].txt", "b.txt", true),
        ("[!a].txt", "a.txt", false),
        ("[a-c].txt", "b.txt", true),
        ("{a,b}/*.csv", "a/1.csv", true),
        ("{a,b}/*.csv", "b/2.csv", true),
        ("{a,b}/*.csv", "c/1.csv", false),
        ("*.{csv,tsv}", "x.tsv", true),
        ("{a,{b,c}}.txt", "c.txt", true),
        ("{a,{b,c}}.txt", "d.txt", false),
        (
            "*dna.primary_assembly.fa.gz",
            "homo.dna.primary_assembly.fa.gz",
            true,
        ),
        (
            "*dna.primary_assembly.fa.gz",
            "g/homo.dna.primary_assembly.fa.gz",
            false,
        ),
        ("data/**/*.nii.gz", "data/s1/t1.nii.gz", true),
        ("data/**/*.nii.gz", "data/t1.nii.gz", true),
        ("\\*.txt", "*.txt", true),
        ("\\*.txt", "a.txt", false),
        ("participants.tsv", "participants.tsv", true),
        ("participants.tsv", "sub/participants.tsv", false),
        ("A.txt", "a.txt", false),
    ];

    #[test]
    fn the_glob_dialect_is_pinned() {
        for &(pattern, file, expected) in DIALECT {
            let files = [(path(file), None)];
            let matched = !kept(&select(&[pattern]), &files).is_empty();
            assert_eq!(matched, expected, "`{pattern}` against `{file}`");
        }
    }

    #[test]
    fn refuses_invalid_globs_naming_them() {
        let cases = [
            ("a[", "unclosed character class; missing ']'"),
            (
                "{a",
                "unclosed alternate group; missing '}' (maybe escape '{' with '[{]'?)",
            ),
            (
                "a}",
                "unopened alternate group; missing '{' (maybe escape '}' with '[}]'?)",
            ),
            ("a\\", "dangling '\\'"),
            ("[z-a]", "invalid range; 'z' > 'a'"),
        ];
        for (text, reason) in cases {
            let err = text.parse::<Pattern>().unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("`{text}` is not a valid glob: {reason}"),
            );
        }
    }

    #[test]
    fn keeps_the_pattern_text() {
        let pattern: Pattern = "**/*.edf".parse().unwrap();
        assert_eq!(pattern.as_str(), "**/*.edf");
        assert_eq!(pattern.to_string(), "**/*.edf");
    }

    fn listing() -> Vec<(DataPath, Option<u64>)> {
        [
            ("participants.tsv", Some(10)),
            ("sub-01/eeg/a.edf", Some(100)),
            ("sub-02/eeg/b.edf", Some(5000)),
            ("derivatives/sub-01/c.edf", Some(100)),
            ("sub-03/eeg/d.edf", None),
        ]
        .into_iter()
        .map(|(text, size)| (path(text), size))
        .collect()
    }

    #[test]
    fn no_filters_keep_everything() {
        let filters = Filters::new(Vec::new(), Vec::new(), None, None).unwrap();
        assert_eq!(kept(&filters, &listing()), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn applies_select_exclude_and_size_in_order() {
        let filters = Filters::new(
            patterns(&["**/*.edf", "participants.tsv"]),
            patterns(&["derivatives/**"]),
            Some("1 KB".parse().unwrap()),
            None,
        )
        .unwrap();
        let selection = filters.apply(listing().iter().map(|(p, s)| (p, *s)));
        assert_eq!(selection.kept(), [0, 1, 4]);
        assert!(selection.unmatched().is_empty());
    }

    #[test]
    fn reports_patterns_that_matched_nothing_with_examples() {
        let filters = Filters::new(
            patterns(&["*.edf", "**/*.edf", "*.csv"]),
            patterns(&["**/tmp/**"]),
            None,
            None,
        )
        .unwrap();
        let files = listing();
        let selection = filters.apply(files.iter().map(|(p, s)| (p, *s)));
        let unmatched: Vec<&str> = selection.unmatched().iter().map(|p| p.as_str()).collect();
        assert_eq!(unmatched, ["*.edf", "*.csv", "**/tmp/**"]);
        let err = selection.refuse_unmatched().unwrap_err();
        assert_eq!(
            err.to_string(),
            "`*.edf`, `*.csv` and `**/tmp/**` matched none of 5 files, which look like `participants.tsv`, `sub-01/eeg/a.edf` and `sub-02/eeg/b.edf`"
        );
        assert_eq!(err.patterns, ["*.edf", "*.csv", "**/tmp/**"]);
        assert_eq!(err.considered, 5);
    }

    #[test]
    fn a_single_unmatched_pattern_reads_naturally() {
        let files = [(path("a.txt"), None)];
        let filters = select(&["*.edf"]);
        let err = filters
            .apply(files.iter().map(|(p, s)| (p, *s)))
            .refuse_unmatched()
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "`*.edf` matched none of 1 file, which look like `a.txt`"
        );
        let empty: [(DataPath, Option<u64>); 0] = [];
        let err = filters
            .apply(empty.iter().map(|(p, s)| (p, *s)))
            .refuse_unmatched()
            .unwrap_err();
        assert_eq!(err.to_string(), "`*.edf` matched none of 0 files");
    }

    #[test]
    fn refuse_unmatched_passes_the_kept_indices_through() {
        let files = listing();
        let kept = select(&["**/*.edf"])
            .apply(files.iter().map(|(p, s)| (p, *s)))
            .refuse_unmatched()
            .unwrap();
        assert_eq!(kept, [1, 2, 3, 4]);
    }

    fn numbered(n: usize) -> Vec<(DataPath, Option<u64>)> {
        (0..n)
            .map(|i| (path(&format!("f{i:03}")), Some(1)))
            .collect()
    }

    fn sampled(sample: Sample, files: &[(DataPath, Option<u64>)]) -> Vec<usize> {
        let filters = Filters::new(Vec::new(), Vec::new(), None, Some(sample)).unwrap();
        kept(&filters, files)
    }

    #[test]
    fn a_count_sample_is_fixed_by_its_seed() {
        let files = numbered(20);
        let picks = sampled(Sample::Count { n: 3, seed: 1 }, &files);
        assert_eq!(picks.len(), 3);
        assert_eq!(picks, sampled(Sample::Count { n: 3, seed: 1 }, &files));
        let mut values: Vec<(u64, usize)> = files
            .iter()
            .enumerate()
            .map(|(index, (path, _))| {
                let mut bytes = 1_u64.to_le_bytes().to_vec();
                bytes.extend_from_slice(&(path.as_str().len() as u64).to_le_bytes());
                bytes.extend_from_slice(path.as_str().as_bytes());
                let hash = blake3::Hasher::new_derive_key("fetchloom sample v1")
                    .update(&bytes)
                    .finalize();
                let first: [u8; 8] = hash.as_bytes()[..8].try_into().unwrap();
                (u64::from_le_bytes(first), index)
            })
            .collect();
        values.sort_unstable();
        let mut lowest: Vec<usize> = values[..3].iter().map(|&(_, index)| index).collect();
        lowest.sort_unstable();
        assert_eq!(picks, lowest);
        assert_eq!(picks, [7, 11, 19]);
        assert_ne!(picks, sampled(Sample::Count { n: 3, seed: 2 }, &files));
        assert_eq!(sampled(Sample::Count { n: 30, seed: 1 }, &files).len(), 20);
    }

    #[test]
    fn a_whole_fraction_keeps_even_the_highest_value() {
        assert!(u128::from(u64::MAX) < share_of_range(1.0));
    }

    #[test]
    fn a_fraction_sample_keeps_about_that_share() {
        let files = numbered(1000);
        let picks = sampled(
            Sample::Fraction {
                fraction: 0.1,
                seed: 7,
            },
            &files,
        );
        assert!((70..=130).contains(&picks.len()), "{}", picks.len());
        assert_eq!(
            picks,
            sampled(
                Sample::Fraction {
                    fraction: 0.1,
                    seed: 7
                },
                &files
            )
        );
        let all = sampled(
            Sample::Fraction {
                fraction: 1.0,
                seed: 7,
            },
            &files,
        );
        assert_eq!(all.len(), 1000);
    }

    #[test]
    fn a_sample_draws_only_from_what_survived_the_other_filters() {
        let files = listing();
        let filters = Filters::new(
            patterns(&["**/*.edf"]),
            Vec::new(),
            None,
            Some(Sample::Count { n: 10, seed: 3 }),
        )
        .unwrap();
        assert_eq!(kept(&filters, &files), [1, 2, 3, 4]);
    }

    #[test]
    fn refuses_empty_or_impossible_samples() {
        let make = |sample| Filters::new(Vec::new(), Vec::new(), None, Some(sample));
        assert_eq!(
            make(Sample::Count { n: 0, seed: 0 })
                .unwrap_err()
                .to_string(),
            "a sample needs at least one file"
        );
        for fraction in [0.0, -0.5, 1.5, f64::NAN] {
            assert_eq!(
                make(Sample::Fraction { fraction, seed: 0 })
                    .unwrap_err()
                    .to_string(),
                format!("a sample fraction must be above 0 and at most 1, got {fraction}")
            );
        }
    }

    fn names_and_a_shuffle() -> impl Strategy<Value = (Vec<String>, Vec<String>)> {
        prop::collection::btree_set("[a-z]{1,6}(/[a-z]{1,6}){0,2}", 0..30).prop_flat_map(|set| {
            let names: Vec<String> = set.into_iter().collect();
            (Just(names.clone()), Just(names).prop_shuffle())
        })
    }

    proptest! {
        #[test]
        fn sample_picks_do_not_depend_on_listing_order(
            (names, shuffled) in names_and_a_shuffle(),
            n in 1_u64..10,
            seed in any::<u64>(),
        ) {
            let picked = |order: &[String]| {
                let files: Vec<(DataPath, Option<u64>)> =
                    order.iter().map(|name| (path(name), None)).collect();
                let mut chosen: Vec<String> = sampled(Sample::Count { n, seed }, &files)
                    .into_iter()
                    .map(|i| order[i].clone())
                    .collect();
                chosen.sort();
                chosen
            };
            prop_assert_eq!(picked(&names), picked(&shuffled));
        }
    }
}
