//! ENA, the european nucleotide archive: the read files of runs, by the accession of a run, an
//! experiment, a sample or a study, as ENA's portal API reports them, with the MD5 and size it
//! states for each file. ENA mirrors the INSDC archives, so a run from NCBI's SRA or DDBJ's DRA
//! is listed too. a run accession names reads, not bytes: ENA's FASTQ, the files the submitter
//! sent and the SRA object are different files, so the reference names which it means, FASTQ
//! unless it says otherwise. runs under controlled access are left out, never downloaded.

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use url::Url;

use super::{Access, Listing, Reference, Remote, malformed};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};
use crate::units::grouped;

const PORTAL: &str = "https://www.ebi.ac.uk/ena/portal/api";

/// every field one report asks for: each kind's files, so a run without the kind asked for can
/// say which it has, and what tells a run under controlled access.
const FIELDS: &str = "run_accession,fastq_ftp,fastq_md5,fastq_bytes,submitted_ftp,submitted_md5,submitted_bytes,submitted_format,sra_ftp,sra_md5,sra_bytes,study_title";

/// which of a run's files a reference means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Files {
    /// the FASTQ files ENA makes from a run's reads.
    Fastq,
    /// the files as their submitter sent them, such as FASTQ, BAM or CRAM.
    Submitted,
    /// the run as one SRA object, which ENA keeps for few runs.
    Sra,
}

impl Files {
    /// the word a reference writes before the accession, as `ena:submitted/ERR1160846`.
    fn word(self) -> &'static str {
        match self {
            Files::Fastq => "fastq",
            Files::Submitted => "submitted",
            Files::Sra => "sra",
        }
    }

    fn named(word: &str) -> Option<Files> {
        [Files::Fastq, Files::Submitted, Files::Sra]
            .into_iter()
            .find(|files| files.word().eq_ignore_ascii_case(word))
    }

    /// the files as a sentence names them.
    fn described(self) -> &'static str {
        match self {
            Files::Fastq => "FASTQ files",
            Files::Submitted => "submitted files",
            Files::Sra => "SRA files",
        }
    }
}

/// the runs of an ENA accession, and which of their files it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Accession {
    files: Files,
    /// the accession, uppercase, as `SRR390728` or `PRJNA172563`.
    id: String,
}

impl Accession {
    /// reads the part after `ena:`: an accession, after `submitted/` or `sra/` for those files.
    pub(super) fn parse(text: &str) -> Result<Accession, Error> {
        let invalid = || {
            malformed(
                &format!("ena:{text}"),
                "ena:SRR390728, or ena:submitted/SRR390728 for the files the submitter sent",
            )
        };
        let text = text.trim();
        let (files, id) = match text.split_once('/') {
            Some((word, id)) => (Files::named(word).ok_or_else(invalid)?, id),
            None => (Files::Fastq, text),
        };
        let id = id.to_ascii_uppercase();
        level(&id).ok_or_else(invalid)?;
        Ok(Accession { files, id })
    }

    /// a pasted link to an accession's page in ENA's browser.
    pub(super) fn from_link(url: &Url) -> Option<Accession> {
        if !matches!(url.host_str()?, "www.ebi.ac.uk" | "ebi.ac.uk") {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let id = match parts.as_slice() {
            ["ena", "browser" | "data", "view", id] => id.to_ascii_uppercase(),
            _ => return None,
        };
        level(&id)?;
        Some(Accession {
            files: Files::Fastq,
            id,
        })
    }

    /// lists the files of every run the accession names, a run's own at the top of the dataset
    /// and those of several runs each in a folder named by its run.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let count = self.count(api).await?;
        if count == 0 {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("ENA has no public runs for {}", self.id),
            )
            .help(format!("check the accession at {}", self.page())));
        }
        if count > super::most_files() {
            return Err(self.too_many());
        }
        let mut rows = self.report(api, count).await?;
        if rows.len() < count {
            return Err(self.invalid(&format!(
                "the report ends after {} of the {} runs ENA counts",
                grouped(rows.len() as u64),
                grouped(count as u64)
            )));
        }
        rows.sort_by(|a, b| a.run_accession.cmp(&b.run_accession));
        let mut found = Found::default();
        for row in &rows {
            found.add(self, row)?;
            if found.files.len() > super::most_files() {
                return Err(self.too_many());
            }
        }
        if found.files.is_empty() {
            return Err(found.nothing(self, rows.len()));
        }
        let warnings = found.left_out(self, rows.len());
        Ok(Listing {
            within: Vec::new(),
            access: Access::Public,
            unavailable: Vec::new(),
            warnings,
            resolved: Reference::Ena(self.clone()),
            title: rows
                .iter()
                .map(|row| row.study_title.trim())
                .find(|title| !title.is_empty())
                .map(str::to_owned),
            license: None,
            doi: None,
            files: found.files,
        })
    }

    /// how many runs ENA's report holds for the accession.
    async fn count(&self, api: &impl Api) -> Result<usize, Error> {
        let mut url = self.url("filereportcount")?;
        url.set_query(Some(&format!(
            "accession={}&result=read_run&format=json",
            self.id
        )));
        let counted: Count = self.answer(&api.get(&url).await?)?;
        counted
            .count
            .trim()
            .parse()
            .map_err(|_| self.invalid("the count of runs is no number"))
    }

    /// ENA's report of the runs it counted, which it sends whole, since it has no pages: no
    /// more than `count` of them, so a report cannot grow past what the count let through.
    async fn report(&self, api: &impl Api, count: usize) -> Result<Vec<Row>, Error> {
        let mut url = self.url("filereport")?;
        url.set_query(Some(&format!(
            "accession={}&result=read_run&fields={FIELDS}&format=json&limit={count}",
            self.id
        )));
        let rows: Vec<Row> = self.answer(&api.get(&url).await?)?;
        if rows.len() > count {
            return Err(self.invalid(&format!(
                "the report holds more than the {} it was asked for",
                crate::units::count(count as u64, "run", "runs")
            )));
        }
        Ok(rows)
    }

    /// reads an answer of ENA's portal API, which refuses an accession it does not take with a
    /// 400.
    fn answer<T: serde::de::DeserializeOwned>(&self, response: &Response) -> Result<T, Error> {
        if response.status == 400 {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!("ENA does not take {} as the accession of runs", self.id),
            )
            .help("use the accession of a run (SRR, ERR, DRR), an experiment (SRX, ERX, DRX), a sample (SAMN, SAMEA, SAMD, SRS, ERS, DRS) or a study (PRJNA, PRJEB, PRJDB, SRP, ERP, DRP)"));
        }
        super::json(self, response)
    }

    fn url(&self, report: &str) -> Result<Url, Error> {
        Url::parse(&format!("{PORTAL}/{report}"))
            .map_err(|_| self.invalid("the API URL is invalid"))
    }

    fn page(&self) -> String {
        format!("https://www.ebi.ac.uk/ena/browser/view/{}", self.id)
    }

    /// the reference to the same runs' files of another kind.
    fn with(&self, files: Files) -> Accession {
        Accession {
            files,
            id: self.id.clone(),
        }
    }

    fn too_many(&self) -> Error {
        super::too_many(self, "add the runs you need by their own run accessions")
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("check the accession in ENA's browser and report the invalid listing at https://github.com/nikhil-yn-aix/fetchloom/issues")
    }
}

/// what an accession names, by its form as ENA's portal API takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Run,
    Experiment,
    Sample,
    Study,
}

fn level(id: &str) -> Option<Level> {
    if insdc(id, b'R') {
        Some(Level::Run)
    } else if insdc(id, b'X') {
        Some(Level::Experiment)
    } else if insdc(id, b'S') || biosample(id) {
        Some(Level::Sample)
    } else if insdc(id, b'P') || bioproject(id) {
        Some(Level::Study)
    } else {
        None
    }
}

/// an INSDC accession of the archive's own: E, S, D or R, then R, then `kind`, then six digits
/// or more, as `SRR390728`.
fn insdc(id: &str, kind: u8) -> bool {
    let bytes = id.as_bytes();
    bytes.len() >= 9
        && matches!(bytes[0], b'E' | b'S' | b'D' | b'R')
        && bytes[1] == b'R'
        && bytes[2] == kind
        && digits(&id[3..], 6)
}

/// a sample in the biosample databases, as `SAMN00630374` or `SAMEA3484362`.
fn biosample(id: &str) -> bool {
    if let Some(rest) = id.strip_prefix("SAMEA").or_else(|| id.strip_prefix("SAME")) {
        return digits(rest, 6);
    }
    id.strip_prefix("SAMN")
        .or_else(|| id.strip_prefix("SAMD"))
        .is_some_and(|rest| rest.len() == 8 && digits(rest, 8))
}

/// a study in the bioproject databases, as `PRJNA172563`.
fn bioproject(id: &str) -> bool {
    id.strip_prefix("PRJ").is_some_and(|rest| {
        rest.len() > 2
            && rest.as_bytes()[..2].iter().all(u8::is_ascii_uppercase)
            && digits(&rest[2..], 1)
    })
}

fn digits(text: &str, least: usize) -> bool {
    text.len() >= least && text.bytes().all(|b| b.is_ascii_digit())
}

/// a run's address in a report, `ftp.sra.ebi.ac.uk/vol1/…`, as the https link ENA serves it
/// at: on one of its own hosts, each part of the path encoded, since a file's name may hold `#`.
fn address(entry: &str) -> Option<Url> {
    let (host, path) = entry.trim().split_once('/')?;
    let own = host == "ebi.ac.uk" || host.ends_with(".ebi.ac.uk");
    let named = host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    let parts: Vec<&str> = path.split('/').collect();
    if !own || !named || parts.iter().any(|part| matches!(*part, "" | "." | "..")) {
        return None;
    }
    let mut url = Url::parse(&format!("https://{host}/")).ok()?;
    url.path_segments_mut().ok()?.clear().extend(parts);
    Some(url)
}

/// the files listed so far, and the runs left out and why.
#[derive(Default)]
struct Found {
    files: Vec<Remote>,
    /// runs under controlled access, and where ENA says to ask for them.
    protected: usize,
    controlled: BTreeSet<String>,
    /// runs without files of the kind asked for, and the kinds they have instead.
    without: usize,
    others: BTreeSet<Files>,
}

impl Found {
    fn add(&mut self, accession: &Accession, row: &Row) -> Result<(), Error> {
        let (entries, md5s, sizes) = row.of(accession.files);
        if row.submitted_format.contains("PROTECTED") || entries.starts_with("Protected") {
            self.protected += 1;
            if let Some(archive) = row.submitted_ftp.split("Go to ").nth(1) {
                let archive = archive.trim().trim_end_matches('.');
                if !archive.is_empty() && archive.len() <= 40 {
                    self.controlled.insert(archive.escape_debug().to_string());
                }
            }
            return Ok(());
        }
        if entries.trim().is_empty() {
            self.without += 1;
            self.others.extend(
                [Files::Fastq, Files::Submitted, Files::Sra]
                    .into_iter()
                    .filter(|files| *files != accession.files)
                    .filter(|files| !row.of(*files).0.trim().is_empty()),
            );
            return Ok(());
        }
        let run = &row.run_accession;
        if level(run) != Some(Level::Run) {
            return Err(accession.invalid("a run has no run accession"));
        }
        let entries: Vec<&str> = entries.split(';').collect();
        let listed = |field: &str| -> Result<Vec<String>, Error> {
            let values: Vec<String> = field.split(';').map(str::to_owned).collect();
            match values.as_slice() {
                [only] if only.is_empty() => Ok(vec![String::new(); entries.len()]),
                _ if values.len() == entries.len() => Ok(values),
                _ => Err(accession.invalid(&format!(
                    "run {run} gives {} but not as many sizes and hashes",
                    crate::units::count(entries.len() as u64, "file", "files")
                ))),
            }
        };
        let (md5s, sizes) = (listed(md5s)?, listed(sizes)?);
        for ((entry, md5), size) in entries.iter().zip(md5s).zip(sizes) {
            let url = address(entry).ok_or_else(|| {
                accession.invalid(&format!(
                    "run {run} gives a file away from ENA: {}",
                    entry.escape_debug()
                ))
            })?;
            let name = entry.rsplit('/').next().unwrap_or_default().to_owned();
            let md5 = match md5.trim() {
                "" => None,
                hex => Some(Md5::from_hex(hex).ok_or_else(|| {
                    accession.invalid(&format!("the MD5 of {} is not hex", name.escape_debug()))
                })?),
            };
            let size = match size.trim() {
                "" => None,
                number => Some(number.parse::<u64>().map_err(|_| {
                    accession.invalid(&format!("the size of {} is no number", name.escape_debug()))
                })?),
            };
            let path = if level(&accession.id) == Some(Level::Run) {
                name
            } else {
                format!("{run}/{name}")
            };
            self.files.push(Remote {
                path,
                size,
                sha256: None,
                md5,
                url,
                named_by_server: false,
                etag: None,
            });
        }
        Ok(())
    }

    /// the error for runs none of which has a file to list.
    fn nothing(&self, accession: &Accession, runs: usize) -> Error {
        if self.protected == runs {
            let ask = match self.controlled.iter().next() {
                Some(archive) => format!("ENA sends people to {archive} for them"),
                None => "the archive that holds them decides who may".to_owned(),
            };
            return Error::new(
                Kind::AuthRequired,
                format!("the runs of {} are under controlled access", accession.id),
            )
            .help(format!(
                "{ask}; fetchloom downloads public files only. check {}",
                accession.page()
            ));
        }
        let error = Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "ENA lists no {} for {accession}",
                accession.files.described()
            ),
        );
        match self.others.iter().next() {
            Some(other) => error.help(format!(
                "its runs have {}: use {}",
                other.described(),
                accession.with(*other)
            )),
            None => error.help(format!(
                "ENA has no files for its runs yet; check {}",
                accession.page()
            )),
        }
    }

    /// what a person should know about the runs left out of a listing of `runs`.
    fn left_out(&self, accession: &Accession, runs: usize) -> Vec<String> {
        let verb = |count: usize, one: &'static str, many: &'static str| {
            if count == 1 { one } else { many }
        };
        let mut warnings = Vec::new();
        if self.protected > 0 {
            warnings.push(format!(
                "{} of the {} runs {} under controlled access, so {} files are left out",
                grouped(self.protected as u64),
                grouped(runs as u64),
                verb(self.protected, "is", "are"),
                verb(self.protected, "its", "their")
            ));
        }
        if self.without > 0 {
            let instead = self.others.iter().next().map_or_else(String::new, |other| {
                format!(
                    "; {} lists {} {}",
                    accession.with(*other),
                    verb(self.without, "its", "their"),
                    other.described()
                )
            });
            warnings.push(format!(
                "{} of the {} runs {} no {}, so {} left out{instead}",
                grouped(self.without as u64),
                grouped(runs as u64),
                verb(self.without, "has", "have"),
                accession.files.described(),
                verb(self.without, "it is", "they are")
            ));
        }
        warnings
    }
}

#[derive(Deserialize)]
struct Count {
    count: String,
}

/// one run in ENA's report: every kind's files, each a `;` list of addresses, MD5s and sizes.
#[derive(Deserialize)]
struct Row {
    run_accession: String,
    #[serde(default)]
    fastq_ftp: String,
    #[serde(default)]
    fastq_md5: String,
    #[serde(default)]
    fastq_bytes: String,
    #[serde(default)]
    submitted_ftp: String,
    #[serde(default)]
    submitted_md5: String,
    #[serde(default)]
    submitted_bytes: String,
    #[serde(default)]
    submitted_format: String,
    #[serde(default)]
    sra_ftp: String,
    #[serde(default)]
    sra_md5: String,
    #[serde(default)]
    sra_bytes: String,
    #[serde(default)]
    study_title: String,
}

impl Row {
    /// the addresses, MD5s and sizes of the run's files of one kind.
    fn of(&self, files: Files) -> (&str, &str, &str) {
        match files {
            Files::Fastq => (&self.fastq_ftp, &self.fastq_md5, &self.fastq_bytes),
            Files::Submitted => (
                &self.submitted_ftp,
                &self.submitted_md5,
                &self.submitted_bytes,
            ),
            Files::Sra => (&self.sra_ftp, &self.sra_md5, &self.sra_bytes),
        }
    }
}

impl fmt::Display for Accession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ena:")?;
        if self.files != Files::Fastq {
            write!(f, "{}/", self.files.word())?;
        }
        f.write_str(&self.id)
    }
}

#[cfg(test)]
mod tests;
