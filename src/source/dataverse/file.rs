//! one file of a dataset, as its page on the install links it. the files API describes the file
//! at a published version of its dataset, with that version's facts, in one request.

use serde::Deserialize;
use url::Url;

use super::version::{File, Version};
use super::{Dataset, Item, is_number};
use crate::error::Error;
use crate::net::Api;
use crate::source::Listing;

#[derive(Deserialize)]
struct Answer {
    status: String,
    data: Described,
}

/// a file at one version of its dataset.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Described {
    #[serde(flatten)]
    file: File,
    dataset_version: Version,
}

/// lists the file `id` names, by its database id or its persistent id, at the version `dataset`
/// asks for, or at the newest published one.
pub(super) async fn resolve(dataset: &Dataset, id: &str, api: &impl Api) -> Result<Listing, Error> {
    let response = api.get(&url(dataset, id)?).await?;
    let answer: Answer = crate::source::json(dataset, &response)?;
    if answer.status != "OK" {
        return Err(dataset.invalid("the API did not return an OK answer"));
    }
    let Described {
        file,
        dataset_version,
    } = answer.data;
    if !file.is(id) {
        return Err(dataset.invalid("the answer describes another file"));
    }
    let item = Item::File(file.id().to_string());
    dataset_version.pinned(dataset, item, vec![file])
}

/// `/api/files/<id>/versions/<version>`, asking for the dataset version's facts as well.
fn url(dataset: &Dataset, id: &str) -> Result<Url, Error> {
    let version = dataset.version.as_deref().unwrap_or(":latest-published");
    let named = if is_number(id) { id } else { ":persistentId" };
    let mut url = Url::parse(&format!("https://{}/api/files/", dataset.host))
        .map_err(|_| dataset.invalid("the file URL is invalid"))?;
    url.path_segments_mut()
        .map_err(|()| dataset.invalid("the file URL has no path"))?
        .pop_if_empty()
        .extend([named, "versions", version]);
    {
        let mut query = url.query_pairs_mut();
        if !is_number(id) {
            query.append_pair("persistentId", id);
        }
        query.append_pair("returnDatasetVersion", "true");
    }
    Ok(url)
}
