//! one paper: its newest version or the one named, its title and license from arXiv, and its
//! pdf from the mirror, else from export.arxiv.org.

use super::{Paper, feed, mirrored};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Access, Listing, Reference};

impl Paper {
    /// lists the paper's pdf at the version named, or at the newest arXiv has.
    pub(in crate::source) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let asked = match self.version {
            Some(version) => format!("{}v{version}", self.id),
            None => self.id.to_string(),
        };
        let page = feed::ask(api, &[("id_list", asked.as_str())], self).await?;
        let entry = match page.entries.as_slice() {
            [] => return Err(self.missing()),
            [entry] => entry,
            more => {
                return Err(feed::unusable(
                    self,
                    &format!("it described {} papers", more.len()),
                ));
            }
        };
        if entry.id != self.id || self.version.is_some_and(|version| version != entry.version) {
            return Err(feed::unusable(
                self,
                &format!("it described arxiv:{}v{}", entry.id, entry.version),
            ));
        }
        let pinned = self.at(entry.version);
        let record = feed::record(api, &self.id).await?;
        if record.withdraws(entry.version) {
            return Err(self.withdrawn(entry.version));
        }
        let license = record.license;
        let key = format!("{}{}", self.id.folder(), self.id.mirrored(entry.version));
        let file = match mirrored(api, &key).await?.pop() {
            Some(mut held) => {
                held.path = self.id.file(entry.version);
                held
            }
            None => self.id.pdf(entry.version)?,
        };
        Ok(Listing {
            within: Vec::new(),
            access: if license.as_deref().is_some_and(is_open) {
                Access::Public
            } else {
                Access::Unknown
            },
            resolved: Reference::Arxiv(pinned),
            title: Some(entry.title.clone()),
            license,
            doi: Some(format!("10.48550/arXiv.{}", self.id)),
            files: vec![file],
            unavailable: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// the error for a paper whose version `version` is a withdrawal, which has no pdf.
    fn withdrawn(&self, version: u32) -> Error {
        let error = Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "arxiv:{} was withdrawn at v{version}, which has no pdf",
                self.id
            ),
        );
        match version.checked_sub(1).filter(|earlier| *earlier > 0) {
            Some(earlier) => error.help(format!(
                "its earlier versions stay on arXiv: add one by its version, as {}",
                self.at(earlier)
            )),
            None => error,
        }
    }

    /// the error for a paper, or a version of one, arXiv does not have.
    fn missing(&self) -> Error {
        let message = match self.version {
            Some(version) => format!("arXiv has no version v{version} of arxiv:{}", self.id),
            None => format!("arXiv has no paper arxiv:{}", self.id),
        };
        Error::new(Kind::ReferenceUnresolved, message).help(
            "check the id on the paper's page at arxiv.org; arXiv's API knows a paper from the day after arXiv announces it",
        )
    }
}

/// whether a license arXiv states lets anyone share copies: a creative commons license or the
/// public domain, unlike arXiv's own, which lets arXiv alone share them.
fn is_open(license: &str) -> bool {
    license.contains("creativecommons.org/")
}
