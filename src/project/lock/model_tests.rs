//! generated valid locks exercise both readers instead of mostly rejecting random text.

use super::derived::{Code, Input};
use super::*;
use crate::cache::Member;

struct Tape<'a> {
    bytes: &'a [u8],
    next: usize,
}

impl<'a> Tape<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, next: 0 }
    }
    fn byte(&mut self) -> u8 {
        let value = self.bytes.get(self.next).copied().unwrap_or(0);
        self.next += 1;
        value
    }
    fn number(&mut self) -> u64 {
        u64::from_le_bytes(std::array::from_fn(|_| self.byte()))
    }
    fn text(&mut self) -> String {
        const CHARS: &[char] = &[
            'a', 'Z', ' ', '"', '\\', '\n', '\r', '\t', '\0', '\u{7f}', '\u{85}', 'é', '\u{301}',
            'ẞ', 'ς', '名', '🚀',
        ];
        (0..self.byte() % 24)
            .map(|_| CHARS[usize::from(self.byte()) % CHARS.len()])
            .collect()
    }
}

fn inputs() -> Vec<Vec<u8>> {
    let mut cases = vec![vec![], vec![255; 128], vec![1; 128], vec![42; 256]];
    let mut random = fastrand::Rng::with_seed(0x10cc_5eed);
    for _ in 0..64 {
        let mut bytes = vec![0; 1024];
        random.fill(&mut bytes);
        cases.push(bytes);
    }
    cases
}

fn generated(bytes: &[u8]) -> Lock {
    let mut input = Tape::new(bytes);
    let mut datasets = Vec::new();
    for dataset_index in 0..=input.byte() % 3 {
        let mut files = Vec::new();
        for file_index in 0..input.byte() % 5 {
            let leaf = [
                "café.csv",
                "cafe\u{301}.csv",
                "quote\".csv",
                "名.csv",
                "x.csv",
            ][usize::from(input.byte()) % 5];
            let size = match input.byte() % 6 {
                0 => 0,
                1 => 1,
                2 => i64::MAX as u64,
                3 => u64::from(u32::MAX) + 1,
                _ => input.number() & i64::MAX as u64,
            };
            files.push(LockedFile {
                path: format!("{file_index}/{leaf}"),
                size,
                sha256: digest::sha256(input.text().as_bytes()),
                md5: (input.byte() & 1 != 0)
                    .then(|| Md5::from_hex("900150983cd24fb0d6963f7d28e17f72").unwrap()),
                url: format!("https://example.org/{}", input.text()),
                unpacks_to: (input.byte() & 1 != 0)
                    .then(|| digest::sha256(input.text().as_bytes())),
            });
        }
        let extract = input.byte() & 1 != 0;
        let access = [
            Access::Public,
            Access::Login,
            Access::Terms,
            Access::Unknown,
        ][usize::from(input.byte()) % 4];
        let mut optional = || (input.byte() & 1 != 0).then(|| input.text());
        let title = optional();
        let license = optional();
        let doi = optional();
        datasets.push(Locked {
            name: format!("set{dataset_index}"),
            reference: format!("https://example.org/{}", input.text()),
            spec: format!("sha256:{}", digest::sha256(bytes)),
            extract,
            resolved: input.text(),
            title,
            license,
            doi,
            access,
            timestamp: input.text(),
            files,
        });
    }
    let derived = (0..input.byte() % 3)
        .map(|index| recipe(&mut input, index))
        .collect();
    Lock { datasets, derived }
}

/// a recipe's result with names, arguments and variables of every awkward kind, which a program
/// could still be given, so the readers check their form rather than refusing them.
fn recipe(input: &mut Tape, index: u8) -> Derived {
    let given = |text: String| text.replace('\0', "");
    let from = (0..=input.byte() % 2)
        .map(|place| Input {
            name: format!("set{place}{}", input.text()),
            content: digest::sha256(input.text().as_bytes()),
        })
        .collect();
    let run = (0..=input.byte() % 3)
        .map(|place| given(format!("{place}{}", input.text())))
        .collect();
    let code = (0..input.byte() % 3)
        .map(|place| Code {
            path: format!(
                "code/{place}/{}.py",
                ["a", "名", "quote\"", "é"][usize::from(input.byte()) % 4]
            ),
            sha256: digest::sha256(input.text().as_bytes()),
            folder: input.byte() & 1 != 0,
        })
        .collect();
    let env = (0..input.byte() % 3)
        .map(|_| {
            let name = ["SEED", "two words", "é", "a-b_c"][usize::from(input.byte()) % 4];
            (name.to_owned(), given(input.text()))
        })
        .collect();
    let files: Vec<Member> = (0..input.byte() % 4)
        .map(|place| Member {
            path: format!(
                "out/{place}/{}",
                ["x.csv", "café.csv", "名.csv"][usize::from(input.byte()) % 3]
            ),
            size: input.number() & i64::MAX.unsigned_abs(),
            sha256: digest::sha256(input.text().as_bytes()),
        })
        .collect();
    Derived {
        name: format!("result{index}"),
        from,
        run,
        code,
        env,
        key: digest::sha256(input.text().as_bytes()),
        timestamp: input.text(),
        tree: crate::cache::tree_hash(&files),
        files,
    }
}

/// the most files a generated dataset lists in data.lock itself, so some keep a list beside it.
const INLINE: usize = 2;

/// writes the lists `lock` keeps beside data.lock into a project folder.
fn project(lock: &Lock) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
    for (part, text) in lock.lists_with(INLINE) {
        std::fs::write(root.path().join(part), lists::compress(&text).unwrap()).unwrap();
    }
    root
}

fn compare(lock: &Lock) {
    let root = project(lock);
    let read = |text: &str| Lock::read(text, root.path()).unwrap();
    let rendered = lock.render_with(INLINE);
    let fast = canonical::read(&rendered, false);
    assert_eq!(
        fast.as_ref(),
        Some(&parse_toml(&rendered).unwrap()),
        "{rendered}"
    );
    assert_eq!(&read(&rendered), lock, "{rendered}");
    assert_eq!(
        canonical::read(&rendered.replace('\n', "\r\n"), false),
        fast
    );
    assert_eq!(&read(&format!("\u{feff}{rendered}")), lock);
    assert_eq!(read(&rendered).render_with(INLINE), rendered);
    let mut shuffled = lock.clone();
    shuffled.datasets.reverse();
    for dataset in &mut shuffled.datasets {
        dataset.files.reverse();
    }
    shuffled.derived.reverse();
    for derived in &mut shuffled.derived {
        derived.code.reverse();
        derived.files.reverse();
    }
    assert_eq!(shuffled.render_with(INLINE), rendered);
}

/// the fast reader of canonical text and the toml reader read every generated lock alike,
/// whatever its line ends, a leading byte order mark or the order it was built in.
#[test]
fn every_generated_lock_reads_back_alike_through_both_readers() {
    compare(&Lock::default());
    for bytes in inputs() {
        compare(&generated(&bytes));
    }
}

#[test]
fn lock_reader_mutations_agree() {
    for bytes in inputs() {
        let lock = generated(&bytes);
        let root = project(&lock);
        let text = lock.render_with(INLINE);
        let variants = [
            text.replacen("version = 1", "version=1", 1),
            text.replacen("version = 1", "version = 0", 1),
            text.replacen("version = 1", "version = 2", 1),
            text.replacen("size = ", "size = +", 1),
            text.replacen("size = ", "size = 0", 1),
            text.replacen("sha256 = \"", "sha256 = \"x", 1),
            text.replacen("path = \"", "path = \"../", 1),
            text.replacen(" },\n", ", unknown = 1 },\n", 1),
            text.replacen("file_count = ", "file_count = 1", 1),
            text.replacen("files_in = [", "files = [", 1),
            text.replace('\n', "\r\n"),
            text.trim_end().to_owned(),
            format!("{text}# comment\n"),
        ];
        for changed in variants {
            let generic = parse_toml(&changed);
            let normal = parse(&changed);
            assert_eq!(normal.as_ref().ok(), generic.as_ref().ok(), "{changed}");
            for comments in [false, true] {
                if let Some(fast) = canonical::read(&changed, comments) {
                    assert_eq!(generic.as_ref().ok(), Some(&fast), "{changed}");
                }
            }
            if let Ok(lock) = normal.and_then(|parsed| resolve(parsed, Some(root.path()))) {
                compare(&lock);
            }
        }
    }
}
