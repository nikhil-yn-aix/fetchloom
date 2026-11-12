use std::ffi::OsString;
use std::fs;
use std::path::Path;

use super::*;

/// a machine whose home is `home`, with `pairs` set in its environment.
fn machine<'a>(
    home: &'a Path,
    pairs: &'a [(&'a str, &'a str)],
) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        if let Some((_, value)) = pairs.iter().rev().find(|(key, _)| *key == name) {
            return Some(OsString::from(value));
        }
        match name {
            "HOME" | "USERPROFILE" | "APPDATA" => Some(home.as_os_str().to_owned()),
            "XDG_CONFIG_HOME" => Some(home.join(".config").into_os_string()),
            "FETCHLOOM_KEYRING" => Some("0".into()),
            _ => None,
        }
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn the_hugging_face_token_is_found_where_its_tools_look() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let found = |pairs: &[(&str, &str)]| {
        Account::HuggingFace
            .find(&machine(root, pairs))
            .map(|found| found.secret)
    };
    assert_eq!(found(&[]), None);
    write(&root.join(".cache/huggingface/token"), "hf_cache\n");
    assert_eq!(found(&[]).as_deref(), Some("hf_cache"));
    let xdg = root.join("xdg");
    write(&xdg.join("huggingface/token"), "hf_xdg");
    let xdg = xdg.to_str().unwrap();
    assert_eq!(found(&[("XDG_CACHE_HOME", xdg)]).as_deref(), Some("hf_xdg"));
    let hf_home = root.join("hf");
    write(&hf_home.join("token"), "hf_home");
    let hf_home = hf_home.to_str().unwrap();
    assert_eq!(
        found(&[("XDG_CACHE_HOME", xdg), ("HF_HOME", hf_home)]).as_deref(),
        Some("hf_home")
    );
    let named = root.join("named");
    write(&named, " hf_named ");
    let named = named.to_str().unwrap();
    assert_eq!(
        found(&[("HF_HOME", hf_home), ("HF_TOKEN_PATH", named)]).as_deref(),
        Some("hf_named")
    );
    assert_eq!(
        found(&[("HF_TOKEN_PATH", named), ("HF_TOKEN", "hf_env")]).as_deref(),
        Some("hf_env")
    );
}

#[test]
fn each_key_goes_to_its_own_host_in_its_own_header() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    write(
        &root.join(".netrc"),
        "machine urs.earthdata.nasa.gov login ada password pw\n",
    );
    let var = machine(
        root,
        &[
            ("HF_TOKEN", "hf_abc"),
            ("ZENODO_TOKEN", "z"),
            ("FIGSHARE_TOKEN", "f"),
            ("OSF_TOKEN", "o"),
            ("DRYAD_TOKEN", "d"),
            ("DATAVERSE_URL", "https://demo.dataverse.org/"),
            ("DATAVERSE_API_TOKEN", "dv"),
        ],
    );
    let mut sent: Vec<(String, &str, String, bool)> = credentials(&var)
        .into_iter()
        .map(|c| (c.host, c.header, c.value, c.every_hop))
        .collect();
    sent.sort();
    let expected = [
        ("api.figshare.com", "authorization", "token f", false),
        ("api.osf.io", "authorization", "Bearer o", false),
        ("datadryad.org", "authorization", "Bearer d", false),
        ("demo.dataverse.org", "x-dataverse-key", "dv", false),
        ("files.osf.io", "authorization", "Bearer o", true),
        ("huggingface.co", "authorization", "Bearer hf_abc", false),
        ("osf.io", "authorization", "Bearer o", true),
        (
            "urs.earthdata.nasa.gov",
            "authorization",
            "Basic YWRhOnB3",
            true,
        ),
        ("zenodo.org", "authorization", "Bearer z", false),
    ]
    .map(|(host, header, value, every)| (host.to_owned(), header, value.to_owned(), every));
    assert_eq!(sent, expected);
}

#[test]
fn a_dataverse_token_goes_only_to_the_installation_it_names() {
    let home = tempfile::tempdir().unwrap();
    let var = machine(home.path(), &[("DATAVERSE_API_TOKEN", "dv")]);
    assert!(
        credentials(&var)
            .iter()
            .all(|c| c.header != "x-dataverse-key")
    );
    let var = machine(
        home.path(),
        &[
            ("DATAVERSE_URL", "https://other.edu"),
            ("DATAVERSE_API_TOKEN", "dv"),
        ],
    );
    assert_eq!(
        Account::Dataverse("dataverse.harvard.edu".into())
            .find(&var)
            .map(|f| f.secret),
        None
    );
}

#[test]
fn a_dataverse_host_keeps_its_port_and_nothing_else() {
    let parse = |host: &str| Account::parse("dataverse", Some(host));
    for (typed, account) in [
        ("dv.lab:8443", "dv.lab:8443"),
        ("https://DV.lab:8443/", "dv.lab:8443"),
        ("dv.lab:443", "dv.lab"),
        ("https://dv.lab", "dv.lab"),
        ("[::1]:8443", "[::1]:8443"),
    ] {
        assert_eq!(
            parse(typed).unwrap(),
            Account::Dataverse(account.to_owned()),
            "{typed}"
        );
    }
    for typed in [
        "user:pw@dv.lab",
        "https://user@dv.lab/",
        "dv.lab/path",
        "dv.lab?x=1",
        "dv.lab#top",
        "dv.lab:port",
        "",
    ] {
        let error = parse(typed).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{typed}");
        assert!(!error.message.contains("pw"), "{}", error.message);
    }
    let account = parse("dv.lab:8443").unwrap();
    let found = Found {
        origin: Origin::Keyring,
        secret: "k".to_owned(),
        login: None,
    };
    assert_eq!(account.credential(&found).unwrap().host, "dv.lab:8443");
    assert_eq!(
        account.check().unwrap().url.as_str(),
        "https://dv.lab:8443/api/users/:me"
    );
    assert_eq!(
        account.command(),
        "fetchloom auth login dataverse --host dv.lab:8443"
    );
}

#[test]
fn a_dataverse_url_with_a_port_names_that_installation() {
    let home = tempfile::tempdir().unwrap();
    let var = machine(
        home.path(),
        &[
            ("DATAVERSE_URL", "https://dv.lab:8443/"),
            ("DATAVERSE_API_TOKEN", "dv"),
        ],
    );
    assert_eq!(dataverses(&var), ["dv.lab:8443"]);
    let account = Account::Dataverse("dv.lab:8443".into());
    assert_eq!(account.find(&var).map(|f| f.secret).as_deref(), Some("dv"));
    assert_eq!(
        Account::Dataverse("dv.lab".into())
            .find(&var)
            .map(|f| f.secret),
        None
    );
    assert!(
        credentials(&var)
            .iter()
            .any(|c| c.host == "dv.lab:8443" && c.header == "x-dataverse-key")
    );
}

#[test]
fn signing_in_saves_where_the_sources_own_tools_read() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let var = machine(root, &[]);
    assert_eq!(
        Account::HuggingFace.home(&var, "hf_x"),
        Home::File(root.join(".cache/huggingface/token"))
    );
    assert_eq!(
        Account::Kaggle.home(&var, "KGAT_x"),
        Home::File(root.join(".kaggle/access_token"))
    );
    // the kaggle CLI's own folder: on linux without ~/.kaggle, the one in XDG_CONFIG_HOME
    let legacy = if cfg!(target_os = "linux") {
        root.join(".config/kaggle/kaggle.json")
    } else {
        root.join(".kaggle/kaggle.json")
    };
    assert_eq!(
        Account::Kaggle.home(&var, "{\"username\":\"a\",\"key\":\"b\"}"),
        Home::File(legacy)
    );
    assert_eq!(
        Account::Login("physionet.org".into()).home(&var, "pw"),
        Home::Netrc(root.join(".netrc"))
    );
    assert_eq!(Account::Zenodo.home(&var, "z"), Home::Store);
    // the kaggle CLI reads kaggle.json from KAGGLE_CONFIG_DIR, kagglesdk the token from ~/.kaggle
    let var = machine(root, &[("KAGGLE_CONFIG_DIR", "/elsewhere/kaggle")]);
    assert_eq!(
        Account::Kaggle.home(&var, "KGAT_x"),
        Home::File(root.join(".kaggle/access_token"))
    );
    assert_eq!(
        Account::Kaggle.home(&var, "{\"username\":\"a\",\"key\":\"b\"}"),
        Home::File(PathBuf::from("/elsewhere/kaggle/kaggle.json"))
    );
    let var = machine(root, &[("HF_HOME", "/elsewhere/hf")]);
    assert_eq!(
        Account::HuggingFace.home(&var, "hf_x"),
        Home::File(PathBuf::from("/elsewhere/hf/token"))
    );
}

#[test]
fn a_key_saved_in_the_store_is_found_after_its_variable() {
    let home = tempfile::tempdir().unwrap();
    let var = machine(home.path(), &[]);
    store::save(&var, "zenodo", "stored", store::Place::File).unwrap();
    let found = Account::Zenodo.find(&var).unwrap();
    assert_eq!(found.secret, "stored");
    assert!(matches!(found.origin, Origin::File(_)));
    let var = machine(home.path(), &[("ZENODO_TOKEN", "env")]);
    let found = Account::Zenodo.find(&var).unwrap();
    assert_eq!(
        (found.secret.as_str(), found.origin),
        ("env", Origin::Variable("ZENODO_TOKEN"))
    );
}

#[test]
fn kaggle_keys_are_found_in_the_kaggle_tools_order() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    write(
        &root.join(".kaggle/kaggle.json"),
        r#"{"username":"ada","key":"legacy"}"#,
    );
    let found = Account::Kaggle.find(&machine(root, &[])).unwrap();
    assert_eq!(
        (found.login.as_deref(), found.secret.as_str()),
        (Some("ada"), "legacy")
    );
    write(&root.join(".kaggle/access_token"), "KGAT_file\n");
    assert_eq!(
        Account::Kaggle.find(&machine(root, &[])).unwrap().secret,
        "KGAT_file"
    );
    let var = machine(root, &[("KAGGLE_API_TOKEN", "KGAT_env")]);
    assert_eq!(Account::Kaggle.find(&var).unwrap().secret, "KGAT_env");
    let named = root.join("secrets/kaggle");
    write(&named, "KGAT_named\n");
    let named = named.to_str().unwrap();
    let pairs = [("KAGGLE_API_TOKEN", named)];
    let var = machine(root, &pairs);
    assert_eq!(
        Account::Kaggle.find(&var).unwrap().secret,
        "KGAT_named",
        "a variable naming a file is read as downloads read it"
    );
}

#[test]
fn auth_checks_a_kaggle_key_with_the_header_downloads_send() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    write(
        &root.join(".kaggle/kaggle.json"),
        r#"{"username":"ada","key":"legacy"}"#,
    );
    let checked = |pairs: &[(&str, &str)]| {
        let var = machine(root, pairs);
        let account = Account::Kaggle;
        let checked = account
            .find(&var)
            .and_then(|found| account.credential(&found))
            .map(|credential| credential.value);
        let sent = crate::source::kaggle::credentials(&var)
            .into_iter()
            .next()
            .map(|credential| credential.value);
        assert_eq!(checked, sent, "{pairs:?}");
        checked
    };
    assert_eq!(checked(&[]), Some(basic("ada", "legacy")));
    // one field from the environment, the other from kaggle.json, as the kaggle CLI merges them
    assert_eq!(
        checked(&[("KAGGLE_KEY", "override")]),
        Some(basic("ada", "override"))
    );
    assert_eq!(
        checked(&[("KAGGLE_USERNAME", "grace")]),
        Some(basic("grace", "legacy"))
    );
    let empty = root.join("empty-token");
    write(&empty, "  \n");
    write(&root.join(".kaggle/access_token"), "KGAT_home");
    // a variable naming an empty file leaves only the legacy key, never the home token
    assert_eq!(
        checked(&[("KAGGLE_API_TOKEN", empty.to_str().unwrap())]),
        Some(basic("ada", "legacy"))
    );
    let nowhere = tempfile::tempdir().unwrap();
    let var = machine(nowhere.path(), &[("KAGGLE_KEY", "alone")]);
    assert!(
        Account::Kaggle.find(&var).is_none(),
        "a key needs its username"
    );
    assert!(crate::source::kaggle::credentials(&var).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn auth_finds_kaggle_json_in_xdg_config_as_downloads_do() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    write(
        &root.join(".config/kaggle/kaggle.json"),
        r#"{"username":"ada","key":"legacy"}"#,
    );
    let found = Account::Kaggle.find(&machine(root, &[])).unwrap();
    assert_eq!(found.secret, "legacy");
}

#[test]
fn names_people_type_find_their_account() {
    let parse = |name: &str| Account::parse(name, None).unwrap();
    assert_eq!(parse("HF"), Account::HuggingFace);
    assert_eq!(parse("hugging-face"), Account::HuggingFace);
    assert_eq!(
        parse("earthdata"),
        Account::Login("urs.earthdata.nasa.gov".into())
    );
    assert_eq!(parse("PhysioNet"), Account::Login("physionet.org".into()));
    assert_eq!(
        parse("data.example.edu"),
        Account::Login("data.example.edu".into())
    );
    assert_eq!(
        parse("dataverse"),
        Account::Dataverse("dataverse.harvard.edu".into())
    );
    assert_eq!(
        Account::parse("dataverse", Some("https://demo.dataverse.org/")).unwrap(),
        Account::Dataverse("demo.dataverse.org".into())
    );
    assert_eq!(
        Account::parse("github", None).unwrap_err().kind,
        Kind::Usage
    );
    assert_eq!(
        Account::parse("hf", Some("x.org")).unwrap_err().kind,
        Kind::Usage
    );
    for name in ["intended.example\nmachine\tother.example", "a.example\tb"] {
        assert_eq!(Account::parse(name, None).unwrap_err().kind, Kind::Usage);
    }
    assert_eq!(
        Account::parse("dataverse", Some("demo.example\nmachine other.example"))
            .unwrap_err()
            .kind,
        Kind::Usage
    );
    assert_eq!(
        Account::Dataverse("demo.dataverse.org".into()).command(),
        "fetchloom auth login dataverse --host demo.dataverse.org"
    );
    assert_eq!(
        Account::Login("urs.earthdata.nasa.gov".into()).command(),
        "fetchloom auth login earthdata"
    );
}

/// a key with a line break inside cannot go in a header, so it is never sent, and `auth` says
/// so instead of the request going out without it in silence.
#[test]
fn a_key_a_header_cannot_carry_is_not_sendable() {
    let home = tempfile::tempdir().unwrap();
    let var = machine(home.path(), &[("HF_TOKEN", "hf_one\nhf_two")]);
    let found = Account::HuggingFace.find(&var).unwrap();
    assert!(!Account::HuggingFace.sendable(&found));
    let var = machine(home.path(), &[("HF_TOKEN", "hf_one")]);
    let found = Account::HuggingFace.find(&var).unwrap();
    assert!(Account::HuggingFace.sendable(&found));
    let login = Found {
        origin: Origin::Keyring,
        secret: "pw".to_owned(),
        login: Some("ada\r\nx-other: 1".to_owned()),
    };
    // a login travels as base64, which any header carries
    assert!(Account::Login("data.example.edu".into()).sendable(&login));
}

#[test]
fn a_netrc_others_can_open_is_told_apart() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".netrc");
    assert!(!netrc::too_open(&path), "a missing file is no risk");
    netrc::save(&path, "a.example", "ada", "pw").unwrap();
    assert!(!netrc::too_open(&path), "saved for its owner only");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(netrc::too_open(&path));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o620)).unwrap();
        assert!(
            netrc::too_open(&path),
            "python's netrc refuses any access but the owner's"
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!netrc::too_open(&path));
    }
}

#[test]
fn account_hosts_cannot_carry_url_components() {
    for name in [
        "example.org?token=mask_canary",
        "example.org#client_secret=mask_canary",
        "https%3A%2F%2Freader%3Amask_canary%40example.org",
    ] {
        let error = Account::parse(name, None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert!(!error.message.contains("mask_canary"));
    }
    assert_eq!(
        Account::parse("DATA.example.org", None).unwrap(),
        Account::Login("data.example.org".to_owned())
    );
}

#[test]
fn stored_account_hosts_cannot_become_displayed_secret_urls() {
    let home = tempfile::tempdir().unwrap();
    write(
        &home.path().join(".netrc"),
        "machine example.org?token=mask_canary login ada password pw\nmachine example.org:8443 login ada password pw\n",
    );
    let listed = listed(&machine(home.path(), &[]));
    assert!(
        listed
            .iter()
            .all(|account| !account.id().contains("mask_canary"))
    );
    assert!(listed.contains(&Account::Login("example.org:8443".to_owned())));
}

#[test]
fn a_guide_starts_with_the_account_and_ends_with_the_terms_a_dataset_needs() {
    for account in [
        Account::HuggingFace,
        Account::Kaggle,
        Account::Zenodo,
        Account::Figshare,
        Account::Osf,
    ] {
        let guide = account.guide();
        let Some(Step::Do(first)) = guide.first() else {
            panic!("{account:?}: {guide:?}");
        };
        assert!(
            first.starts_with("make a free account at "),
            "{account:?}: {first}"
        );
        assert!(matches!(guide.get(1), Some(Step::Open(page)) if page.starts_with("https://")));
    }
    let last = |account: Account| match account.guide().pop() {
        Some(Step::Do(text)) => text,
        other => panic!("{other:?}"),
    };
    assert!(last(Account::HuggingFace).contains("gated dataset also needs its terms accepted"));
    assert!(last(Account::Kaggle).contains("competition also needs its rules accepted"));
}
