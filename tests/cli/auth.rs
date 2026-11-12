//! signing in: one key, saved where the source's own tools read it, asked for only at a terminal.
#![cfg(debug_assertions)]

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

use crate::support::{Result, Routes, Sandbox, Server, json, reply, text, workspace};

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

/// plain `auth` lists every account. `auth status`, the spelling other tools use, is no command,
/// and its help names the one that is.
#[test]
fn auth_alone_lists_every_account() -> Result {
    let sandbox = Sandbox::new()?;
    let plain = sandbox.run(&["auth", "--offline"])?;
    assert!(plain.status.success(), "{}", text(&plain.stderr)?);
    let story = text(&plain.stderr)?;
    assert!(
        story.lines().any(|line| line.starts_with(
            "  Signed in to none of hf, kaggle, zenodo, figshare, osf, earthdata, physionet"
        )),
        "{story}"
    );
    let status = sandbox.run(&["--json", "auth", "status", "--offline"])?;
    assert_eq!(status.status.code(), Some(2), "{}", text(&status.stdout)?);
    let error = &json(&status)?["errors"][0];
    assert_eq!(error["kind"], "usage");
    assert_eq!(error["fix"], "fetchloom auth");
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

/// a hugging face key goes to the file the hf tools read and back out of it, and the lines that
/// name the file show it below the home folder as `~/…`, while `--json` gives it whole.
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
    let shown = std::path::Path::new("~")
        .join(".cache")
        .join("huggingface")
        .join("token");
    let told = text(&again.stderr)?;
    assert!(
        told.contains(&format!("Signed in to hf · saved to {}\n", shown.display())),
        "{told}"
    );
    assert!(!told.contains("hf_example"), "{told}");
    let status = text(&sandbox.run(&["auth", "--offline"])?.stderr)?;
    assert!(
        status.contains(&format!("Signed in to hf · {}\n", shown.display())),
        "{status}"
    );
    let quiet_logout = sandbox.run(&["auth", "logout", "hf", "--yes"])?;
    assert!(
        text(&quiet_logout.stderr)?.contains(&format!(
            "Signed out of hf · removed from {}\n",
            shown.display()
        )),
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

/// `--yes` answers the questions signing in asks, as it answers every other question, so a key
/// typed once is saved without another question.
#[cfg(target_os = "linux")]
#[test]
fn yes_answers_the_question_to_save_a_key() -> Result {
    let sandbox = Sandbox::new()?;
    let login = sandbox.command(&["auth", "login", "hf", "--offline", "--yes"]);
    let Some((code, screen)) = crate::support::typed(
        &sandbox.work(),
        &login,
        "",
        "        key ",
        "hf_test1234567890\r",
    )?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{screen}");
    assert!(
        home(&sandbox).join(".cache/huggingface/token").exists(),
        "{screen}"
    );
    assert!(!screen.contains("[Y/n]"), "{screen}");
    Ok(())
}

/// a no while signing in, to saving the key or to replacing one, and while signing out, to
/// removing a key other tools read, changes nothing and ends as a cancel: one line says what was
/// cancelled, and the exit code is 1, as for any no.
#[cfg(target_os = "linux")]
#[test]
fn a_no_while_signing_in_or_out_is_a_cancel_that_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let token = home(&sandbox).join(".cache/huggingface/token");
    let login = sandbox.command(&["auth", "login", "hf", "--offline"]);
    let signing = "  Cancelled signing in to hugging face, so nothing changed\n";
    let Some((code, screen)) = crate::support::typed(
        &sandbox.work(),
        &login,
        "",
        "        key ",
        "hf_test1234567890\rn\r",
    )?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(1), "{screen}");
    assert!(screen.contains(signing), "{screen}");
    assert!(
        screen.contains("save it to ~/.cache/huggingface/token? [Y/n]"),
        "{screen}"
    );
    assert!(!token.exists(), "{screen}");
    std::fs::create_dir_all(token.parent().ok_or("no folder")?)?;
    std::fs::write(&token, "hf_old\n")?;
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &login, "", "replace it?", "n\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(1), "{screen}");
    assert!(screen.contains(signing), "{screen}");
    let logout = sandbox.command(&["auth", "logout", "hf"]);
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &logout, "", "remove it from", "n\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(1), "{screen}");
    assert!(
        screen.contains("remove it from ~/.cache/huggingface/token? [y/N]"),
        "{screen}"
    );
    assert!(
        screen.contains("  Cancelled removing the hugging face key, so nothing changed\n"),
        "{screen}"
    );
    assert_eq!(std::fs::read_to_string(&token)?, "hf_old\n");
    Ok(())
}

/// what the hub asks for a dataset's revision, as fetchloom asks, and for a model's.
const EXPAND: &str = "expand=sha&expand=gated&expand=private&expand=tags&expand=cardData";
const MODEL_EXPAND: &str = "expand=sha&expand=gated&expand=private&expand=tags";

/// the hub on this machine: its answer to a name nobody may see, as a typo gets, to a gated
/// dataset before its terms are accepted, to a model link whose name a dataset has, and to any
/// key a sign-in checks, as ada's.
fn hub() -> Result<Routes> {
    let answer = |status: &str, fixture: &str| -> Result<_> {
        let path = workspace().join("tests/fixtures/sources/hf").join(fixture);
        let body = std::fs::read(path)?;
        let length = body.len().to_string();
        let head = [
            ("Content-Type", "application/json"),
            ("Content-Length", length.as_str()),
        ];
        Ok(reply(status, &head, &body))
    };
    Routes::start(vec![
        (
            format!("/api/datasets/scikit-learn/irs/revision/main?{EXPAND}"),
            answer("401 Unauthorized", "hidden.json")?,
        ),
        (
            format!("/api/datasets/bigcode/the-stack/revision/main?{EXPAND}"),
            answer("200 OK", "stack-revision.json")?,
        ),
        (
            "/api/datasets/bigcode/the-stack/auth-check".to_owned(),
            answer("401 Unauthorized", "stack-auth-check.json")?,
        ),
        (
            format!("/api/models/scikit-learn/iris/revision/main?{MODEL_EXPAND}"),
            answer("401 Unauthorized", "hidden.json")?,
        ),
        (
            format!("/api/models/scikit-learn/irs/revision/main?{MODEL_EXPAND}"),
            answer("401 Unauthorized", "hidden.json")?,
        ),
        (
            "/api/datasets/scikit-learn/iris".to_owned(),
            answer("200 OK", "imdb-revision.json")?,
        ),
        (
            "/api/whoami-v2".to_owned(),
            reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                br#"{"name":"ada"}"#,
            ),
        ),
    ])
}

/// at a terminal, a refusal that signing in may lift asks before any error: a line names what
/// was refused, then what else may be wrong comes first: a name with a typo, which the hub
/// answers as it answers a private one, with the page to check it at, or terms a gated dataset
/// needs accepted on its page. enter alone is a no, so a reflex starts no sign-in, and the no
/// ends the run with one line and the refusal's exit code.
#[cfg(target_os = "linux")]
#[test]
fn the_offer_to_sign_in_comes_before_any_error_and_defaults_to_no() -> Result {
    let sandbox = Sandbox::new()?;
    let hub = hub()?;
    for (reference, refused, first) in [
        (
            "hf:datasets/scikit-learn/irs",
            "huggingface.co has no dataset scikit-learn/irs, or it is private",
            "Check the name first at https://huggingface.co/datasets/scikit-learn/irs\n            hugging face answers alike for a name that does not exist and for a private one\n",
        ),
        (
            "hf:datasets/bigcode/the-stack",
            "hf:datasets/bigcode/the-stack requires permission to download its files",
            "First accept the terms at https://huggingface.co/datasets/bigcode/the-stack",
        ),
    ] {
        let mut add = sandbox.command(&["add", reference]);
        add.env("FETCHLOOM_TEST_HF_API", &hub.url);
        let Some((code, screen)) = crate::support::typed(
            &sandbox.work(),
            &add,
            "",
            "sign in to hugging face now?",
            "\r",
        )?
        else {
            return Ok(());
        };
        assert_eq!(code, Some(40), "{screen}");
        assert!(!screen.contains("error:"), "{screen}");
        // a line told at a terminal starts with a return, which the lines below leave out
        let screen = screen.replace('\r', "");
        let lead = format!("    Refused {refused}\n");
        let told = [lead.as_str(), first, "sign in to hugging face now? [y/N]"]
            .map(|text| screen.find(text));
        assert!(
            told.windows(2)
                .all(|pair| pair[0].zip(pair[1]).is_some_and(|(one, next)| one < next)),
            "{screen}"
        );
        assert!(
            screen
                .trim_end()
                .ends_with("  Cancelled signing in to hugging face"),
            "{screen}"
        );
        assert!(!screen.contains("        key "), "no sign-in: {screen}");
    }
    // -q tells the error above the question, as it always did
    let mut quiet = sandbox.command(&["-q", "add", "hf:datasets/scikit-learn/irs"]);
    quiet.env("FETCHLOOM_TEST_HF_API", &hub.url);
    let Some((code, screen)) = crate::support::typed(&sandbox.work(), &quiet, "", "now?", "\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(40), "{screen}");
    let told = screen.find("error: huggingface.co has no dataset scikit-learn/irs");
    let asked = screen.find("sign in to hugging face now? [y/N]");
    assert!(
        told.zip(asked).is_some_and(|(told, asked)| told < asked),
        "{screen}"
    );
    assert!(!screen.contains("Cancelled"), "{screen}");
    Ok(())
}

/// the command runs once more after signing in, and a refusal then says the key it went with has
/// no access or was refused, never to sign in again, and asks nothing more: a saved key's help
/// says how to check it, and a key kept for the command alone is named so.
#[cfg(target_os = "linux")]
#[test]
fn a_refusal_after_signing_in_says_how_to_check_the_key() -> Result {
    let sandbox = Sandbox::new()?;
    let hub = hub()?;
    let mut add = sandbox.command(&["add", "hf:datasets/scikit-learn/irs"]);
    add.env("FETCHLOOM_TEST_HF_API", &hub.url);
    let page = "check the name at https://huggingface.co/datasets/scikit-learn/irs. if it is right";
    for (save, help) in [
        (
            "n",
            format!("{page}, the key you signed in with has no access to it\n"),
        ),
        (
            "y",
            format!("{page}, the key has no access to it: {CHECK_KEY}\n"),
        ),
    ] {
        let keys = format!("y\rhf_test1234567890\r{save}\r");
        let Some((code, screen)) = crate::support::typed(&sandbox.work(), &add, "", "now?", &keys)?
        else {
            return Ok(());
        };
        assert_eq!(code, Some(40), "{screen}");
        let signed = screen.find("Signed in to hf as ada");
        let told =
            screen.find("error: huggingface.co has no dataset scikit-learn/irs, or it is private");
        assert!(
            signed.zip(told).is_some_and(|(signed, told)| signed < told),
            "{screen}"
        );
        let after = &screen[told.unwrap_or_default()..];
        assert!(after.contains(&help), "{screen}");
        assert!(!after.contains("auth login"), "{screen}");
        assert_eq!(screen.matches("now?").count(), 1, "{screen}");
    }
    Ok(())
}

/// how a refusal with a key on this machine says to check it.
const CHECK_KEY: &str = "`fetchloom auth` shows where it comes from and whose it is";

/// with a key on this machine, a refusal says the key has no access, or was refused, and how to
/// check it, which `--json` gives as the fix, never a sign-in.
#[test]
fn a_refusal_with_a_key_here_says_how_to_check_it() -> Result {
    let sandbox = Sandbox::new()?;
    let hub = hub()?;
    let token = home(&sandbox)
        .join(".cache")
        .join("huggingface")
        .join("token");
    std::fs::create_dir_all(token.parent().ok_or("no folder")?)?;
    std::fs::write(&token, "hf_test1234567890\n")?;
    let output = sandbox
        .command(&["--json", "add", "hf:datasets/scikit-learn/irs"])
        .env("FETCHLOOM_TEST_HF_API", &hub.url)
        .output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    let shown = std::path::Path::new("~")
        .join(".cache")
        .join("huggingface")
        .join("token");
    assert_eq!(
        error["details"][0],
        format!("the key saved in {} went with the request", shown.display())
    );
    assert_eq!(
        error["help"],
        format!(
            "check the name at https://huggingface.co/datasets/scikit-learn/irs. if it is right, the key has no access to it: {CHECK_KEY}"
        )
    );
    assert_eq!(error["fix"], "fetchloom auth");
    Ok(())
}

/// a server that asks for a login over plain http gets no offer to sign in, since fetchloom
/// sends a key only over https, and the help says so.
#[test]
fn no_sign_in_is_offered_for_a_login_over_http() -> Result {
    let sandbox = Sandbox::new()?;
    let challenge = [
        ("WWW-Authenticate", "Basic realm=\"lab\""),
        ("Content-Length", "0"),
    ];
    let server = Routes::start(vec![(
        "/files/x.csv".to_owned(),
        reply("401 Unauthorized", &challenge, b""),
    )])?;
    let link = format!("{}/files/x.csv", server.url);
    let output = sandbox.run(&["--json", "add", &link])?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["errors"][0]["help"],
        "fetchloom sends a key only over https, so none goes with this http link: ask its owner for an https one"
    );
    #[cfg(target_os = "linux")]
    if let Some((code, screen)) = crate::support::typed(
        &sandbox.work(),
        &sandbox.command(&["add", &link]),
        "",
        "error:",
        "",
    )? {
        assert_eq!(code, Some(40), "{screen}");
        assert!(!screen.contains("now?"), "{screen}");
    }
    Ok(())
}

/// a link without `/datasets/` names a model. when the hub shows no such model but a dataset of
/// that name, the error gives the dataset's link rather than a sign-in that cannot help. a model
/// whose name no dataset has stays a refusal.
#[test]
fn a_model_link_whose_name_a_dataset_has_gives_the_datasets_link() -> Result {
    let sandbox = Sandbox::new()?;
    let hub = hub()?;
    let add = |link: &str| {
        sandbox
            .command(&["--json", "add", link])
            .env("FETCHLOOM_TEST_HF_API", &hub.url)
            .output()
    };
    let output = add("https://huggingface.co/scikit-learn/iris")?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "reference.unresolved", "{error}");
    assert_eq!(
        error["message"],
        "huggingface.co has no public model scikit-learn/iris, but it has a dataset of that name"
    );
    assert_eq!(
        error["help"],
        "for the dataset, use https://huggingface.co/datasets/scikit-learn/iris"
    );
    let output = add("https://huggingface.co/scikit-learn/irs")?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "huggingface.co has no model scikit-learn/irs, or it is private"
    );
    Ok(())
}

/// a key that went with the request and was refused, as an old or wrong `HF_TOKEN` is, is named
/// by where it comes from, never by its value, with how to replace it. signing in is not offered
/// for a key from a variable, which would still win over the key a sign-in saves.
#[test]
fn a_refused_key_is_named_by_where_it_comes_from() -> Result {
    let sandbox = Sandbox::new()?;
    let hub = hub()?;
    let key = "hf_FAKEfakeFAKEfakeFAKEfake1234";
    let output = sandbox
        .command(&["--json", "add", "hf:datasets/bigcode/the-stack"])
        .env("FETCHLOOM_TEST_HF_API", &hub.url)
        .env("HF_TOKEN", key)
        .output()?;
    let (stdout, stderr) = (text(&output.stdout)?, text(&output.stderr)?);
    assert_eq!(output.status.code(), Some(40), "{stderr}");
    assert!(
        !stdout.contains(key) && !stderr.contains(key),
        "{stdout}{stderr}"
    );
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{error}");
    assert_eq!(
        error["message"],
        "huggingface.co refused the key in HF_TOKEN for hf:datasets/bigcode/the-stack"
    );
    assert_eq!(
        error["help"],
        "the key may be old or wrong: set HF_TOKEN to a current key from https://huggingface.co/settings/tokens, or unset HF_TOKEN to use the key `fetchloom auth login hf` saves"
    );
    // a name the hub does not show may be a typo too, so the key is named beside it
    let output = sandbox
        .command(&["--json", "add", "hf:datasets/scikit-learn/irs"])
        .env("FETCHLOOM_TEST_HF_API", &hub.url)
        .env("HF_TOKEN", key)
        .output()?;
    let (stdout, stderr) = (text(&output.stdout)?, text(&output.stderr)?);
    assert_eq!(output.status.code(), Some(40), "{stderr}");
    assert!(
        !stdout.contains(key) && !stderr.contains(key),
        "{stdout}{stderr}"
    );
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["details"][0],
        "the key in HF_TOKEN went with the request"
    );
    assert!(
        error["help"]
            .as_str()
            .is_some_and(|help| help.contains("set HF_TOKEN to a current one")
                && !help.contains("fetchloom auth login")),
        "{error}"
    );
    Ok(())
}

/// a ctrl-c during a sign-out, which has nothing to stop, lets it remove the key and say so, exit
/// 0, rather than say it stops and then that it stopped, with the key removed.
#[cfg(unix)]
#[test]
fn a_ctrl_c_during_a_sign_out_lets_it_finish() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = home(&sandbox).join(".kaggle");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(
        folder.join("kaggle.json"),
        r#"{"username":"ada","key":"api-key"}"#,
    )?;
    let (running, release) = crate::support::held_at_its_mark(
        &sandbox,
        &mut sandbox.command(&["auth", "logout", "kaggle", "--yes"]),
    )?;
    let stderr = crate::support::finished_anyway(running, &release)?;
    assert!(stderr.contains("Signed out of kaggle"), "{stderr}");
    assert!(!folder.join("kaggle.json").exists());
    Ok(())
}

/// a ctrl-c once a sign-in has its key and marked its work as finishing, as while it saves the key,
/// lets it save and say so, exit 0, rather than say it stops and then that it stopped, with the
/// key saved.
#[cfg(unix)]
#[test]
fn a_ctrl_c_past_a_sign_ins_mark_lets_it_save_the_key() -> Result {
    let sandbox = Sandbox::new()?;
    let key = sandbox.folder("input")?.join("key");
    std::fs::write(&key, "hf_example\n")?;
    let mut command = sandbox.command(&["auth", "login", "hf", "--with-token", "--offline"]);
    command.stdin(std::fs::File::open(&key)?);
    let (running, release) = crate::support::held_at_its_mark(&sandbox, &mut command)?;
    let stderr = crate::support::finished_anyway(running, &release)?;
    assert!(
        stderr.contains("Signed in to hf \u{b7} saved to"),
        "{stderr}"
    );
    let token = home(&sandbox)
        .join(".cache")
        .join("huggingface")
        .join("token");
    assert_eq!(std::fs::read_to_string(token)?, "hf_example\n");
    Ok(())
}
