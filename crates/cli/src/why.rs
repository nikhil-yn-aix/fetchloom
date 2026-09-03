//! Explaining a decision a run already made, without making a new one.

use std::path::PathBuf;

use fetchloom_cache::Cache;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::error::Error;
use fetchloom_engine::trust::{ArtifactKey, TrustClass};
use fetchloom_platform::NativePlatform;
use serde::Serialize;

use crate::run;

/// How many independent witnesses `corroborated` requires.
const REQUIRED_FOR_CORROBORATED: usize = 2;

/// How a reference was resolved, without reaching the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// Which reference form this is.
    pub form: String,
    /// What the reference resolved to.
    pub resolved_to: String,
    /// Where a run would materialize it by default.
    pub destination: String,
}

/// What a receipt says was chosen, when one exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceChoice {
    /// Whether a run has been recorded for this destination.
    pub recorded: bool,
    /// The source a run used, when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The reason recorded for choosing it, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What is known about the trust class recorded for a reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrustReasoning {
    /// Whether a trust class has been recorded.
    pub recorded: bool,
    /// The class recorded, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// The mechanical reason for that class.
    pub reason: String,
    /// The independent witnesses found for the recorded digest, when the
    /// class is `tofu`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witnesses_found: Option<usize>,
    /// The independent witnesses `corroborated` requires, when the class is
    /// `tofu`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witnesses_required: Option<usize>,
}

/// Everything `why` reports about one reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Explanation {
    /// How the reference was resolved.
    pub resolution: Resolution,
    /// What a receipt says was chosen.
    pub source: SourceChoice,
    /// What is known about the trust class recorded.
    pub trust: TrustReasoning,
}

impl Explanation {
    /// Renders this explanation as aligned text.
    #[must_use]
    pub fn render(&self) -> String {
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
                    "witnesses    {found} found, corroborated requires {required} independent"
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

/// Explains the resolution, source choice, and trust reasoning already
/// recorded for a reference.
///
/// # Errors
///
/// Fails with `reference.unresolved` when the reference cannot be resolved at
/// all.
pub fn explain(
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
                "a run materialized the tree {} here and resolved no object, because this reference names a directory and a directory states no bytes to choose a source for",
                receipt.tree.map_or_else(|| "it recorded".to_owned(), |tree| tree.to_string())
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
