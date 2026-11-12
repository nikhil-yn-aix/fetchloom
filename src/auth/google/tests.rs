use std::ffi::OsString;
use std::path::Path;

use super::*;
use crate::source::recorded::fixture_bytes;

const EMAIL: &str = "loader@lab-project.iam.gserviceaccount.com";
const CLIENT_SECRET: &str = "client-secret-of-the-test";
const REFRESH_TOKEN: &str = "refresh-token-of-the-test";

/// a machine whose home is `home`, with `pairs` set in its environment.
fn machine<'a>(
    home: &'a Path,
    pairs: &'a [(&'a str, &'a str)],
) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        if let Some((_, value)) = pairs.iter().find(|(key, _)| *key == name) {
            return Some(OsString::from(value));
        }
        (name == "HOME").then(|| home.as_os_str().to_owned())
    }
}

/// the test key, a PKCS#8 PEM that openssl made.
fn test_key() -> String {
    String::from_utf8(fixture_bytes("google/test-key.pem")).unwrap()
}

/// a service account's JSON key with `key`, as the google cloud console downloads one.
fn key_file(key: &str, token_uri: &str) -> String {
    serde_json::json!({
        "type": "service_account",
        "project_id": "lab-project",
        "private_key_id": "test-key-id",
        "private_key": key,
        "client_email": EMAIL,
        "client_id": "100000000000000000001",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": token_uri,
        "universe_domain": "googleapis.com"
    })
    .to_string()
}

fn service_account(token_uri: &str) -> String {
    key_file(&test_key(), token_uri)
}

/// a person's login, as `gcloud auth application-default login` writes it.
fn login() -> String {
    serde_json::json!({
        "account": "",
        "client_id": "764086051850-test.apps.googleusercontent.com",
        "client_secret": CLIENT_SECRET,
        "refresh_token": REFRESH_TOKEN,
        "type": "authorized_user",
        "universe_domain": "googleapis.com"
    })
    .to_string()
}

/// writes `text` at `path`, making its folder.
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// what `found` found, as messages show it.
fn shown(var: &dyn Fn(&str) -> Option<OsString>, windows: bool) -> Option<String> {
    found(var, windows)
        .unwrap()
        .map(|credentials| credentials.shown())
}

/// the credentials in `text`, as if gcloud's own file held them.
fn credentials(text: &str) -> Credentials {
    read(text.as_bytes(), Path::new("/home/ada/key.json"), false).unwrap()
}

/// base64 with the URL alphabet, as a JSON web token's parts are written.
fn decode(text: &str) -> Vec<u8> {
    let mut bits = 0u32;
    let mut held = 0;
    let mut bytes = Vec::new();
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            other => panic!("{other} is not url base64 without padding"),
        };
        bits = (bits << 6) | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from((bits >> held) & 0xff).unwrap());
        }
    }
    bytes
}

fn pairs(form: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(form.as_bytes())
        .into_owned()
        .collect()
}

/// an environment, whether it is windows, and what is found there.
type Case<'a> = (&'a [(&'a str, &'a str)], bool, String);

#[test]
fn credentials_come_from_the_named_file_before_gclouds_own() {
    let home = tempfile::tempdir().unwrap();
    // joined part by part, as fetchloom joins them, so windows shows the same separators
    let gcloud = home
        .path()
        .join(".config")
        .join("gcloud")
        .join("application_default_credentials.json");
    write(&gcloud, &login());
    let key = home.path().join("keys").join("lab.json");
    write(&key, &service_account(TOKEN_ENDPOINT));
    let elsewhere = home.path().join("elsewhere");
    write(
        &elsewhere.join("application_default_credentials.json"),
        &login(),
    );
    let appdata = home.path().join("AppData/Roaming");
    write(
        &appdata.join("gcloud/application_default_credentials.json"),
        &login(),
    );
    let key_text = key.to_str().unwrap();
    let cases: [Case<'_>; 5] = [
        (
            &[],
            false,
            format!("the google login in {}", gcloud.display()),
        ),
        (
            &[("GOOGLE_APPLICATION_CREDENTIALS", key_text)],
            false,
            format!("the service account {EMAIL} in {}", key.display()),
        ),
        (
            &[("CLOUDSDK_CONFIG", elsewhere.to_str().unwrap())],
            false,
            format!(
                "the google login in {}",
                elsewhere
                    .join("application_default_credentials.json")
                    .display()
            ),
        ),
        (
            &[("APPDATA", appdata.to_str().unwrap())],
            true,
            format!(
                "the google login in {}",
                appdata
                    .join("gcloud")
                    .join("application_default_credentials.json")
                    .display()
            ),
        ),
        (
            &[
                ("GOOGLE_APPLICATION_CREDENTIALS", ""),
                ("CLOUDSDK_CONFIG", ""),
            ],
            false,
            format!("the google login in {}", gcloud.display()),
        ),
    ];
    for (pairs, windows, expected) in cases {
        assert_eq!(
            shown(&machine(home.path(), pairs), windows).as_deref(),
            Some(expected.as_str()),
            "{pairs:?}"
        );
    }
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(shown(&machine(empty.path(), &[]), false), None);
    assert_eq!(shown(&machine(empty.path(), &[]), true), None);
}

/// so fetchloom never signs in as whoever gcloud's file names instead.
#[test]
fn a_named_file_that_cannot_be_used_stops_the_search() {
    let home = tempfile::tempdir().unwrap();
    write(
        &home
            .path()
            .join(".config/gcloud/application_default_credentials.json"),
        &login(),
    );
    let missing = home.path().join("missing.json");
    let pairs = [("GOOGLE_APPLICATION_CREDENTIALS", missing.to_str().unwrap())];
    let error = found(&machine(home.path(), &pairs), false).err().unwrap();
    assert_eq!(error.kind, Kind::Usage);
    assert!(
        error.message.starts_with(&format!(
            "GOOGLE_APPLICATION_CREDENTIALS names {}, which fetchloom cannot read",
            missing.display()
        )),
        "{}",
        error.message
    );
    for (kind, named) in [
        ("external_account", "workload identity federation"),
        (
            "impersonated_service_account",
            "service account impersonation",
        ),
        (
            "external_account_authorized_user",
            "workforce identity federation",
        ),
        ("gdch_service_account", "the type gdch_service_account"),
    ] {
        let file = home.path().join(format!("{kind}.json"));
        write(
            &file,
            &serde_json::json!({"type": kind, "audience": "//iam.googleapis.com/x"}).to_string(),
        );
        let pairs = [("GOOGLE_APPLICATION_CREDENTIALS", file.to_str().unwrap())];
        let error = found(&machine(home.path(), &pairs), false).err().unwrap();
        assert_eq!(error.kind, Kind::AuthRequired, "{kind}");
        assert_eq!(
            error.message,
            format!(
                "{} holds credentials for {named}, which fetchloom does not read",
                file.display()
            ),
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "run `gcloud auth application-default login`, or set GOOGLE_APPLICATION_CREDENTIALS to a service account's key file"
            ),
            "{kind}"
        );
    }
}

#[test]
fn a_file_naming_a_token_endpoint_other_than_googles_is_refused() {
    let path = Path::new("/home/ada/key.json");
    for uri in KNOWN_ENDPOINTS {
        assert!(
            read(service_account(uri).as_bytes(), path, true).is_ok(),
            "{uri}"
        );
    }
    for uri in [
        "https://tokens.lab.example/token?key=uri-secret",
        "http://oauth2.googleapis.com/token",
        "https://oauth2.googleapis.com:8443/token",
        "https://oauth2.googleapis.com.lab.example/token",
    ] {
        let error = read(service_account(uri).as_bytes(), path, true)
            .err()
            .unwrap();
        assert_eq!(error.kind, Kind::AuthRequired, "{uri}");
        assert!(
            error.message.starts_with(
                "/home/ada/key.json sends its token requests to a server other than google's: "
            ),
            "{}",
            error.message
        );
        assert!(!error.message.contains("uri-secret"), "{}", error.message);
    }
}

/// the assertion is checked with the public half of the test key, as openssl wrote it, so the
/// check does not lean on the code that signed it.
#[test]
fn a_service_account_asks_with_a_token_its_key_signed() {
    let credentials = credentials(&service_account(TOKEN_ENDPOINT));
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let form = pairs(&credentials.grant(STORAGE_READ, now).unwrap());
    assert_eq!(form.len(), 2, "{form:?}");
    assert_eq!(
        form[0],
        (
            "grant_type".to_owned(),
            "urn:ietf:params:oauth:grant-type:jwt-bearer".to_owned()
        )
    );
    assert_eq!(form[1].0, "assertion");
    let parts: Vec<&str> = form[1].1.split('.').collect();
    assert_eq!(parts.len(), 3, "{}", form[1].1);
    let header: serde_json::Value = serde_json::from_slice(&decode(parts[0])).unwrap();
    assert_eq!(
        header,
        serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": "test-key-id"})
    );
    let claims: serde_json::Value = serde_json::from_slice(&decode(parts[1])).unwrap();
    assert_eq!(
        claims,
        serde_json::json!({
            "iss": EMAIL,
            "scope": STORAGE_READ,
            "aud": "https://oauth2.googleapis.com/token",
            "iat": 1_700_000_000,
            "exp": 1_700_003_600,
        })
    );
    let public = fixture_bytes("google/test-key.public.der");
    let key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        &public,
    );
    let signed = format!("{}.{}", parts[0], parts[1]);
    let signature = decode(parts[2]);
    assert_eq!(signature.len(), 256);
    assert!(key.verify(signed.as_bytes(), &signature).is_ok());
    let forged = format!("{}.{}", parts[0], url_base64(br#"{"iss":"someone"}"#));
    assert!(key.verify(forged.as_bytes(), &signature).is_err());
}

#[test]
fn a_login_asks_with_its_refresh_token_and_client() {
    let credentials = credentials(&login());
    let form = pairs(
        &credentials
            .grant(STORAGE_READ, UNIX_EPOCH + Duration::from_secs(1))
            .unwrap(),
    );
    assert_eq!(
        form,
        [
            ("grant_type", "refresh_token"),
            ("client_id", "764086051850-test.apps.googleusercontent.com"),
            ("client_secret", CLIENT_SECRET),
            ("refresh_token", REFRESH_TOKEN),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
    );
}

#[test]
fn the_token_endpoints_answer_gives_the_token_and_how_long_it_lives() {
    let credentials = credentials(&login());
    let token = credentials
        .answer(
            200,
            br#"{"access_token":"ya29.test-token","expires_in":3599,"token_type":"Bearer","scope":"https://www.googleapis.com/auth/cloud-platform openid"}"#,
        )
        .unwrap();
    assert_eq!(token.value, "ya29.test-token");
    assert_eq!(token.lifetime, Duration::from_secs(3599));
    let unstated = credentials
        .answer(
            200,
            br#"{"access_token":"ya29.unstated","token_type":"bearer"}"#,
        )
        .unwrap();
    assert_eq!(unstated.lifetime, Duration::from_secs(3600));
    for body in [
        &br#"{"token_type":"Bearer","expires_in":3599}"#[..],
        br#"{"access_token":"ya29.mac-token","token_type":"mac"}"#,
        br#"{"access_token":"","token_type":"Bearer"}"#,
        br#"{"access_token":"ya29.line\nbreak","token_type":"Bearer"}"#,
        b"<html>not json</html>",
    ] {
        let error = credentials.answer(200, body).err().unwrap();
        assert_eq!(error.kind, Kind::SourceInvalid, "{}", error.message);
        assert!(!error.message.contains("ya29"), "{}", error.message);
    }
}

#[test]
fn a_refused_grant_says_what_google_said_and_how_to_sign_in_again() {
    let person = credentials(&login());
    let revoked = person
        .answer(
            400,
            br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        )
        .err()
        .unwrap();
    assert_eq!(revoked.kind, Kind::AuthRequired);
    assert_eq!(
        revoked.message,
        "google refused the google login in /home/ada/key.json"
    );
    assert_eq!(
        revoked.details,
        ["google said invalid_grant: Token has been expired or revoked."]
    );
    assert_eq!(
        revoked.help.as_deref(),
        Some("sign in again with `gcloud auth application-default login`")
    );
    let service = credentials(&service_account(TOKEN_ENDPOINT));
    let disabled = service
        .answer(
            400,
            br#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#,
        )
        .err()
        .unwrap();
    assert_eq!(
        disabled.message,
        format!("google refused the service account {EMAIL} in /home/ada/key.json")
    );
    assert_eq!(
        disabled.help.as_deref().unwrap(),
        format!(
            "the key may be deleted or disabled: make a new key for {EMAIL}, or set GOOGLE_APPLICATION_CREDENTIALS to another one"
        )
    );
    let named = read(login().as_bytes(), Path::new("/home/ada/login.json"), true)
        .unwrap()
        .answer(400, br#"{"error":"invalid_grant"}"#)
        .err()
        .unwrap();
    assert_eq!(named.details, ["google said invalid_grant"]);
    assert_eq!(
        named.help.as_deref(),
        Some(
            "sign in again with `gcloud auth application-default login`, then unset GOOGLE_APPLICATION_CREDENTIALS, which names this file"
        )
    );
    let silent = person.answer(401, b"").err().unwrap();
    assert_eq!(silent.details, ["google's token endpoint answered 401"]);
    let noisy = person
        .answer(
            400,
            "{\"error\":\"invalid_grant\",\"error_description\":\"two\\nlines\"}".as_bytes(),
        )
        .err()
        .unwrap();
    assert_eq!(noisy.details, ["google said invalid_grant: two\\nlines"]);
}

/// however the credentials file or the token endpoint's answer is wrong.
#[test]
fn no_secret_reaches_a_message() {
    let path = Path::new("/home/ada/key.json");
    let key = test_key();
    let line = key.lines().nth(5).unwrap().to_owned();
    let mangled = key.replace(&line, &line.chars().rev().collect::<String>());
    let mut errors = Vec::new();
    for text in [
        key_file(&mangled, TOKEN_ENDPOINT),
        key_file("secret-key-text", TOKEN_ENDPOINT),
        serde_json::json!({"type": "authorized_user", "client_id": "id", "client_secret": CLIENT_SECRET}).to_string(),
        serde_json::json!({"type": "authorized_user", "refresh_token": REFRESH_TOKEN, "client_secret": 7}).to_string(),
        format!("{{\"type\": \"authorized_user\", \"refresh_token\": \"{REFRESH_TOKEN}\""),
    ] {
        errors.push(read(text.as_bytes(), path, true).err().unwrap());
    }
    errors.push(
        credentials(&login())
            .answer(
                200,
                br#"{"access_token":"ya29.secret token\u0000","token_type":"Bearer"}"#,
            )
            .err()
            .unwrap(),
    );
    for error in errors {
        let said = format!("{} {:?} {:?}", error.message, error.details, error.help);
        for secret in [
            line.as_str(),
            "secret-key-text",
            CLIENT_SECRET,
            REFRESH_TOKEN,
            "ya29",
        ] {
            assert!(!said.contains(secret), "{said}");
        }
    }
}
