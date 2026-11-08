//! signing in: one key, saved where the source's own tools read it, asked for only at a terminal.
#![cfg(debug_assertions)]

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

use crate::support::{Result, Sandbox, Server, json, reply, text};

/// runs `command` with `input` on stdin.
fn with_stdin(mut command: Command, input: &str) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(input.as_bytes())?;
    Ok(child.wait_with_output()?)
}

fn home(sandbox: &Sandbox) -> std::path::PathBuf {
    sandbox
        .work()
        .parent()
        .map(|root| root.join("home"))
        .unwrap_or_default()
}

#[test]
fn nobody_is_signed_in_on_a_fresh_machine() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["auth", "--json", "--offline"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["command"], "auth");
    let accounts = document["result"]["accounts"]
        .as_array()
        .ok_or("no accounts")?;
    assert!(accounts.iter().any(|a| a["account"] == "hf"));
    assert!(
        accounts
            .iter()
            .any(|a| a["account"] == "login:urs.earthdata.nasa.gov")
    );
    assert!(accounts.iter().all(|a| a["signed_in"] == false));
    Ok(())
}

#[test]
fn auth_status_lists_every_account_like_plain_auth() -> Result {
    let sandbox = Sandbox::new()?;
    let plain = sandbox.run(&["auth", "--offline"])?;
    let status = sandbox.run(&["auth", "status", "--offline"])?;
    assert!(status.status.success(), "{}", text(&status.stderr)?);
    let story = text(&status.stderr)?;
    assert_eq!(story, text(&plain.stderr)?);
    assert!(
        story
            .lines()
            .any(|line| line.starts_with(" Signed out hugging face, kaggle, zenodo")),
        "{story}"
    );
    Ok(())
}

#[test]
fn logout_names_another_configured_key_that_remains() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = home(&sandbox).join(".kaggle");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("access_token"), "KGAT_current\n")?;
    std::fs::write(
        folder.join("kaggle.json"),
        r#"{"username":"ada","key":"api-key"}"#,
    )?;
    let first = sandbox.run(&["auth", "logout", "kaggle", "--yes"])?;
    let stderr = text(&first.stderr)?;
    assert!(first.status.success(), "{stderr}");
    assert!(stderr.contains("is still read from"), "{stderr}");
    assert!(!folder.join("access_token").exists());
    assert!(folder.join("kaggle.json").exists());
    let second = sandbox.run(&["auth", "logout", "kaggle", "--yes"])?;
    assert!(second.status.success(), "{}", text(&second.stderr)?);
    assert!(!text(&second.stderr)?.contains("is still read from"));
    assert!(!folder.join("kaggle.json").exists());
    Ok(())
}

#[test]
fn a_hugging_face_key_goes_where_the_hf_tools_read_it_and_back_out() -> Result {
    let sandbox = Sandbox::new()?;
    let login = with_stdin(
        sandbox.command(&["auth", "login", "hf", "--with-token", "--offline", "--json"]),
        "hf_example\n",
    )?;
    assert!(login.status.success(), "{}", text(&login.stderr)?);
    let token = home(&sandbox)
        .join(".cache")
        .join("huggingface")
        .join("token");
    assert_eq!(std::fs::read_to_string(&token)?, "hf_example\n");
    let document = json(&login)?;
    assert_eq!(document["result"]["account"], "hf");
    assert_eq!(document["result"]["saved_to"], token.display().to_string());
    assert!(
        !text(&login.stdout)?.contains("hf_example"),
        "the key is never printed"
    );

    let status = json(&sandbox.run(&["auth", "--json", "--offline"])?)?;
    let hf = status["result"]["accounts"]
        .as_array()
        .and_then(|accounts| accounts.iter().find(|a| a["account"] == "hf"))
        .ok_or("no hf")?;
    assert_eq!(hf["signed_in"], true);
    assert_eq!(hf["from"], token.display().to_string());

    // the hf tools read the same file, so nobody at a terminal means it stays without --yes
    let refused = sandbox.run(&["auth", "logout", "hf", "--json"])?;
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr)?);
    assert_eq!(
        json(&refused)?["errors"][0]["fix"],
        "fetchloom auth logout hf --yes"
    );
    assert!(token.exists());
    let logout = sandbox.run(&["auth", "logout", "hf", "--json", "--yes"])?;
    assert!(logout.status.success(), "{}", text(&logout.stderr)?);
    assert!(!token.exists());
    let again = with_stdin(
        sandbox.command(&["auth", "login", "hf", "--with-token", "--offline"]),
        "hf_example\n",
    )?;
    assert!(again.status.success());
    let told = text(&again.stderr)?;
    assert!(told.contains("Signed in to hugging face"), "{told}");
    assert!(
        told.contains(&format!("saved to {}", token.display())),
        "{told}"
    );
    assert!(!told.contains("hf_example"), "{told}");
    let quiet_logout = sandbox.run(&["auth", "logout", "hf", "--yes"])?;
    assert!(
        text(&quiet_logout.stderr)?.contains("Signed out of hugging face"),
        "{}",
        text(&quiet_logout.stderr)?
    );
    Ok(())
}

#[test]
fn a_key_without_a_home_of_its_own_needs_the_store_or_a_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let login = with_stdin(
        sandbox.command(&["auth", "login", "zenodo", "--with-token", "--offline"]),
        "zenodo-key",
    )?;
    assert!(login.status.success(), "{}", text(&login.stderr)?);
    let stderr = text(&login.stderr)?;
    assert!(stderr.contains("no credential store"), "{stderr}");
    assert!(!stderr.contains("zenodo-key"));
    let status = json(&sandbox.run(&["auth", "--json", "--offline"])?)?;
    let zenodo = status["result"]["accounts"]
        .as_array()
        .and_then(|accounts| accounts.iter().find(|a| a["account"] == "zenodo"))
        .ok_or("no zenodo")?;
    assert_eq!(zenodo["signed_in"], true);
    let logout = sandbox.run(&["auth", "logout", "zenodo"])?;
    assert!(logout.status.success(), "{}", text(&logout.stderr)?);
    let status = json(&sandbox.run(&["auth", "--json", "--offline"])?)?;
    assert!(
        status["result"]["accounts"]
            .as_array()
            .is_some_and(|accounts| accounts.iter().all(|a| a["signed_in"] == false))
    );
    Ok(())
}

#[test]
fn a_login_lands_in_netrc_for_curl_and_wget_too() -> Result {
    let sandbox = Sandbox::new()?;
    let netrc = home(&sandbox).join(".netrc");
    std::fs::create_dir_all(home(&sandbox))?;
    std::fs::write(&netrc, "machine other.example login x password y\n")?;
    let login = with_stdin(
        sandbox.command(&["auth", "login", "earthdata", "--with-token", "--offline"]),
        "ada:s3cret pass",
    )?;
    assert!(login.status.success(), "{}", text(&login.stderr)?);
    assert_eq!(
        std::fs::read_to_string(&netrc)?,
        "machine other.example login x password y\n\
         machine urs.earthdata.nasa.gov login ada password \"s3cret pass\"\n"
    );
    // a password keeps the spaces around it; only the line ending a pipe adds goes
    let spaced = with_stdin(
        sandbox.command(&["auth", "login", "physionet", "--with-token", "--offline"]),
        " grace : pass \r\n",
    )?;
    assert!(spaced.status.success(), "{}", text(&spaced.stderr)?);
    assert!(
        std::fs::read_to_string(&netrc)?
            .contains("machine physionet.org login grace password \" pass \"")
    );
    let bad = with_stdin(
        sandbox.command(&["auth", "login", "physionet", "--with-token", "--json"]),
        "no-colon",
    )?;
    assert_eq!(bad.status.code(), Some(2));
    assert_eq!(json(&bad)?["errors"][0]["kind"], "usage");
    Ok(())
}

#[test]
fn signing_in_without_a_terminal_names_the_scripted_way() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["auth", "login", "hf", "--json"])?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert!(
        document["errors"][0]["help"]
            .as_str()
            .is_some_and(|help| help.contains("fetchloom auth login hf --with-token"))
    );
    let unknown = json(&sandbox.run(&["auth", "login", "github", "--json"])?)?;
    assert!(
        unknown["errors"][0]["help"]
            .as_str()
            .is_some_and(|help| help.contains("hf, kaggle"))
    );
    Ok(())
}

#[test]
fn a_key_the_source_refuses_is_not_saved() -> Result {
    let sandbox = Sandbox::new()?;
    let refusing = Server::start("/", vec![reply("401 Unauthorized", &[], b"{}")])?;
    let mut command = sandbox.command(&["auth", "login", "hf", "--with-token", "--json"]);
    command.env("FETCHLOOM_TEST_HF_API", &refusing.url);
    let output = with_stdin(command, "hf_wrong")?;
    assert!(!output.status.success());
    assert_eq!(json(&output)?["errors"][0]["kind"], "auth.required");
    assert!(!home(&sandbox).join(".cache/huggingface/token").exists());

    let accepting = Server::start(
        "/",
        vec![reply(
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"name":"ada"}"#,
        )],
    )?;
    let mut command = sandbox.command(&["auth", "login", "hf", "--with-token", "--json"]);
    command.env("FETCHLOOM_TEST_HF_API", &accepting.url);
    let output = with_stdin(command, "hf_right")?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["result"]["user"], "ada");
    assert!(
        accepting
            .requests()
            .iter()
            .any(|line| line.contains("/api/whoami-v2"))
    );
    Ok(())
}

/// q typed to quit at the question whether to save a key is no answer: the question asks again,
/// and the key is never saved as if it were a yes, though enter alone means yes there.
#[cfg(target_os = "linux")]
#[test]
fn q_at_the_question_to_save_a_key_asks_again_and_saves_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let login = sandbox.command(&["auth", "login", "hf", "--offline"]);
    let Some((_, screen)) = crate::support::typed(
        &sandbox.work(),
        &login,
        "",
        "        key ",
        "hf_test1234567890\rq\rn\r",
    )?
    else {
        return Ok(());
    };
    assert!(
        !home(&sandbox).join(".cache/huggingface/token").exists(),
        "{screen}"
    );
    assert!(!screen.contains("saved to"), "{screen}");
    assert!(screen.contains("? [Y/n] q\n"), "{screen}");
    assert!(
        screen.contains("type y or n, or press ctrl-c to cancel\n"),
        "{screen}"
    );
    Ok(())
}
