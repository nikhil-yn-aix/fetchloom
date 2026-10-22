//! Live smoke tests against the real providers, ignored unless asked for: `just live` runs them
//! with `FETCHLOOM_LIVE=1`. Each resolves one small public record and fetches its first file.

use std::sync::Arc;

use fl_sources::Registry;
use fl_transfer::client::{Client, Config};
use fl_transfer::credentials::Chain;
use fl_transfer::download::Download;

use crate::support::{self, Failure, Memory};

/// Resolves `reference` against the real provider, fetches its first file and checks it
/// against what the provider states, and returns how many files it lists.
async fn smoke(reference: &str) -> Result<usize, Failure> {
    if std::env::var_os("FETCHLOOM_LIVE").is_none() {
        return Err("live tests run with FETCHLOOM_LIVE=1".into());
    }
    let chain = Arc::new(Chain::new(Vec::new()));
    let client = Arc::new(Client::new(Config::default(), chain.clone())?);
    let registry = Registry::builtin(&client)?;
    for credential in registry.credentials(|name| std::env::var(name).ok()) {
        chain.add(Box::new(credential));
    }
    let listing = registry
        .resolve(&reference.parse()?, &support::no_filters()?)
        .await?;
    let entry = listing.entries.first().ok_or("no files")?;
    let download = Download {
        url: entry.urls[0].clone(),
        length: entry.size,
        stable: !entry.stated.is_empty(),
        stated: entry.stated.clone(),
        typed: false,
    };
    let done = client.download(&download, Memory::default()).await?;
    assert!(done.bytes > 0, "{reference}: empty first file");
    Ok(listing.entries.len())
}

#[tokio::test]
#[ignore = "reaches zenodo.org"]
async fn zenodo() -> Result<(), Failure> {
    assert_eq!(smoke("zenodo:3608990").await?, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches doi.org and zenodo.org"]
async fn a_zenodo_doi() -> Result<(), Failure> {
    assert_eq!(smoke("doi:10.5281/zenodo.3608990").await?, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches huggingface.co"]
async fn hugging_face() -> Result<(), Failure> {
    assert_eq!(smoke("hf:datasets/rajpurkar/squad").await?, 4);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches dataverse.harvard.edu"]
async fn croissant() -> Result<(), Failure> {
    let reference = "croissant:https://dataverse.harvard.edu/api/datasets/export?exporter=croissant&persistentId=doi:10.7910/DVN/TJCLKP";
    assert_eq!(smoke(reference).await?, 3);
    Ok(())
}
