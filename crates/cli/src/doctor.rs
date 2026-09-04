//! Environment checks that report what they found and change nothing.

use std::path::Path;
use std::sync::Arc;

use fetchloom_engine::credential::{Necessity, host_variable, token_variable};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use serde::Serialize;

use crate::config::Discovered;
use crate::policy::CredentialStore;
use crate::settings::Environment;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Unknown,
    Actionable,
}

impl Status {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unknown => "unknown",
            Self::Actionable => "actionable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub key: String,
    pub status: Status,
    pub finding: String,
}

impl Check {
    fn new(key: &str, status: Status, finding: impl Into<String>) -> Self {
        Self {
            key: key.to_owned(),
            status,
            finding: finding.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        if self
            .checks
            .iter()
            .any(|check| check.status == Status::Actionable)
        {
            ExitCode::Resource
        } else {
            ExitCode::Success
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.checks
            .iter()
            .map(|check| {
                format!(
                    "{:<20} {:<10} {}",
                    check.key,
                    check.status.label(),
                    check.finding
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[must_use]
pub fn run(
    discovered: &Discovered,
    cache_root: &Path,
    environment: &dyn Environment,
    store: &dyn CredentialStore,
) -> Report {
    let work = Arc::new(WorkCounter::new());
    let mut checks = vec![
        configuration_check(discovered),
        cache_check(cache_root),
        disk_check(cache_root, &NativePlatform::new(Arc::clone(&work))),
        permissions_check(cache_root, &work),
        certificates_check(),
    ];
    checks.extend(provider_checks(environment, store));
    Report { checks }
}

fn configuration_check(discovered: &Discovered) -> Check {
    let project = discovered
        .project
        .as_ref()
        .map(|loaded| loaded.path.display().to_string());
    let user = discovered
        .user
        .as_ref()
        .map(|loaded| loaded.path.display().to_string());
    let finding = match (&project, &user) {
        (None, None) => "no project or user configuration file was found".to_owned(),
        (Some(project), None) => {
            format!("project configuration at {project}; no user configuration was found")
        }
        (None, Some(user)) => {
            format!("user configuration at {user}; no project configuration was found")
        }
        (Some(project), Some(user)) => {
            format!("project configuration at {project}; user configuration at {user}")
        }
    };
    Check::new("configuration", Status::Ok, finding)
}

fn cache_check(cache_root: &Path) -> Check {
    if !cache_root.is_dir() {
        return Check::new(
            "cache",
            Status::Ok,
            format!("{} does not exist yet", cache_root.display()),
        );
    }
    if std::fs::read_dir(cache_root).is_err() {
        return Check::new(
            "cache",
            Status::Actionable,
            format!("{} cannot be read", cache_root.display()),
        );
    }
    let writable = std::fs::metadata(cache_root).is_ok_and(|found| !found.permissions().readonly());
    if !writable {
        return Check::new(
            "cache",
            Status::Actionable,
            format!(
                "{} is read-only; use a cache directory this process can write to",
                cache_root.display()
            ),
        );
    }
    let format_path = cache_root.join("format");
    let ours = fetchloom_cache::format::render(fetchloom_cache::format::fingerprint());
    match std::fs::read_to_string(&format_path) {
        Ok(found) if found == ours => Check::new(
            "cache",
            Status::Ok,
            format!("{} matches this build's format", cache_root.display()),
        ),
        Ok(_) => Check::new(
            "cache",
            Status::Actionable,
            format!(
                "{} was written in a format this build does not read; run cache clear",
                cache_root.display()
            ),
        ),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Check::new(
            "cache",
            Status::Ok,
            format!("{} has not been initialized yet", cache_root.display()),
        ),
        Err(reason) => Check::new(
            "cache",
            Status::Actionable,
            format!("{} could not be read: {reason}", format_path.display()),
        ),
    }
}

fn disk_check(cache_root: &Path, platform: &NativePlatform) -> Check {
    let measured = cache_root
        .ancestors()
        .find(|held| held.is_dir())
        .map(|held| platform.free_space(held));
    match measured {
        Some(Ok(free)) => Check::new(
            "disk",
            Status::Ok,
            format!(
                "{free} bytes free on the volume holding {}",
                cache_root.display()
            ),
        ),
        Some(Err(reason)) => Check::new(
            "disk",
            Status::Unknown,
            format!(
                "free space on the volume holding {} could not be read: {}",
                cache_root.display(),
                reason.next_action()
            ),
        ),
        None => Check::new(
            "disk",
            Status::Unknown,
            format!(
                "no directory above {} exists, so no volume was measured",
                cache_root.display()
            ),
        ),
    }
}

fn probe_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "fetchloom-doctor-probe-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn permissions_check(cache_root: &Path, work: &Arc<WorkCounter>) -> Check {
    if !cache_root.is_dir() {
        return Check::new(
            "permissions",
            Status::Unknown,
            format!(
                "{} does not exist yet, so nothing was probed",
                cache_root.display()
            ),
        );
    }
    let platform = NativePlatform::new(Arc::clone(work));
    let probe = cache_root.join(probe_name());
    match platform.create_file_exclusive(&probe) {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            Check::new(
                "permissions",
                Status::Ok,
                format!("{} can be written to", cache_root.display()),
            )
        }
        Err(reason) => Check::new(
            "permissions",
            Status::Actionable,
            format!(
                "{} cannot be written to: {}",
                cache_root.display(),
                reason.next_action()
            ),
        ),
    }
}

fn certificates_check() -> Check {
    let outcome = std::panic::catch_unwind(fetchloom_sources::trust_store_loads);
    match outcome {
        Ok(true) => Check::new(
            "certificates",
            Status::Ok,
            "the platform trust store loaded",
        ),
        Ok(false) | Err(_) => Check::new(
            "certificates",
            Status::Actionable,
            "the platform trust store could not be loaded",
        ),
    }
}

struct ProviderProbe {
    label: &'static str,
    host: &'static str,
}

const PROVIDER_HOSTS: [ProviderProbe; 3] = [
    ProviderProbe {
        label: "provider.google_cloud_storage",
        host: "storage.googleapis.com",
    },
    ProviderProbe {
        label: "provider.azure_blob_storage",
        host: "blob.core.windows.net",
    },
    ProviderProbe {
        label: "provider.amazon_s3",
        host: "s3.amazonaws.com",
    },
];

fn provider_checks(environment: &dyn Environment, store: &dyn CredentialStore) -> Vec<Check> {
    PROVIDER_HOSTS
        .iter()
        .map(|probe| provider_check(probe, environment, store))
        .collect()
}

fn provider_check(
    probe: &ProviderProbe,
    environment: &dyn Environment,
    store: &dyn CredentialStore,
) -> Check {
    let host = Host::new(probe.host);
    let help = fetchloom_sources::help_for(probe.host, Necessity::Optional);
    if environment.get(&token_variable(&host)).is_some() {
        return Check::new(
            probe.label,
            Status::Ok,
            format!(
                "{}: a credential is present, from the environment",
                help.provider
            ),
        );
    }
    if environment
        .get(&host_variable("FETCHLOOM_ACCESS_KEY_", &host))
        .is_some()
    {
        return Check::new(
            probe.label,
            Status::Ok,
            format!(
                "{}: a signing credential is present, from the environment",
                help.provider
            ),
        );
    }
    match store.token(&host) {
        Ok(Some(_)) => Check::new(
            probe.label,
            Status::Ok,
            format!(
                "{}: a credential is present, from {}",
                help.provider,
                store.describe()
            ),
        ),
        Ok(None) => {
            let helper_present = fetchloom_sources::signs_requests(probe.host)
                && environment.get("AWS_ACCESS_KEY_ID").is_some();
            if helper_present {
                Check::new(
                    probe.label,
                    Status::Ok,
                    format!(
                        "{}: a signing credential is present, from the AWS environment variables",
                        help.provider
                    ),
                )
            } else {
                Check::new(
                    probe.label,
                    Status::Ok,
                    format!("{}: no credential is present", help.provider),
                )
            }
        }
        Err(reason) => Check::new(
            probe.label,
            Status::Actionable,
            format!(
                "{}: the credential store could not be read: {}",
                help.provider,
                reason.next_action()
            ),
        ),
    }
}
