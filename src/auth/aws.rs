//! AWS keys where the AWS tools keep them: the environment, then the profile `AWS_PROFILE` names,
//! else `default`, in the shared credentials and config files. a profile that gets its keys
//! another way, by a role, a sign-in or a program, is refused rather than passed over for
//! another identity, and keys reach an S3-compatible endpoint only where the person's own AWS
//! settings send them there.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::{Error, Kind};
use crate::net::sign::Keys;
use crate::source::Var;

/// keys found for one endpoint, and where.
#[derive(Clone, Debug)]
pub(crate) struct Identity {
    pub(crate) keys: Keys,
    /// where the keys came from, as messages name it: `AWS_ACCESS_KEY_ID`, or `profile lab in
    /// /home/ada/.aws/credentials`.
    pub(crate) origin: String,
    /// the region set beside the keys, by `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile.
    pub(crate) region: Option<String>,
}

/// the keys that sign for `endpoint`, or for AWS itself when it is none. the environment's keys,
/// else the chosen profile's, sign where the settings send S3 requests: AWS, unless
/// `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT_URL` or the profile names an endpoint. another endpoint
/// takes the keys of the one profile whose `endpoint_url` names it. none when no keys are there.
///
/// # Errors
///
/// `usage` when `AWS_PROFILE` names a profile neither file has, or an endpoint setting is no
/// link; `auth.required` for half a key, a profile that gets its keys another way, settings that
/// send these keys to another endpoint than the one asked for, or two profiles naming it.
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
        return Ok(own(var, &files, profile)?.map(|(keys, origin)| Identity {
            keys,
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
            Ok(profile_keys(&files, name)?.map(|(keys, origin)| Identity {
                keys,
                origin,
                region,
            }))
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

/// the environment's keys, else the profile's, and where they came from.
fn own(var: Var<'_>, files: &Files, profile: &str) -> Result<Option<(Keys, String)>, Error> {
    let id = set(var, "AWS_ACCESS_KEY_ID");
    let secret = set(var, "AWS_SECRET_ACCESS_KEY");
    match (id, secret) {
        (Some(id), Some(secret)) => Ok(Some((
            Keys {
                id,
                secret,
                token: set(var, "AWS_SESSION_TOKEN"),
            },
            "AWS_ACCESS_KEY_ID".to_owned(),
        ))),
        (None, None) => profile_keys(files, profile),
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

/// the keys a profile holds: in the credentials file, else in the config file. a profile that
/// gets them by a role, a sign-in or a program is refused, in the order the AWS tools try them.
fn profile_keys(files: &Files, profile: &str) -> Result<Option<(Keys, String)>, Error> {
    let has = |key: &str| files.value(profile, key).is_some();
    let other = if has("role_arn") {
        Some("by taking on a role")
    } else if has("web_identity_token_file") {
        Some("from a web identity token")
    } else if has("sso_session") || has("sso_start_url") {
        Some("by signing in to IAM identity center")
    } else {
        None
    };
    let unsupported = |how: &str| {
        Error::new(
            Kind::AuthRequired,
            format!("the AWS profile {profile} gets its keys {how}, which fetchloom does not do"),
        )
        .help(format!(
            "set the variables `aws configure export-credentials --profile {profile} --format env` prints, or set AWS_PROFILE to a profile that holds aws_access_key_id and aws_secret_access_key"
        ))
    };
    if let Some(how) = other {
        return Err(unsupported(how));
    }
    if let Some(found) = keys_in(
        files.credentials.section(profile),
        profile,
        files.credentials_path.as_ref(),
    )? {
        return Ok(Some(found));
    }
    if has("credential_process") {
        return Err(unsupported("from a program, its credential_process"));
    }
    keys_in(
        files.config_section(profile),
        profile,
        files.config_path.as_ref(),
    )
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
        find(&machine(home, pairs), None)
            .unwrap()
            .map(|identity| (identity.keys.id, identity.origin, identity.region))
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
            identity.keys,
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

    #[test]
    fn a_profile_that_gets_its_keys_another_way_is_refused() {
        let home = tempfile::tempdir().unwrap();
        files(
            home.path(),
            "[process]\naws_access_key_id = AKIAPROCESS\naws_secret_access_key = process-secret\n",
            "[profile role]\nrole_arn = arn:aws:iam::123456789012:role/lab\nsource_profile = default\n[profile sso]\nsso_session = lab\n[profile web]\nweb_identity_token_file = /var/token\n[profile process]\ncredential_process = /usr/bin/keys\n[profile program]\ncredential_process = /usr/bin/keys\n",
        );
        for (profile, how) in [
            ("role", "by taking on a role"),
            ("sso", "by signing in to IAM identity center"),
            ("web", "from a web identity token"),
            ("program", "from a program, its credential_process"),
        ] {
            let error = find(&machine(home.path(), &[("AWS_PROFILE", profile)]), None).unwrap_err();
            assert_eq!(error.kind, Kind::AuthRequired, "{profile}");
            assert_eq!(
                error.message,
                format!(
                    "the AWS profile {profile} gets its keys {how}, which fetchloom does not do"
                ),
            );
            assert!(
                error.help.unwrap().contains(&format!(
                    "aws configure export-credentials --profile {profile} --format env"
                )),
                "{profile}"
            );
        }
        // keys in the credentials file come before a program, as the AWS tools take them
        assert_eq!(
            found(home.path(), &[("AWS_PROFILE", "process")]).unwrap().0,
            "AKIAPROCESS"
        );
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
                .map(|found| found.map(|identity| (identity.keys.id, identity.region)))
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
