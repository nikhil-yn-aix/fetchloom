//! AWS keys where the AWS tools keep them: the environment, then the profile `AWS_PROFILE` names,
//! else `default`, in the shared credentials and config files. a profile that signs in to IAM
//! identity center or takes on a role gets its keys from the AWS CLI, and one with a
//! `credential_process` from that program, each run as [`program`] says. a profile is never
//! passed over for another identity, and keys reach an S3-compatible endpoint only where the
//! person's own AWS settings send them there.

mod program;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use self::program::Program;
use crate::auth::lease::Lease;
use crate::error::{Error, Kind};
use crate::net::sign::Keys;
use crate::source::Var;
use crate::stop::Stop;

/// keys found for one endpoint, and where.
pub(crate) struct Identity {
    pub(crate) source: Source,
    /// where the keys came from, as messages name it: `AWS_ACCESS_KEY_ID`, `profile lab in
    /// /home/ada/.aws/credentials`, or `the credential_process of profile lab`.
    pub(crate) origin: String,
    /// the region set beside the keys, by `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile.
    pub(crate) region: Option<String>,
}

/// where an identity's keys come from.
pub(crate) enum Source {
    /// keys written in the environment or a profile, which never expire.
    Keys(Arc<Keys>),
    /// a program that prints keys, run again shortly before they expire.
    Program {
        program: Program,
        lease: Box<Lease<Arc<Keys>>>,
    },
}

impl Identity {
    /// the keys that sign a request made at `now`: written keys as they are, a program's from
    /// its last run while they hold, else from a run that every request needing them waits for.
    ///
    /// # Errors
    ///
    /// what running the program found wrong, the same for every later request, or `interrupted`
    /// once `stop` asks.
    pub(crate) async fn keys(&self, now: SystemTime, stop: &Stop) -> Result<Arc<Keys>, Error> {
        match &self.source {
            Source::Keys(keys) => Ok(keys.clone()),
            Source::Program { program, lease } => {
                let run = || async {
                    let issued = program.run(now, stop).await?;
                    // keys that expired by `now` are refused before they come this far
                    let lifetime = issued
                        .expires
                        .map(|expires| expires.duration_since(now).unwrap_or_default());
                    Ok((Arc::new(issued.keys), lifetime))
                };
                lease.get(Instant::now, run).await
            }
        }
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // where the keys came from, never the keys or a program's arguments
        f.debug_struct("Identity")
            .field("origin", &self.origin)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl Source {
    fn program(program: Program) -> Source {
        Source::Program {
            program,
            lease: Box::default(),
        }
    }
}

/// the keys that sign for `endpoint`, or for AWS itself when it is none. the environment's keys,
/// else the chosen profile's, sign where the settings send S3 requests: AWS, unless
/// `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT_URL` or the profile names an endpoint. another endpoint
/// takes the keys of the one profile whose `endpoint_url` names it. none when no keys are there.
/// a program that gives a profile's keys is found here and run only once keys are needed.
///
/// # Errors
///
/// `usage` when `AWS_PROFILE` names a profile neither file has, an endpoint setting is no link,
/// or a `credential_process` cannot be read; `auth.required` for half a key, a program PATH does
/// not hold, settings that send these keys to another endpoint than the one asked for, or two
/// profiles naming it.
pub(crate) fn find(
    var: Var<'_>,
    endpoint: Option<&url::Origin>,
) -> Result<Option<Identity>, Error> {
    let files = Files::read(var);
    let chosen = set(var, "AWS_PROFILE");
    let profile = chosen.as_deref().unwrap_or("default");
    if chosen.is_some() && !files.has(profile) {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "AWS_PROFILE names the profile {profile}, which the AWS credentials and config files do not have"
            ),
        )
        .help("set AWS_PROFILE to a profile in ~/.aws/credentials or ~/.aws/config, or unset it to use the default one"));
    }
    let ignored = files.ignores_endpoints(var, profile);
    let sent = if ignored {
        None
    } else {
        files.sends_to(var, profile)?
    };
    if sent.as_ref().map(|(origin, _)| origin) == endpoint {
        let region = region(var).or_else(|| files.value(profile, "region"));
        return Ok(own(var, &files, profile)?.map(|(source, origin)| Identity {
            source,
            origin,
            region,
        }));
    }
    if let (None, Some((origin, named))) = (endpoint, &sent) {
        return Err(Error::new(
            Kind::AuthRequired,
            format!(
                "{named} sends these AWS keys to {}, so fetchloom does not send them to AWS",
                origin.ascii_serialization()
            ),
        )
        .help("use an AWS profile without an endpoint for buckets on AWS, or write s3:https://host/bucket/ for a bucket at that endpoint"));
    }
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let mut naming: Vec<&str> = files
        .profiles()
        .filter(|name| *name != profile)
        .filter(|name| {
            !ignored && files.endpoint_of(name).ok().flatten().as_ref() == Some(endpoint)
        })
        .collect();
    naming.sort_unstable();
    naming.dedup();
    match naming.as_slice() {
        [] => Ok(None),
        [name] => {
            let region = region(var).or_else(|| files.value(name, "region"));
            Ok(
                profile_keys(var, &files, name)?.map(|(source, origin)| Identity {
                    source,
                    origin,
                    region,
                }),
            )
        }
        several => Err(Error::new(
            Kind::AuthRequired,
            format!(
                "the AWS profiles {} each name {} as their endpoint",
                several.join(" and "),
                endpoint.ascii_serialization()
            ),
        )
        .help("set AWS_PROFILE to the one whose keys fetchloom should use there")),
    }
}

/// where the keys that sign S3 requests would come from, as `auth` lists them: found as [`find`]
/// finds them for the endpoint the settings send S3 requests to, none for AWS itself. a program
/// is named and never run, and written keys show only their id's last four characters.
pub(crate) struct Described {
    /// the endpoint the settings send S3 requests to, none for AWS itself.
    pub(crate) endpoint: Option<url::Origin>,
    /// where the keys come from, as [`Identity::origin`] words it.
    pub(crate) origin: String,
    /// the same, with a file below the home folder written `~/…`.
    pub(crate) shown: String,
    pub(crate) how: How,
}

/// how described keys come.
pub(crate) enum How {
    /// written in the environment or a profile, shown by their id's last four characters.
    Keys(String),
    /// from the profile's `credential_process`.
    Process,
    /// from the AWS CLI, which signs in to IAM identity center when `signs_in`, else takes on a
    /// role.
    Cli { signs_in: bool },
}

impl How {
    /// the kind `auth --json` names.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            How::Keys(_) => "keys",
            How::Process => "credential_process",
            How::Cli { signs_in: true } => "sso",
            How::Cli { signs_in: false } => "role",
        }
    }
}

/// the keys for the endpoint the settings send S3 requests to, described for `auth`, none when
/// no keys are there.
///
/// # Errors
///
/// what [`find`] finds wrong with the settings.
pub(crate) fn describe(var: Var<'_>) -> Result<Option<Described>, Error> {
    let files = Files::read(var);
    let chosen = set(var, "AWS_PROFILE");
    let profile = chosen.as_deref().unwrap_or("default");
    let endpoint = if files.ignores_endpoints(var, profile) {
        None
    } else {
        files.sends_to(var, profile)?.map(|(origin, _)| origin)
    };
    let Some(identity) = find(var, endpoint.as_ref())? else {
        return Ok(None);
    };
    let how = match &identity.source {
        Source::Keys(keys) => How::Keys(keys.shown()),
        Source::Program { program, .. } => match program.by {
            program::By::Process => How::Process,
            program::By::Cli { signs_in } => How::Cli { signs_in },
        },
    };
    // the file a profile's origin names whole reads below the home folder as `~/…`
    let shown = [&files.credentials_path, &files.config_path]
        .into_iter()
        .flatten()
        .find_map(|path| {
            identity
                .origin
                .strip_suffix(&path.display().to_string())
                .map(|head| format!("{head}{}", crate::units::path(path)))
        })
        .unwrap_or_else(|| identity.origin.clone());
    Ok(Some(Described {
        endpoint,
        origin: identity.origin,
        shown,
        how,
    }))
}

/// the environment's keys, else the profile's, and where they came from.
fn own(var: Var<'_>, files: &Files, profile: &str) -> Result<Option<(Source, String)>, Error> {
    let id = set(var, "AWS_ACCESS_KEY_ID");
    let secret = set(var, "AWS_SECRET_ACCESS_KEY");
    match (id, secret) {
        (Some(id), Some(secret)) => Ok(Some((
            Source::Keys(Arc::new(Keys {
                id,
                secret,
                token: set(var, "AWS_SESSION_TOKEN"),
            })),
            "AWS_ACCESS_KEY_ID".to_owned(),
        ))),
        (None, None) => profile_keys(var, files, profile),
        (Some(_), None) => Err(half_in_environment(
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
        )),
        (None, Some(_)) => Err(half_in_environment(
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
        )),
    }
}

fn half_in_environment(given: &str, missing: &str) -> Error {
    Error::new(
        Kind::AuthRequired,
        format!("{given} is set, but {missing} is not"),
    )
    .help(format!(
        "set {missing} too, or unset {given} to use a profile in ~/.aws/credentials"
    ))
}

/// where a profile's keys come from, in the order the AWS tools try them: the AWS CLI, for a
/// profile that takes on a role or signs in to IAM identity center, which fetchloom leaves to
/// it; keys in the credentials file; the profile's `credential_process`; keys in the config file.
fn profile_keys(
    var: Var<'_>,
    files: &Files,
    profile: &str,
) -> Result<Option<(Source, String)>, Error> {
    let signs_in = files.names(profile).any(|name| name.starts_with("sso_"));
    let takes_role = ["role_arn", "source_profile", "web_identity_token_file"]
        .iter()
        .any(|key| files.value(profile, key).is_some());
    if signs_in || takes_role {
        let program = Program::cli(profile, signs_in, var)?;
        return Ok(Some((
            Source::program(program),
            format!("the AWS CLI's profile {profile}"),
        )));
    }
    let written = |found: Option<(Keys, String)>| {
        found.map(|(keys, origin)| (Source::Keys(Arc::new(keys)), origin))
    };
    if let Some(found) = keys_in(
        files.credentials.section(profile),
        profile,
        files.credentials_path.as_ref(),
    )? {
        return Ok(written(Some(found)));
    }
    if let Some(text) = files.value(profile, "credential_process") {
        let program = Program::process(&text, profile, var)?;
        return Ok(Some((
            Source::program(program),
            format!("the credential_process of profile {profile}"),
        )));
    }
    Ok(written(keys_in(
        files.config_section(profile),
        profile,
        files.config_path.as_ref(),
    )?))
}

/// the keys in one file's section for `profile`, and where they came from.
fn keys_in(
    section: Option<&HashMap<String, String>>,
    profile: &str,
    path: Option<&PathBuf>,
) -> Result<Option<(Keys, String)>, Error> {
    let Some(section) = section else {
        return Ok(None);
    };
    let value = |key: &str| section.get(key).filter(|value| !value.is_empty()).cloned();
    let shown = path.map_or_else(String::new, |path| path.display().to_string());
    let half = |given: &str, missing: &str| {
        Error::new(
            Kind::AuthRequired,
            format!("profile {profile} has {given} but no {missing}, in {shown}"),
        )
        .help(format!("add {missing} to the profile, or remove {given}"))
    };
    match (value("aws_access_key_id"), value("aws_secret_access_key")) {
        (Some(id), Some(secret)) => Ok(Some((
            Keys {
                id,
                secret,
                token: value("aws_session_token"),
            },
            format!("profile {profile} in {shown}"),
        ))),
        (None, None) => Ok(None),
        (Some(_), None) => Err(half("aws_access_key_id", "aws_secret_access_key")),
        (None, Some(_)) => Err(half("aws_secret_access_key", "aws_access_key_id")),
    }
}

/// the region the environment sets.
fn region(var: Var<'_>) -> Option<String> {
    set(var, "AWS_REGION").or_else(|| set(var, "AWS_DEFAULT_REGION"))
}

/// a variable's text, when it is set and not empty.
fn set(var: Var<'_>, name: &str) -> Option<String> {
    var(name)
        .and_then(|value| value.into_string().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// the shared credentials and config files.
struct Files {
    credentials: Ini,
    credentials_path: Option<PathBuf>,
    config: Ini,
    config_path: Option<PathBuf>,
}

impl Files {
    /// the files `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE` name, else `credentials` and
    /// `config` in `~/.aws`. a file that is not there holds nothing.
    fn read(var: Var<'_>) -> Files {
        let home = crate::source::home(var);
        let path = |variable: &str, name: &str| {
            set(var, variable)
                .map(|named| match (named.strip_prefix("~/"), &home) {
                    (Some(rest), Some(home)) => home.join(rest),
                    _ => PathBuf::from(named),
                })
                .or_else(|| home.as_ref().map(|home| home.join(".aws").join(name)))
        };
        let credentials_path = path("AWS_SHARED_CREDENTIALS_FILE", "credentials");
        let config_path = path("AWS_CONFIG_FILE", "config");
        let ini = |path: &Option<PathBuf>| {
            path.as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map_or_else(Ini::default, |text| Ini::parse(&text))
        };
        Files {
            credentials: ini(&credentials_path),
            config: ini(&config_path),
            credentials_path,
            config_path,
        }
    }

    /// the config file's section for `profile`: `[profile name]`, or `[default]` for default.
    fn config_section(&self, profile: &str) -> Option<&HashMap<String, String>> {
        self.config
            .section(&format!("profile {profile}"))
            .or_else(|| {
                (profile == "default")
                    .then(|| self.config.section("default"))
                    .flatten()
            })
    }

    fn has(&self, profile: &str) -> bool {
        self.credentials.section(profile).is_some() || self.config_section(profile).is_some()
    }

    /// what `profile` sets for `key`: in the credentials file, else in the config file.
    fn value(&self, profile: &str, key: &str) -> Option<String> {
        [
            self.credentials.section(profile),
            self.config_section(profile),
        ]
        .into_iter()
        .flatten()
        .find_map(|section| section.get(key).filter(|value| !value.is_empty()).cloned())
    }

    /// the keys `profile` sets to something, in either file.
    fn names(&self, profile: &str) -> impl Iterator<Item = &str> {
        [
            self.credentials.section(profile),
            self.config_section(profile),
        ]
        .into_iter()
        .flatten()
        .flat_map(|section| section.iter())
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, _)| key.as_str())
    }

    /// every profile either file has.
    fn profiles(&self) -> impl Iterator<Item = &str> {
        let configured = self.config.sections.keys().filter_map(|name| {
            name.strip_prefix("profile ")
                .or_else(|| (name == "default").then_some("default"))
        });
        self.credentials
            .sections
            .keys()
            .map(String::as_str)
            .chain(configured)
    }

    /// whether `AWS_IGNORE_CONFIGURED_ENDPOINT_URLS`, or the profile, turns endpoint settings off.
    fn ignores_endpoints(&self, var: Var<'_>, profile: &str) -> bool {
        set(var, "AWS_IGNORE_CONFIGURED_ENDPOINT_URLS")
            .or_else(|| self.value(profile, "ignore_configured_endpoint_urls"))
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    }

    /// where requests made with the environment's keys or `profile`'s go, when not to AWS, and
    /// the setting that says so. the variables come first, the S3 one before the general one,
    /// then the profile's own settings.
    fn sends_to(
        &self,
        var: Var<'_>,
        profile: &str,
    ) -> Result<Option<(url::Origin, String)>, Error> {
        for variable in ["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"] {
            if let Some(text) = set(var, variable) {
                return endpoint(&text, variable).map(|origin| Some((origin, variable.to_owned())));
            }
        }
        Ok(self
            .endpoint_of(profile)?
            .map(|origin| (origin, format!("the AWS profile {profile}"))))
    }

    /// the endpoint a profile names: for S3 in the `services` section it names, else for every
    /// service.
    fn endpoint_of(&self, profile: &str) -> Result<Option<url::Origin>, Error> {
        let named = format!("the AWS profile {profile}");
        let services = self
            .value(profile, "services")
            .and_then(|services| self.config.section(&format!("services {services}")))
            .and_then(|section| section.get("s3.endpoint_url"))
            .filter(|value| !value.is_empty());
        services
            .cloned()
            .or_else(|| self.value(profile, "endpoint_url"))
            .map(|text| endpoint(&text, &named))
            .transpose()
    }
}

/// the origin an endpoint setting names, such as `https://minio.lab.example:9000`.
fn endpoint(text: &str, named: &str) -> Result<url::Origin, Error> {
    url::Url::parse(text)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        .map(|url| url.origin())
        .ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!(
                    "{named} sets the endpoint \"{}\", which is not a link such as https://minio.lab.example:9000",
                    crate::net::mask_query(text)
                ),
            )
            .help("give the endpoint as a link, or unset the setting to send S3 requests to AWS")
        })
}

/// an ini file as the AWS tools read it: sections of lowercase keys. an indented line under a
/// key with no value of its own sets a key inside it, kept as `key.inner`.
#[derive(Default)]
struct Ini {
    sections: HashMap<String, HashMap<String, String>>,
}

impl Ini {
    fn parse(text: &str) -> Ini {
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current: Option<String> = None;
        // the section's last key, and whether its value was empty, so indented lines set keys
        // inside it rather than continuing its value
        let mut last: Option<(String, bool)> = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(['#', ';']) {
                continue;
            }
            if let Some(name) = trimmed
                .strip_prefix('[')
                .and_then(|rest| rest.rfind(']').map(|end| &rest[..end]))
            {
                let name = name
                    .split_whitespace()
                    .map(|word| word.trim_matches('"'))
                    .collect::<Vec<_>>()
                    .join(" ");
                sections.entry(name.clone()).or_default();
                current = Some(name);
                last = None;
                continue;
            }
            let Some(section) = &current else {
                continue;
            };
            let indented = line.starts_with(char::is_whitespace);
            let parent = match &last {
                Some((key, true)) if indented => Some(key.clone()),
                Some(_) if indented => continue,
                _ => None,
            };
            let Some(at) = trimmed.find(['=', ':']) else {
                continue;
            };
            let key = trimmed[..at].trim().to_ascii_lowercase();
            let value = trimmed[at + 1..].trim().to_owned();
            let entries = sections.entry(section.clone()).or_default();
            if let Some(parent) = parent {
                entries.insert(format!("{parent}.{key}"), value);
            } else {
                last = Some((key.clone(), value.is_empty()));
                entries.insert(key, value);
            }
        }
        Ini { sections }
    }

    fn section(&self, name: &str) -> Option<&HashMap<String, String>> {
        self.sections.get(name)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::program::By;
    use super::*;

    /// a machine whose home is `home`, with `pairs` set in its environment.
    fn machine<'a>(
        home: &'a Path,
        pairs: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            if let Some((_, value)) = pairs.iter().find(|(key, _)| *key == name) {
                return Some(OsString::from(value));
            }
            matches!(name, "HOME" | "USERPROFILE").then(|| home.as_os_str().to_owned())
        }
    }

    /// writes the shared credentials and config files in `home/.aws`.
    fn files(home: &Path, credentials: &str, config: &str) {
        let folder = home.join(".aws");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("credentials"), credentials).unwrap();
        std::fs::write(folder.join("config"), config).unwrap();
    }

    fn origin(link: &str) -> url::Origin {
        url::Url::parse(link).unwrap().origin()
    }

    /// the key id, where it came from and the region of what `find` found for AWS.
    fn found(home: &Path, pairs: &[(&str, &str)]) -> Option<(String, String, Option<String>)> {
        find(&machine(home, pairs), None).unwrap().map(|identity| {
            (
                written(&identity).id.clone(),
                identity.origin,
                identity.region,
            )
        })
    }

    /// the keys an identity holds as written, which a program never gives.
    fn written(identity: &Identity) -> &Keys {
        match &identity.source {
            Source::Keys(keys) => keys,
            Source::Program { .. } => panic!("{} runs a program", identity.origin),
        }
    }

    const KEYS: [(&str, &str); 3] = [
        ("AWS_ACCESS_KEY_ID", "AKIAENVIRONMENT"),
        ("AWS_SECRET_ACCESS_KEY", "environment-secret"),
        ("AWS_SESSION_TOKEN", "environment-session"),
    ];

    #[test]
    fn keys_in_the_environment_come_before_any_profile() {
        let home = tempfile::tempdir().unwrap();
        files(
            home.path(),
            "[default]\naws_access_key_id = AKIAPROFILE\naws_secret_access_key = profile-secret\n",
            "",
        );
        let identity = find(&machine(home.path(), &KEYS), None).unwrap().unwrap();
        assert_eq!(
            *written(&identity),
            Keys {
                id: "AKIAENVIRONMENT".to_owned(),
                secret: "environment-secret".to_owned(),
                token: Some("environment-session".to_owned()),
            }
        );
        assert_eq!(identity.origin, "AWS_ACCESS_KEY_ID");
        let profile = found(home.path(), &[]).unwrap();
        assert_eq!(profile.0, "AKIAPROFILE");
    }

    #[test]
    fn a_profile_is_read_from_the_credentials_file_before_the_config_file() {
        let home = tempfile::tempdir().unwrap();
        files(
            home.path(),
            "# keys\n[lab]\nAWS_Access_Key_Id = AKIALABCREDENTIALS\naws_secret_access_key=lab-secret\n\n[default]\naws_access_key_id = AKIADEFAULT\naws_secret_access_key = default-secret\n",
            "[profile lab]\nregion = eu-west-1\naws_access_key_id = AKIALABCONFIG\naws_secret_access_key = config-secret\n; elsewhere\n[profile  configured]\naws_access_key_id = AKIACONFIGURED\naws_secret_access_key = configured-secret\nregion = us-west-2\n",
        );
        let credentials = home.path().join(".aws").join("credentials");
        let config = home.path().join(".aws").join("config");
        assert_eq!(
            found(home.path(), &[("AWS_PROFILE", "lab")]),
            Some((
                "AKIALABCREDENTIALS".to_owned(),
                format!("profile lab in {}", credentials.display()),
                Some("eu-west-1".to_owned())
            ))
        );
        assert_eq!(
            found(home.path(), &[("AWS_PROFILE", "configured")]),
            Some((
                "AKIACONFIGURED".to_owned(),
                format!("profile configured in {}", config.display()),
                Some("us-west-2".to_owned())
            ))
        );
        assert_eq!(found(home.path(), &[]).unwrap().0, "AKIADEFAULT");
        // the variables name other files, and the region comes from the environment first
        let elsewhere = home.path().join("elsewhere");
        std::fs::write(
            &elsewhere,
            "[lab]\naws_access_key_id = AKIAELSEWHERE\naws_secret_access_key = elsewhere\n",
        )
        .unwrap();
        let elsewhere = elsewhere.to_str().unwrap();
        let pairs = [
            ("AWS_PROFILE", "lab"),
            ("AWS_SHARED_CREDENTIALS_FILE", elsewhere),
            ("AWS_DEFAULT_REGION", "ap-south-1"),
        ];
        let named = found(home.path(), &pairs).unwrap();
        assert_eq!(named.0, "AKIAELSEWHERE");
        assert_eq!(named.2.as_deref(), Some("ap-south-1"));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(found(empty.path(), &[]), None);
    }

    #[test]
    fn half_a_key_names_the_missing_half() {
        let home = tempfile::tempdir().unwrap();
        files(home.path(), "[default]\naws_access_key_id = AKIAHALF\n", "");
        let error = find(&machine(home.path(), &KEYS[..1]), None).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(
            error.message,
            "AWS_ACCESS_KEY_ID is set, but AWS_SECRET_ACCESS_KEY is not"
        );
        let error = find(&machine(home.path(), &[]), None).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(
            error.message.starts_with(
                "profile default has aws_access_key_id but no aws_secret_access_key, in "
            ),
            "{}",
            error.message
        );
    }

    /// makes a file named `name` in `folder` that this user may run, as PATH finds a program.
    fn runnable_file(folder: &Path, name: &str) -> PathBuf {
        let path = folder.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// the program an identity runs for its keys.
    fn program(identity: &Identity) -> &Program {
        match &identity.source {
            Source::Program { program, .. } => program,
            Source::Keys(_) => panic!("{} holds its keys", identity.origin),
        }
    }

    #[test]
    fn a_profile_that_signs_in_or_takes_a_role_asks_the_aws_cli() {
        let home = tempfile::tempdir().unwrap();
        files(
            home.path(),
            "[role]\naws_access_key_id = AKIAROLE\naws_secret_access_key = role-secret\n",
            "[profile role]\nrole_arn = arn:aws:iam::123456789012:role/lab\nsource_profile = base\n[profile chained]\nsource_profile = base\n[profile web]\nrole_arn = arn:aws:iam::123456789012:role/ci\nweb_identity_token_file = /var/token\n[profile sso]\nsso_session = lab\nsso_account_id = 123456789012\n[profile legacy]\nsso_start_url = https://lab.awsapps.com/start\n[profile base]\nregion = eu-west-1\n",
        );
        let tools = home.path().join("tools");
        std::fs::create_dir(&tools).unwrap();
        let aws = runnable_file(&tools, "aws");
        let path = tools.to_str().unwrap();
        // the role comes before the keys the credentials file holds, as the AWS tools take it
        for (profile, signs_in) in [
            ("role", false),
            ("chained", false),
            ("web", false),
            ("sso", true),
            ("legacy", true),
        ] {
            let pairs = [("AWS_PROFILE", profile), ("PATH", path)];
            let identity = find(&machine(home.path(), &pairs), None).unwrap().unwrap();
            assert_eq!(identity.origin, format!("the AWS CLI's profile {profile}"));
            let program = program(&identity);
            assert_eq!(program.path, aws, "{profile}");
            assert_eq!(
                program.args,
                [
                    "configure",
                    "export-credentials",
                    "--profile",
                    profile,
                    "--format",
                    "process"
                ]
            );
            assert_eq!(program.by, By::Cli { signs_in }, "{profile}");
        }
        let pairs = [("AWS_PROFILE", "role"), ("PATH", "relative")];
        let error = find(&machine(home.path(), &pairs), None).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(
            error.message,
            "the AWS profile role takes on a role, which fetchloom leaves to the AWS CLI, and no folder on PATH holds the AWS CLI"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("install the AWS CLI, version 2.9 or later")
        );
    }

    #[test]
    fn a_credential_process_is_split_into_words_and_found_by_a_full_path() {
        let home = tempfile::tempdir().unwrap();
        let tools = home.path().join("tools");
        std::fs::create_dir(&tools).unwrap();
        let keys = runnable_file(&tools, "keys");
        files(
            home.path(),
            "[written]\naws_access_key_id = AKIAWRITTEN\naws_secret_access_key = written-secret\n",
            &format!(
                "[profile named]\ncredential_process = keys --as \"two words\"\n[profile full]\ncredential_process = \"{}\" --region eu-west-1\n[profile written]\ncredential_process = keys\n[profile relative]\ncredential_process = bin/keys\n[profile open]\ncredential_process = \"keys --as\n[profile missing]\ncredential_process = missing-keys\n",
                keys.display()
            ),
        );
        let path = tools.to_str().unwrap();
        let run = |profile: &str| {
            let pairs = [("AWS_PROFILE", profile), ("PATH", path)];
            let identity = find(&machine(home.path(), &pairs), None).unwrap().unwrap();
            assert_eq!(
                identity.origin,
                format!("the credential_process of profile {profile}")
            );
            let program = program(&identity);
            assert_eq!(program.by, By::Process);
            (program.path.clone(), program.args.clone())
        };
        assert_eq!(
            run("named"),
            (
                keys.clone(),
                vec!["--as".to_owned(), "two words".to_owned()]
            )
        );
        assert_eq!(
            run("full"),
            (
                keys.clone(),
                vec!["--region".to_owned(), "eu-west-1".to_owned()]
            )
        );
        // keys in the credentials file come before a program, as the AWS tools take them
        let pairs = [("AWS_PROFILE", "written"), ("PATH", path)];
        assert_eq!(found(home.path(), &pairs).unwrap().0, "AKIAWRITTEN");
        for (profile, kind, message) in [
            (
                "relative",
                Kind::Usage,
                "the credential_process of the AWS profile relative names the program bin/keys by a relative path",
            ),
            (
                "open",
                Kind::Usage,
                "the credential_process of the AWS profile open has a quote that never closes",
            ),
            (
                "missing",
                Kind::AuthRequired,
                "the credential_process of the AWS profile missing names missing-keys, which no folder on PATH holds",
            ),
        ] {
            let pairs = [("AWS_PROFILE", profile), ("PATH", path)];
            let error = find(&machine(home.path(), &pairs), None).unwrap_err();
            assert_eq!((error.kind, error.message.as_str()), (kind, message));
        }
    }

    #[test]
    fn an_unknown_aws_profile_is_a_usage_error() {
        let home = tempfile::tempdir().unwrap();
        files(home.path(), "", "[profile lab]\nregion = eu-west-1\n");
        let error = find(&machine(home.path(), &[("AWS_PROFILE", "missing")]), None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "AWS_PROFILE names the profile missing, which the AWS credentials and config files do not have"
        );
        assert_eq!(found(home.path(), &[("AWS_PROFILE", "lab")]), None);
    }

    #[test]
    fn keys_reach_an_endpoint_only_where_the_aws_settings_send_them() {
        let home = tempfile::tempdir().unwrap();
        files(
            home.path(),
            "[default]\naws_access_key_id = AKIADEFAULT\naws_secret_access_key = default-secret\n[minio]\naws_access_key_id = AKIAMINIO\naws_secret_access_key = minio-secret\n[ceph]\naws_access_key_id = AKIACEPH\naws_secret_access_key = ceph-secret\n",
            "[profile minio]\nendpoint_url = https://minio.lab.example:9000/\nregion = lab-1\n[profile ceph]\nservices = lab-services\n[services lab-services]\ns3 =\n  endpoint_url = http://ceph.lab.example\n[profile twin]\nendpoint_url = https://twin.lab.example\n[profile other-twin]\nendpoint_url = https://twin.lab.example\n",
        );
        let id = |pairs: &[(&str, &str)], endpoint: &str| {
            find(&machine(home.path(), pairs), Some(&origin(endpoint)))
                .map(|found| found.map(|identity| (written(&identity).id.clone(), identity.region)))
        };
        // the one profile naming an endpoint signs for it, AWS keys never do
        assert_eq!(
            id(&[], "https://minio.lab.example:9000").unwrap(),
            Some(("AKIAMINIO".to_owned(), Some("lab-1".to_owned())))
        );
        assert_eq!(
            id(&KEYS, "http://ceph.lab.example").unwrap(),
            Some(("AKIACEPH".to_owned(), None))
        );
        assert_eq!(id(&KEYS, "https://unnamed.lab.example").unwrap(), None);
        let twins = id(&[], "https://twin.lab.example").unwrap_err();
        assert_eq!(twins.kind, Kind::AuthRequired);
        assert!(
            twins.message.contains("other-twin and twin"),
            "{}",
            twins.message
        );
        // the environment sends its keys to the endpoint it names, and then not to AWS
        let pairs = [
            KEYS[0],
            KEYS[1],
            ("AWS_ENDPOINT_URL_S3", "https://unnamed.lab.example"),
        ];
        assert_eq!(
            id(&pairs, "https://unnamed.lab.example").unwrap(),
            Some(("AKIAENVIRONMENT".to_owned(), None))
        );
        let elsewhere = find(&machine(home.path(), &pairs), None).unwrap_err();
        assert_eq!(elsewhere.kind, Kind::AuthRequired);
        assert_eq!(
            elsewhere.message,
            "AWS_ENDPOINT_URL_S3 sends these AWS keys to https://unnamed.lab.example, so fetchloom does not send them to AWS"
        );
        // a chosen profile with an endpoint signs there and not for AWS, unless told to ignore it
        let chosen = [("AWS_PROFILE", "minio")];
        assert!(find(&machine(home.path(), &chosen), None).is_err());
        let ignoring = [
            ("AWS_PROFILE", "minio"),
            ("AWS_IGNORE_CONFIGURED_ENDPOINT_URLS", "true"),
        ];
        assert_eq!(
            found(home.path(), &ignoring)
                .map(|found| found.0)
                .as_deref(),
            Some("AKIAMINIO")
        );
        let bad = [("AWS_ENDPOINT_URL", "minio.lab.example")];
        assert_eq!(
            find(&machine(home.path(), &bad), None).unwrap_err().kind,
            Kind::Usage
        );
    }
}
