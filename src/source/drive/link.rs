//! an open link does not say whether its id names a file or a folder; drive's redirect does.

use url::Url;

use super::{Item, ItemKind, page};
use crate::error::{Error, Kind};
use crate::net::Api;

impl Item {
    pub(super) async fn opened(&self, api: &impl Api) -> Result<Item, Error> {
        let mut url = Url::parse("https://drive.google.com/open").map_err(|_| self.unknown())?;
        url.query_pairs_mut().append_pair("id", &self.id);
        if let Some(resource) = &self.resource {
            url.query_pairs_mut().append_pair("resourcekey", resource);
        }
        let response = api.get(&url).await?;
        if response.status != 200 || response.url.host_str() == Some("accounts.google.com") {
            return Err(
                page::divert(&response.url, response.status, &response.text())
                    .err()
                    .unwrap_or_else(|| self.unknown()),
            );
        }
        let mut target = Item::from_link(&response.url)
            .filter(|item| item.kind != ItemKind::Open)
            .ok_or_else(|| self.unknown())?;
        target.resource = target.resource.or_else(|| self.resource.clone());
        Ok(target)
    }

    fn unknown(&self) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("drive did not identify {self} as a file or folder"),
        )
        .help("open the link in a browser and copy the file or folder's sharing link")
    }
}
