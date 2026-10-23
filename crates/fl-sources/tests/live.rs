//! Live smoke tests against the real providers, ignored unless asked for: `just live` runs them
//! with `FETCHLOOM_LIVE=1`. Each resolves one small public record and fetches its first file.

use std::sync::Arc;

use fl_sources::{Listing, Registry};
use fl_transfer::client::{Client, Config};
use fl_transfer::credentials::Chain;
use fl_transfer::download::Download;

use crate::support::{self, Failure, Memory};

/// Resolves `reference` against the real provider.
async fn resolve(reference: &str) -> Result<(Listing, Arc<Client>), Failure> {
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
    Ok((listing, client))
}

/// Resolves `reference` against the real provider, fetches its first file and checks it
/// against what the provider states, and returns how many files it lists.
async fn smoke(reference: &str) -> Result<usize, Failure> {
    let (listing, client) = resolve(reference).await?;
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

#[tokio::test]
#[ignore = "reaches www.openml.org"]
async fn openml() -> Result<(), Failure> {
    assert_eq!(smoke("openml:61").await?, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches api.figshare.com and ndownloader.figshare.com"]
async fn figshare() -> Result<(), Failure> {
    assert_eq!(smoke("figshare:34433697").await?, 18);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches dataverse.harvard.edu"]
async fn dataverse() -> Result<(), Failure> {
    assert_eq!(
        smoke("dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP").await?,
        3
    );
    Ok(())
}

#[tokio::test]
#[ignore = "reaches data.humdata.org"]
async fn ckan() -> Result<(), Failure> {
    assert_eq!(
        smoke("ckan:data.humdata.org/cerf-allocations-cub").await?,
        2
    );
    Ok(())
}

#[tokio::test]
#[ignore = "reaches data.caltech.edu"]
async fn inveniordm() -> Result<(), Failure> {
    assert_eq!(smoke("inveniordm:data.caltech.edu/mhmjd-4pr39").await?, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches api.github.com and github.com"]
async fn github() -> Result<(), Failure> {
    assert_eq!(smoke("github:dbrgn/tealdeer@v1.7.3").await?, 5);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches api.github.com and github.com"]
async fn github_latest() -> Result<(), Failure> {
    assert!(smoke("github:dbrgn/tealdeer").await? > 0);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches www.kaggle.com"]
async fn kaggle() -> Result<(), Failure> {
    assert_eq!(smoke("kaggle:uciml/iris@2").await?, 2);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches www.kaggle.com"]
async fn kaggle_pages() -> Result<(), Failure> {
    let (listing, _) = resolve("kaggle:justinianus/icpc-world-finals-ranking-since-1999@9").await?;
    assert_eq!(listing.entries.len(), 27);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches api.osf.io and osf.io"]
async fn osf() -> Result<(), Failure> {
    assert_eq!(smoke("osf:w867p").await?, 23);
    Ok(())
}

#[tokio::test]
#[ignore = "reaches datadryad.org"]
async fn dryad() -> Result<(), Failure> {
    let (listing, _) = resolve("dryad:doi:10.5061/dryad.2jm63xt6m").await?;
    assert_eq!(listing.entries.len(), 3);
    Ok(())
}
