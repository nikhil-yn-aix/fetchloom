//! Explaining a decision a run already made, without making a new one.

use std::path::PathBuf;

use fetchloom_cache::Cache;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::error::Error;
use fetchloom_engine::trust::{ArtifactKey, TrustClass};
use fetchloom_platform::NativePlatform;
use serde::Serialize;

use crate::run;

const REQUIRED_FOR_CORROBORATED: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Resolution {
    pub form: String,
    resolved_to: String,
    pub destination: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SourceChoice {
    pub(crate) recorded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct TrustReasoning {
    pub(crate) recorded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) class: Option<String>,
    pub(crate) reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    witnesses_found: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    witnesses_required: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Explanation {
    pub(crate) resolution: Resolution,
    pub(crate) source: SourceChoice,
    pub(crate) trust: TrustReasoning,
}

impl Explanation {
    #[must_use]
    pub(crate) fn render(&self) -> String {
        let mut lines = vec![
            format!(
                "resolution   {}: {}",
                self.resolution.form, self.resolution.resolved_to
            ),
            format!("destination  {}", self.resolution.destination),
        ];
        if self.source.recorded {
            lines.push(format!(
                "source       {}",
                self.source.source.as_deref().unwrap_or("?")
            ));
            if let Some(reason) = &self.source.reason {
                lines.push(format!("source reason {reason}"));
            }
        } else {
            lines.push("source       no run has been recorded for this destination".to_owned());
        }
        if self.trust.recorded {
            lines.push(format!(
                "trust        {}",
                self.trust.class.as_deref().unwrap_or("?")
            ));
            lines.push(format!("trust reason {}", self.trust.reason));
            if let (Some(found), Some(required)) =
                (self.trust.witnesses_found, self.trust.witnesses_required)
            {
                lines.push(format!(
                    "witnesses    {found} found; corroborated needs {required} independent, \\
                     which takes a second machine and no run in this build records one"
                ));
            }
        } else {
            lines.push(format!("trust        {}", self.trust.reason));
        }
        lines.join("\n")
    }
}

fn not_recorded(resolution: Resolution, reason: &str) -> Explanation {
    Explanation {
        resolution,
        source: SourceChoice {
            recorded: false,
            source: None,
            reason: None,
        },
        trust: TrustReasoning {
            recorded: false,
            class: None,
            reason: reason.to_owned(),
            witnesses_found: None,
            witnesses_required: None,
        },
    }
}

fn trust_reason(class: TrustClass) -> String {
    match class {
        TrustClass::Verified => {
            "the content digest matched a digest supplied before this run".to_owned()
        }
        TrustClass::Corroborated => {
            "no prior digest was supplied, and the observed digest matched at least two \
                independent recorded witnesses"
                .to_owned()
        }
        TrustClass::Tofu => {
            "no prior digest was supplied and no witnesses corroborated it, so the observed \
                digest was recorded as first use"
                .to_owned()
        }
        TrustClass::Unverified => {
            "the content could not be digested, or verification was disabled".to_owned()
        }
    }
}

pub(crate) fn explain(
    reference: &str,
    adapters: &Adapters,
    cache: Option<&Cache<NativePlatform>>,
) -> Result<Explanation, Error> {
    let served = run::is_served(adapters, reference);
    let source_path = if served {
        PathBuf::from(run::remote_name(reference))
    } else {
        run::local_path(reference)?
    };
    let destination = run::resolve_path(&run::default_destination(&source_path))?;
    let resolution = Resolution {
        form: if served {
            "a location an adapter of this build serves".to_owned()
        } else {
            "a local path".to_owned()
        },
        resolved_to: if served {
            reference.to_owned()
        } else {
            source_path.display().to_string()
        },
        destination: destination.display().to_string(),
    };

    let Some(receipt) = cache.and_then(|held| held.read_receipt(&destination).ok().flatten())
    else {
        return Ok(not_recorded(
            resolution,
            "no run has been recorded for this destination",
        ));
    };

    let dataset = run::dataset_name(adapters, reference, &source_path);
    let Some(artifact) = receipt.artifacts.get(&dataset) else {
        return Ok(not_recorded(
            resolution,
            &format!(
                "a run materialized the tree {} here and recorded no object under {dataset}, and a source is chosen for an object",
                receipt
                    .tree
                    .map_or_else(|| "it recorded".to_owned(), |tree| tree.to_string())
            ),
        ));
    };

    let source = SourceChoice {
        recorded: true,
        source: Some(artifact.source_used.to_string()),
        reason: artifact.source_reason.clone(),
    };

    let (witnesses_found, witnesses_required) = if artifact.trust == TrustClass::Tofu {
        let key = ArtifactKey::of(receipt.manifest, &dataset);
        let count = cache
            .and_then(|held| held.witnesses(&key).ok())
            .map(|found| {
                found
                    .iter()
                    .filter(|witness| witness.digest == artifact.digest)
                    .count()
            });
        (count, Some(REQUIRED_FOR_CORROBORATED))
    } else {
        (None, None)
    };

    let trust = TrustReasoning {
        recorded: true,
        class: Some(artifact.trust.label().to_owned()),
        reason: trust_reason(artifact.trust),
        witnesses_found,
        witnesses_required,
    };

    Ok(Explanation {
        resolution,
        source,
        trust,
    })
}
