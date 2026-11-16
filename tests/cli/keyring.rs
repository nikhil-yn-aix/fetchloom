//! the system keyring for real: a session bus and a gnome keyring of the test's own, unlocked with
//! a password, where fetchloom keeps keys with `FETCHLOOM_KEYRING` on and finds chrome's key.
#![cfg(all(target_os = "linux", debug_assertions))]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::sites::{chrome_cookies_sealed, heard, host_of};
use crate::support::{Fetchloom, Result, Routes, Sandbox, file, json, skipped, text};

/// the password that unlocks the test's keyring.
const PASSWORD: &str = "keyring-password-of-the-test";
/// the secret chrome keeps in the keyring, and the key PBKDF2-HMAC-SHA1 derives from it over
/// "saltysalt" once, as python's `hashlib.pbkdf2_hmac` gives it.
const CHROME_SECRET: &str = "chrome-keyring-secret-of-the-test";
const CHROME_KEY: [u8; 16] = [
    0xa2, 0xb2, 0x03, 0xe9, 0x58, 0x8c, 0x5f, 0xf6, 0xdf, 0xd7, 0x66, 0xdf, 0xbc, 0xac, 0x5c, 0xf8,
];
/// where gnome keyring keeps the login keyring on its bus.
const LOGIN: &str = "/org/freedesktop/secrets/collection/login";
/// the attributes the keyring keeps fetchloom's zenodo key under.
const ZENODO: [&str; 4] = ["service", "fetchloom", "username", "zenodo"];

/// a session bus of the test's own, with gnome keyring answering on it as the secret service or
/// with nothing, so the bus is a machine without a keyring. both end when it goes.
struct Keyring {
    address: String,
    folder: PathBuf,
    home: PathBuf,
    bus: Child,
    daemon: Option<Child>,
}

impl Keyring {
    /// a bus with gnome keyring on it when `with_keyring`, in the sandbox, its keyrings in the
    /// sandbox's home. none, said as a skip, where the programs are missing.
    fn start(sandbox: &Sandbox, with_keyring: bool) -> Result<Option<Keyring>> {
        let missing = [
            "dbus-daemon",
            "dbus-send",
            "gnome-keyring-daemon",
            "secret-tool",
        ]
        .into_iter()
        .find(|program| !on_path(program));
        if let Some(program) = missing {
            skipped(
                "secret-service",
                &format!("no {program}, so no keyring of the test's own"),
            )?;
            return Ok(None);
        }
        let folder = sandbox.folder("bus")?;
        let home = sandbox.work().parent().ok_or("no sandbox")?.join("home");
        let config = folder.join("bus.conf");
        // no service folders: the bus starts no keyring of its own, as a desktop's would
        std::fs::write(
            &config,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig>\n  <type>session</type>\n  <listen>unix:path={}</listen>\n  \
                 <policy context=\"default\">\n    <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    \
                 <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
                folder.join("socket").display()
            ),
        )?;
        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let said = bus.stdout.take().ok_or("dbus-daemon has no stdout")?;
        // the bus prints its address once it listens
        let address = within("the bus to listen", move || {
            let mut line = String::new();
            BufReader::new(said).read_line(&mut line)?;
            Ok(line.trim().to_owned())
        });
        let address = match address {
            Ok(address) if !address.is_empty() => address,
            failed => {
                let _ = bus.kill();
                let _ = bus.wait();
                return Err(format!("the bus never listened: {failed:?}").into());
            }
        };
        let mut keyring = Keyring {
            address,
            folder,
            home,
            bus,
            daemon: None,
        };
        if with_keyring {
            keyring.unlock()?;
        }
        Ok(Some(keyring))
    }

    /// starts gnome keyring, which unlocks the login keyring with the password, or makes it.
    fn unlock(&mut self) -> Result {
        let runtime = self.folder.join("runtime");
        std::fs::create_dir_all(&runtime)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.folder.join("keyring.log"))?;
        let mut daemon = self
            .tool("gnome-keyring-daemon")
            .args(["--foreground", "--components=secrets", "--unlock"])
            .arg(format!("--control-directory={}", runtime.display()))
            .env("XDG_RUNTIME_DIR", &runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()?;
        let typed = daemon.stdin.take().map(|mut stdin| {
            let typed = stdin.write_all(PASSWORD.as_bytes());
            drop(stdin);
            typed
        });
        let said = daemon.stdout.take().ok_or("gnome keyring has no stdout")?;
        self.daemon = Some(daemon);
        typed.ok_or("gnome keyring has no stdin")??;
        // it closes its stdout once it is set up and answers on the bus
        within("gnome keyring to answer", move || {
            let mut said = said;
            let mut text = String::new();
            said.read_to_string(&mut text)?;
            Ok(text)
        })?;
        Ok(())
    }

    /// ends gnome keyring, as a machine's session ends, leaving the keyring on disk.
    fn stop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    /// `program` with nothing of the machine's but `PATH`, on this bus, in the sandbox's home.
    fn tool(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("LC_ALL", "C.UTF-8")
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address);
        command
    }

    /// the secret the keyring keeps under `attributes`, as `secret-tool lookup` finds it; none
    /// when there is none, or the keyring is locked.
    fn lookup(&self, attributes: &[&str]) -> Result<Option<String>> {
        let found = self
            .tool("secret-tool")
            .arg("lookup")
            .args(attributes)
            .stdin(Stdio::null())
            .output()?;
        Ok(found
            .status
            .success()
            .then(|| String::from_utf8_lossy(&found.stdout).into_owned()))
    }

    /// keeps `secret` under `label` and `attributes`, as a browser does.
    fn store(&self, label: &str, attributes: &[&str], secret: &str) -> Result {
        let mut store = self
            .tool("secret-tool")
            .arg("store")
            .arg(format!("--label={label}"))
            .args(attributes)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = store.stdin.take() {
            stdin.write_all(secret.as_bytes())?;
        }
        let stored = store.wait_with_output()?;
        if !stored.status.success() {
            return Err(format!("secret-tool store failed: {}", text(&stored.stderr)?).into());
        }
        Ok(())
    }

    /// locks the login keyring, as a session that locks its screen may. with no screen to ask
    /// for the password on, gnome keyring then answers each use of it as dismissed.
    fn lock(&self) -> Result {
        let locked = self
            .tool("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.secrets",
                "/org/freedesktop/secrets",
                "org.freedesktop.Secret.Service.Lock",
                &format!("array:objpath:{LOGIN}"),
            ])
            .stdin(Stdio::null())
            .output()?;
        if !locked.status.success() || !text(&locked.stdout)?.contains(LOGIN) {
            return Err(format!("the keyring did not lock: {}", text(&locked.stderr)?).into());
        }
        Ok(())
    }

    /// fetchloom with `args`, the keyring on, on this bus.
    fn command(&self, sandbox: &Sandbox, args: &[&str]) -> Fetchloom {
        let mut command = sandbox.command(args);
        command
            .env("FETCHLOOM_KEYRING", "1")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address);
        command
    }
}

impl Drop for Keyring {
    fn drop(&mut self) {
        self.stop();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

/// whether a program named `name` is on `PATH`.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|folder| folder.join(name).is_file()))
}

/// what `read` gives, failing past a minute, which only keeps a broken setup from hanging.
fn within<T: Send + 'static>(
    what: &str,
    read: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T> {
    let (sent, received) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(read());
    });
    match received.recv_timeout(Duration::from_secs(60)) {
        Ok(read) => Ok(read?),
        Err(_) => Err(format!("timed out waiting for {what}").into()),
    }
}

/// fetchloom's own settings folder in the sandbox's home.
fn settings(sandbox: &Sandbox) -> Result<PathBuf> {
    Ok(sandbox
        .work()
        .parent()
        .ok_or("no sandbox")?
        .join("home/.config/fetchloom"))
}

/// what fetchloom's index says of where each key went.
fn index(sandbox: &Sandbox) -> Result<serde_json::Value> {
    let path = settings(sandbox)?.join("accounts.json");
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

/// a source's key and a website's token saved with the keyring on go into it alone, come back
/// from it for `auth` and for the website's download, and leave it with `auth logout`.
#[test]
fn keys_kept_in_the_keyring_are_read_from_it_and_removed_from_it() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(keyring) = Keyring::start(&sandbox, true)? else {
        return Ok(());
    };
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    for (args, typed) in [
        (vec!["auth", "login", "zenodo"], "z-key-of-the-test\n"),
        (
            vec!["auth", "login", &host, "--token"],
            "t-key-of-the-test\n",
        ),
    ] {
        let mut args = args;
        args.extend(["--with-token", "--offline"]);
        let login = keyring
            .command(&sandbox, &args)
            .output_with(Some(typed.as_bytes()))?;
        let stderr = text(&login.stderr)?;
        assert_eq!(login.status.code(), Some(0), "{stderr}");
        assert!(
            stderr.contains("· saved to the system keyring\n"),
            "{stderr}"
        );
    }
    let site_key = ["service", "fetchloom", "username", &format!("site:{host}")];
    assert_eq!(
        keyring.lookup(&ZENODO)?.as_deref(),
        Some("z-key-of-the-test")
    );
    assert_eq!(
        keyring.lookup(&site_key)?.as_deref(),
        Some(r#"{"kind":"token","token":"t-key-of-the-test"}"#)
    );
    assert_eq!(
        index(&sandbox)?,
        serde_json::json!({"zenodo": "keyring", format!("site:{host}"): "keyring"})
    );
    assert!(!settings(&sandbox)?.join("credentials.json").exists());
    let listed = keyring.command(&sandbox, &["auth", "--offline"]).output()?;
    let stderr = text(&listed.stderr)?;
    assert_eq!(listed.status.code(), Some(0), "{stderr}");
    for told in [
        "Signed in to zenodo · the system keyring".to_owned(),
        format!("Signed in to {host} with a token · the system keyring"),
    ] {
        assert!(stderr.contains(&told), "{told}: {stderr}");
    }
    let output = keyring
        .command(&sandbox, &["get", &format!("{}/data.csv", site.url)])
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        heard(&site, "authorization"),
        [Some("bearer t-key-of-the-test".to_owned())]
    );
    for (account, said) in [
        (
            "zenodo",
            "Signed out of zenodo · removed from the system keyring".to_owned(),
        ),
        (
            host.as_str(),
            format!("Signed out of {host} · removed from the system keyring"),
        ),
    ] {
        let logout = keyring
            .command(&sandbox, &["auth", "logout", account, "--yes"])
            .output()?;
        let stderr = text(&logout.stderr)?;
        assert_eq!(logout.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains(&said), "{said}: {stderr}");
    }
    assert_eq!(keyring.lookup(&ZENODO)?, None);
    assert_eq!(keyring.lookup(&site_key)?, None);
    assert_eq!(index(&sandbox)?, serde_json::json!({}));
    Ok(())
}

/// with `FETCHLOOM_KEYRING` off, keys the index lists in the keyring are never read or removed,
/// though the keyring answers on the bus: `auth` and a download leave them out and say why,
/// signing out of one is refused, and signing in again keeps the new key in fetchloom's file
/// while the old one stays in the keyring.
#[test]
fn with_the_keyring_off_its_keys_stay_there_untouched() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(keyring) = Keyring::start(&sandbox, true)? else {
        return Ok(());
    };
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    for (args, typed) in [
        (vec!["auth", "login", "zenodo"], "z-key-of-the-test\n"),
        (
            vec!["auth", "login", &host, "--token"],
            "t-key-of-the-test\n",
        ),
    ] {
        let mut args = args;
        args.extend(["--with-token", "--offline"]);
        let login = keyring
            .command(&sandbox, &args)
            .output_with(Some(typed.as_bytes()))?;
        assert_eq!(login.status.code(), Some(0), "{}", text(&login.stderr)?);
    }
    // the sandbox's own FETCHLOOM_KEYRING, 0, on the keyring's bus
    let off = |args: &[&str]| {
        let mut command = sandbox.command(args);
        command.env("DBUS_SESSION_BUS_ADDRESS", &keyring.address);
        command
    };
    let hint = format!(
        "hint: FETCHLOOM_KEYRING is off, so fetchloom leaves out the keys for {host} and zenodo in the system keyring; unset FETCHLOOM_KEYRING to use them"
    );
    let listed = off(&["auth", "--offline"]).output()?;
    let stderr = text(&listed.stderr)?;
    assert_eq!(listed.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&hint), "{stderr}");
    for unread in [
        "Signed in to zenodo".to_owned(),
        format!("Signed in to {host}"),
    ] {
        assert!(!stderr.contains(&unread), "{unread}: {stderr}");
    }
    let output = off(&["get", &format!("{}/data.csv", site.url)]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&hint), "{stderr}");
    assert_eq!(heard(&site, "authorization"), [None]);
    let logout = off(&["--json", "auth", "logout", "zenodo", "--yes"]).output()?;
    let error = &json(&logout)?["errors"][0];
    assert_eq!(logout.status.code(), Some(2), "{}", text(&logout.stdout)?);
    assert_eq!(
        error["message"],
        "the zenodo key is in the system keyring, which fetchloom leaves alone while FETCHLOOM_KEYRING is off"
    );
    assert_eq!(
        error["help"],
        "unset FETCHLOOM_KEYRING, then run `fetchloom auth logout zenodo` again"
    );
    let again = off(&["auth", "login", "zenodo", "--with-token", "--offline"])
        .output_with(Some(b"z-new-key\n"))?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "hint: the zenodo key saved before stays in the system keyring, since FETCHLOOM_KEYRING is off"
        ),
        "{stderr}"
    );
    assert_eq!(
        index(&sandbox)?,
        serde_json::json!({"zenodo": "file", format!("site:{host}"): "keyring"})
    );
    assert_eq!(
        keyring.lookup(&ZENODO)?.as_deref(),
        Some("z-key-of-the-test")
    );
    Ok(())
}

/// a site's key saved again with `FETCHLOOM_KEYRING` off leaves its keyring copy behind, which the
/// hint names once; signing out with the keyring on later removes that copy too, rather than
/// saying nothing was saved while the keyring still holds it.
#[test]
fn a_keyring_copy_a_sign_in_with_the_keyring_off_left_is_signed_out_of() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(keyring) = Keyring::start(&sandbox, true)? else {
        return Ok(());
    };
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    let site_key = ["service", "fetchloom", "username", &format!("site:{host}")];
    let login = [
        "auth",
        "login",
        &host,
        "--token",
        "--with-token",
        "--offline",
    ];
    let on = keyring
        .command(&sandbox, &login)
        .output_with(Some(b"t-kept\n"))?;
    assert_eq!(on.status.code(), Some(0), "{}", text(&on.stderr)?);
    let off = |args: &[&str]| {
        let mut command = sandbox.command(args);
        command.env("DBUS_SESSION_BUS_ADDRESS", &keyring.address);
        command
    };
    let again = off(&login).output_with(Some(b"t-filed\n"))?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "hint: what was saved for {host} before stays in the system keyring, since FETCHLOOM_KEYRING is off"
        )),
        "{stderr}"
    );
    let filed = off(&["auth", "logout", &host, "--yes"]).output()?;
    assert_eq!(filed.status.code(), Some(0), "{}", text(&filed.stderr)?);
    assert_eq!(
        keyring.lookup(&site_key)?.as_deref(),
        Some("{\"kind\":\"token\",\"token\":\"t-kept\"}")
    );
    let logout = keyring
        .command(&sandbox, &["auth", "logout", &host, "--yes"])
        .output()?;
    let stderr = text(&logout.stderr)?;
    assert_eq!(logout.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "Signed out of {host} · removed from the system keyring"
        )),
        "{stderr}"
    );
    assert_eq!(keyring.lookup(&site_key)?, None);
    Ok(())
}

/// a locked keyring, which gnome keyring can only ask its password for on a screen, keeps the
/// key: signing out says it could not remove it, rather than that none was saved, and signing in
/// again says how to keep the key elsewhere. unlocked, the key is still there.
#[test]
fn a_locked_keyring_keeps_its_key_and_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(mut keyring) = Keyring::start(&sandbox, true)? else {
        return Ok(());
    };
    let login = keyring
        .command(
            &sandbox,
            &["auth", "login", "zenodo", "--with-token", "--offline"],
        )
        .output_with(Some(b"z-key-of-the-test\n"))?;
    assert_eq!(login.status.code(), Some(0), "{}", text(&login.stderr)?);
    keyring.lock()?;
    let logout = keyring
        .command(&sandbox, &["--json", "auth", "logout", "zenodo", "--yes"])
        .output()?;
    let error = &json(&logout)?["errors"][0];
    assert_eq!(logout.status.code(), Some(50), "{}", text(&logout.stdout)?);
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with("could not remove the zenodo key: "),
        "{message}"
    );
    assert_eq!(
        error["help"],
        "unlock the system keyring, then run `fetchloom auth logout zenodo` again"
    );
    assert_eq!(index(&sandbox)?, serde_json::json!({"zenodo": "keyring"}));
    let again = keyring
        .command(
            &sandbox,
            &[
                "--json",
                "auth",
                "login",
                "zenodo",
                "--with-token",
                "--offline",
            ],
        )
        .output_with(Some(b"z-new-key\n"))?;
    let error = &json(&again)?["errors"][0];
    assert_eq!(again.status.code(), Some(50), "{}", text(&again.stdout)?);
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with("could not save the zenodo key: "),
        "{message}"
    );
    assert_eq!(
        error["help"],
        "unlock the system keyring, or set FETCHLOOM_KEYRING=false to keep the key in fetchloom's own file, readable only by you, then run it again"
    );
    for output in [&logout, &again] {
        let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
        assert!(
            !said.contains("z-key-of-the-test") && !said.contains("z-new-key"),
            "{said}"
        );
    }
    keyring.stop();
    keyring.unlock()?;
    assert_eq!(
        keyring.lookup(&ZENODO)?.as_deref(),
        Some("z-key-of-the-test")
    );
    Ok(())
}

/// a bus with no keyring on it, as a server's, is a machine without a credential store: the key
/// goes to fetchloom's own file, said so, and the run waits on nothing.
#[test]
fn without_a_keyring_on_the_bus_a_key_goes_to_fetchlooms_file() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(keyring) = Keyring::start(&sandbox, false)? else {
        return Ok(());
    };
    let login = keyring
        .command(
            &sandbox,
            &["auth", "login", "zenodo", "--with-token", "--offline"],
        )
        .output_with(Some(b"z-key-of-the-test\n"))?;
    let stderr = text(&login.stderr)?;
    assert_eq!(login.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "warning: this machine has no credential store, so the key is saved in ~/.config/fetchloom/credentials.json, readable only by you"
        ),
        "{stderr}"
    );
    assert_eq!(index(&sandbox)?, serde_json::json!({"zenodo": "file"}));
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(
        settings(&sandbox)?.join("credentials.json"),
    )?)?;
    assert_eq!(saved, serde_json::json!({"zenodo": "z-key-of-the-test"}));
    Ok(())
}

/// chrome on linux seals cookies under `v11` with a key derived from the secret it keeps in the
/// keyring: without the keyring none opens, and with it the cookie opens, goes into the keyring
/// with the sign-in and goes to its host.
#[test]
fn chromes_secret_in_the_keyring_opens_its_v11_cookies() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(keyring) = Keyring::start(&sandbox, true)? else {
        return Ok(());
    };
    keyring.store(
        "Chrome Safe Storage",
        &["application", "chrome"],
        CHROME_SECRET,
    )?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    let profile = Path::new(&sandbox.work())
        .parent()
        .ok_or("no sandbox")?
        .join("home/.config/google-chrome/Default");
    chrome_cookies_sealed(
        &profile,
        &[("127.0.0.1", "sid", "v11-value", true)],
        "v11",
        &CHROME_KEY,
    )?;
    let login = |command: &mut Fetchloom| command.output();
    let unopened = login(&mut sandbox.command(&[
        "--json",
        "auth",
        "login",
        &host,
        "--browser",
        "chrome",
        "--yes",
    ]))?;
    assert_eq!(
        unopened.status.code(),
        Some(40),
        "{}",
        text(&unopened.stdout)?
    );
    let opened = login(&mut keyring.command(
        &sandbox,
        &["auth", "login", &host, "--browser", "chrome", "--yes"],
    ))?;
    let stderr = text(&opened.stderr)?;
    assert_eq!(opened.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "Signed in to {host} with 1 cookie from chrome · saved to the system keyring"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("v11-value"), "{stderr}");
    let saved = keyring
        .lookup(&["service", "fetchloom", "username", &format!("site:{host}")])?
        .ok_or("the cookies are not in the keyring")?;
    assert!(saved.contains("v11-value"), "{saved}");
    let output = keyring
        .command(&sandbox, &["get", &format!("{}/data.csv", site.url)])
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(heard(&site, "cookie"), [Some("sid=v11-value".to_owned())]);
    Ok(())
}
