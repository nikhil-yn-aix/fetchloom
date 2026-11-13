//! signing in: one key, saved where the source's own tools read it, asked for only at a terminal.
#![cfg(debug_assertions)]

use std::process::Output;

use crate::support::{Fetchloom, Result, Routes, Sandbox, Server, json, reply, text, workspace};

pub(crate) fn with_stdin(mut command: Fetchloom, input: &str) -> Result<Output> {
    Ok(command.output_with(Some(input.as_bytes()))?)
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
    for account in ["aws", "google", "azure"] {
        assert!(
            accounts.iter().any(|a| a["account"] == account),
            "{account}: {document}"
        );
    }
    Ok(())
}

/// `auth status`, as other tools spell it, is no command, and its help names the one that is.
#[test]
fn auth_alone_lists_every_account() -> Result {
    let sandbox = Sandbox::new()?;
    let plain = sandbox.run(&["auth", "--offline"])?;
    assert!(plain.status.success(), "{}", text(&plain.stderr)?);
    let story = text(&plain.stderr)?;
    assert!(
        story.lines().any(|line| line
            == "  Signed in to none of hf, kaggle, zenodo, figshare, osf, dryad, github, gitlab, earthdata, physionet, aws, google, azure"),
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

/// lines show the file below the home folder as `~/…`, while `--json` gives the path whole.
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

    // the hf tools read the same file, so nobody at a terminal means it stays without --yes, a
    // question no one answered, as every other one is, which a script asks the person about
    let refused = sandbox.run(&["auth", "logout", "hf", "--json"])?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr)?);
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "unconfirmed", "{error}");
    assert_eq!(error["fix"], serde_json::Value::Null);
    assert!(
        error["help"]
            .as_str()
            .is_some_and(|help| help.contains("`fetchloom auth logout hf --yes`")),
        "{error}"
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
    let unknown = json(&sandbox.run(&["auth", "login", "bitbucket", "--json"])?)?;
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

/// q is no answer, and must never count as yes, though enter alone means yes there.
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

/// a key typed once is saved without another question, as `--yes` answers every question.
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

/// a screen reader reads a line again once it is drawn over, so with `FETCHLOOM_ACCESSIBLE` the
/// masked key comes on a line of its own below the prompt.
#[cfg(target_os = "linux")]
#[test]
fn an_accessible_terminal_shows_the_masked_key_without_drawing_over_the_prompt() -> Result {
    let sandbox = Sandbox::new()?;
    let mut login = sandbox.command(&["auth", "login", "hf", "--offline", "--yes"]);
    login.env("FETCHLOOM_ACCESSIBLE", "1");
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
        screen.contains("        key ************ · 17 characters\n"),
        "{screen:?}"
    );
    assert!(!screen.contains('\u{1b}'), "{screen:?}");
    Ok(())
}

/// a question at a narrow terminal wraps between words, as the story's own lines there do.
#[cfg(target_os = "linux")]
#[test]
fn a_question_wraps_between_words_at_a_narrow_terminal() -> Result {
    let sandbox = Sandbox::new()?;
    let token = home(&sandbox).join(".cache/huggingface/token");
    std::fs::create_dir_all(token.parent().ok_or("no folder")?)?;
    std::fs::write(&token, "hf_old\n")?;
    let logout = sandbox.command(&["auth", "logout", "hf"]);
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &logout, "rows 30 cols 40", "[y/N] ", "n\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(1), "{screen}");
    assert!(
        screen.contains("remove it from\n~/.cache/huggingface/token? [y/N] n\n"),
        "{screen}"
    );
    assert!(
        screen.lines().all(|line| line.chars().count() <= 40),
        "{screen}"
    );
    assert_eq!(std::fs::read_to_string(&token)?, "hf_old\n");
    Ok(())
}

/// a key typed at a terminal for `--with-token` stays hidden, as an interactive sign-in keeps it;
/// a piped one is read as it comes.
#[cfg(target_os = "linux")]
#[test]
fn a_key_typed_for_with_token_at_a_terminal_stays_hidden() -> Result {
    let sandbox = Sandbox::new()?;
    let login = sandbox.command(&["auth", "login", "hf", "--with-token", "--offline"]);
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
    assert!(!screen.contains("hf_test1234567890"), "{screen}");
    assert_eq!(
        std::fs::read_to_string(home(&sandbox).join(".cache/huggingface/token"))?,
        "hf_test1234567890\n"
    );
    Ok(())
}

/// the questions are saving the key, replacing one and removing a key other tools read; one line
/// says what was cancelled, and the exit code is 1, as for any no.
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

/// the hub on this machine, answering a name nobody may see, as a typo gets, a gated dataset
/// before its terms are accepted, a model whose name a dataset has, and a sign-in's key check.
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

/// the refused line comes first, then what else may be wrong: a typo, which the hub answers as a
/// private name, or terms to accept. enter alone is a no, so a reflex starts no sign-in.
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
    // with -q, the error comes above the question
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

/// the command runs again after signing in, and its refusal never offers another sign-in; a key
/// kept for the command alone is named so, and a saved key's help says how to check it.
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

/// `--json` gives the check as the fix, never a sign-in.
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

/// fetchloom sends a key only over https, and the help says so.
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

/// a link without `/datasets/` names a model, and a sign-in could not help; a model whose name no
/// dataset has stays a refusal.
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

/// the key's value never shows. no sign-in is offered for a key from a variable, which would
/// still win over the key a sign-in saves.
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

/// a sign-out has nothing to stop, so it removes the key and says so, with exit 0.
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

/// the mark comes once the sign-in has its key, so it saves it and says so, with exit 0.
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

/// the accounts `auth --json` lists, and the lines `auth` tells a person, run with `env`, after
/// checking that neither shows any of `secrets`.
fn auth_says(
    sandbox: &Sandbox,
    env: &[(&str, String)],
    secrets: &[&str],
) -> Result<(Vec<serde_json::Value>, String)> {
    let run = |args: &[&str]| {
        let mut command = sandbox.command(args);
        command.envs(env.iter().map(|(name, value)| (name, value)));
        command.output()
    };
    let document = run(&["auth", "--json"])?;
    let person = run(&["auth"])?;
    assert!(person.status.success(), "{}", text(&person.stderr)?);
    let story = text(&person.stderr)?;
    let said = format!("{}{story}", text(&document.stdout)?);
    for secret in secrets {
        assert!(!said.contains(secret), "{secret} shown: {said}");
    }
    let accounts = json(&document)?["result"]["accounts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok((accounts, story))
}

/// the row of `account` among `accounts`, or null.
fn row(accounts: &[serde_json::Value], account: &str) -> serde_json::Value {
    accounts
        .iter()
        .find(|row| row["account"] == account)
        .cloned()
        .unwrap_or_default()
}

/// the secrets of the AWS settings these tests write, and the parts of their key ids no output may
/// show.
const AWS_SECRETS: [&str; 4] = [
    "environment-secret-never-shown",
    "file-secret-never-shown",
    "AKIAENVIRONMENT",
    "AKIAFILEKEY",
];

/// AWS settings in the sandbox's home: keys for `default`, and profiles `process`, `sso` and
/// `role` whose programs, in the folder this returns, leave the file `ran` if they ever run.
fn aws_settings(sandbox: &Sandbox) -> Result<std::path::PathBuf> {
    let aws = home(sandbox).join(".aws");
    std::fs::create_dir_all(&aws)?;
    std::fs::write(
        aws.join("credentials"),
        "[default]\naws_access_key_id = AKIAFILEKEYABCD\naws_secret_access_key = file-secret-never-shown\n",
    )?;
    let bin = sandbox.folder("bin")?;
    #[cfg(unix)]
    for name in ["aws", "keys"] {
        let ran = sandbox.work().join("ran");
        crate::buckets::program(&bin.join(name), &format!(": > '{}'", ran.display()))?;
    }
    #[cfg(windows)]
    for name in ["aws.exe", "keys.exe"] {
        std::fs::write(bin.join(name), b"not a program")?;
    }
    std::fs::write(
        aws.join("config"),
        format!(
            "[profile process]\ncredential_process = \"{}\" --quiet\n[profile sso]\nsso_session = lab\nsso_account_id = 123456789012\nsso_role_name = reader\n[profile role]\nrole_arn = arn:aws:iam::123456789012:role/reader\nsource_profile = default\n",
            bin.join("keys").display()
        ),
    )?;
    Ok(bin)
}

/// a program a profile names never runs, a key shows only by its id's last four characters, and
/// keys the settings send to another endpoint are that endpoint's, leaving AWS with none.
#[test]
fn auth_names_where_aws_keys_would_come_from() -> Result {
    let sandbox = Sandbox::new()?;
    let bin = aws_settings(&sandbox)?;
    let aws = home(&sandbox).join(".aws");
    let file = |name: &str| aws.join(name).display().to_string();
    let files = [
        ("AWS_SHARED_CREDENTIALS_FILE", file("credentials")),
        ("AWS_CONFIG_FILE", file("config")),
    ];
    let keys = [
        ("AWS_ACCESS_KEY_ID", "AKIAENVIRONMENTWXYZ".to_owned()),
        (
            "AWS_SECRET_ACCESS_KEY",
            "environment-secret-never-shown".to_owned(),
        ),
    ];
    let and = |base: &[(&'static str, String)], more: &[(&'static str, &str)]| {
        let more = more
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()));
        base.iter().cloned().chain(more).collect::<Vec<_>>()
    };
    let bin = bin.display().to_string();
    let shown = "~/.aws/credentials".replace('/', std::path::MAIN_SEPARATOR_STR);
    let later = "when a bucket needs keys";
    let minio = "https://minio.lab.example:9000";
    for (env, account, from, kind, said) in [
        (
            keys.to_vec(),
            "aws",
            "AWS_ACCESS_KEY_ID".to_owned(),
            "keys",
            "aws with key \u{2026}WXYZ \u{b7} AWS_ACCESS_KEY_ID".to_owned(),
        ),
        (
            files.to_vec(),
            "aws",
            format!("profile default in {}", file("credentials")),
            "keys",
            format!("aws with key \u{2026}ABCD \u{b7} profile default in {shown}"),
        ),
        (
            and(&files, &[("AWS_PROFILE", "process")]),
            "aws",
            "the credential_process of profile process".to_owned(),
            "credential_process",
            format!("aws \u{b7} the credential_process of profile process, which runs {later}"),
        ),
        (
            and(&files, &[("AWS_PROFILE", "sso"), ("PATH", &bin)]),
            "aws",
            "the AWS CLI's profile sso".to_owned(),
            "sso",
            format!(
                "aws \u{b7} the AWS CLI's profile sso, which signs in to IAM identity center {later}"
            ),
        ),
        (
            and(&files, &[("AWS_PROFILE", "role"), ("PATH", &bin)]),
            "aws",
            "the AWS CLI's profile role".to_owned(),
            "role",
            format!("aws \u{b7} the AWS CLI's profile role, which takes on a role {later}"),
        ),
        (
            and(&keys, &[("AWS_ENDPOINT_URL_S3", minio)]),
            "s3:https://minio.lab.example:9000",
            "AWS_ACCESS_KEY_ID".to_owned(),
            "keys",
            format!("s3 at {minio} with key \u{2026}WXYZ \u{b7} AWS_ACCESS_KEY_ID"),
        ),
    ] {
        let (accounts, story) = auth_says(&sandbox, &env, &AWS_SECRETS)?;
        assert_eq!(
            row(&accounts, account),
            serde_json::json!({
                "account": account,
                "signed_in": true,
                "from": from,
                "user": null,
                "valid": null,
                "kind": kind,
            }),
            "{said}"
        );
        assert!(
            story.contains(&format!("  Signed in to {said}\n")),
            "{story}"
        );
        assert_eq!(
            row(&accounts, "aws")["signed_in"],
            account == "aws",
            "{said}"
        );
        assert!(!sandbox.work().join("ran").exists(), "{said} ran a program");
    }
    Ok(())
}

/// the warning has a kind of its own, and AWS counts as having no keys.
#[test]
fn auth_warns_of_aws_settings_it_cannot_use() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox
        .command(&["auth", "--json"])
        .env("AWS_PROFILE", "nope")
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let warning = &document["warnings"][0];
    assert_eq!(warning["kind"], "auth.unusable", "{document}");
    assert_eq!(
        warning["message"],
        "AWS_PROFILE names the profile nope, which the AWS credentials and config files do not have"
    );
    let aws = document["result"]["accounts"]
        .as_array()
        .and_then(|accounts| accounts.iter().find(|a| a["account"] == "aws"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(aws["signed_in"], false, "{document}");
    Ok(())
}

/// a service account comes with its email, and nothing either kind signs with ever shows.
#[test]
fn auth_names_googles_credentials_by_file_and_kind() -> Result {
    let sandbox = Sandbox::new()?;
    let key = crate::google::service_account(&sandbox)?;
    let gcloud = crate::google::login(&sandbox)?;
    let saved = gcloud.join("application_default_credentials.json");
    let secrets = [
        "PRIVATE KEY",
        crate::google::CLIENT_SECRET,
        crate::google::REFRESH_TOKEN,
    ];
    for (variable, folder, from, user, kind, said) in [
        (
            "GOOGLE_APPLICATION_CREDENTIALS",
            key.clone(),
            key.display().to_string(),
            serde_json::json!(crate::google::EMAIL),
            "service_account",
            format!(
                "Signed in to google as {} \u{b7} {}",
                crate::google::EMAIL,
                key.display()
            ),
        ),
        (
            "CLOUDSDK_CONFIG",
            gcloud.clone(),
            saved.display().to_string(),
            serde_json::Value::Null,
            "authorized_user",
            format!(
                "Signed in to google with a person's login \u{b7} {}",
                saved.display()
            ),
        ),
    ] {
        let env = [(variable, folder.display().to_string())];
        let (accounts, story) = auth_says(&sandbox, &env, &secrets)?;
        assert_eq!(
            row(&accounts, "google"),
            serde_json::json!({
                "account": "google",
                "signed_in": true,
                "from": from,
                "user": user,
                "valid": null,
                "kind": kind,
            }),
            "{said}"
        );
        assert!(story.contains(&format!("  {said}\n")), "{story}");
    }
    Ok(())
}

/// the az cli, which leaves `ran` if it ever runs, and its profile, signed in as ada and signed
/// out, as the az cli writes them: with a byte order mark, and no subscription once signed out.
fn az_settings(
    sandbox: &Sandbox,
) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    let bin = sandbox.folder("az-bin")?;
    let az = bin.join(if cfg!(windows) { "az.cmd" } else { "az" });
    #[cfg(unix)]
    crate::buckets::program(
        &az,
        &format!(": > '{}'", sandbox.work().join("ran").display()),
    )?;
    #[cfg(windows)]
    std::fs::write(&az, b"not a program")?;
    let signed_in = sandbox.folder("az-signed-in")?;
    std::fs::write(
        signed_in.join("azureProfile.json"),
        "\u{feff}{\"installationId\": \"1\", \"subscriptions\": [{\"id\": \"a\", \"isDefault\": false, \"user\": {\"name\": \"bo@lab.example\", \"type\": \"user\"}}, {\"id\": \"b\", \"isDefault\": true, \"user\": {\"name\": \"ada@lab.example\", \"type\": \"user\"}}]}",
    )?;
    let signed_out = sandbox.folder("az-signed-out")?;
    std::fs::write(
        signed_out.join("azureProfile.json"),
        "\u{feff}{\"installationId\": \"1\"}",
    )?;
    Ok((az, signed_in, signed_out))
}

/// what of the azure settings these tests write no output may show: the SAS's signature, and the
/// account key as it is written and as the bytes it stands for.
const AZURE_SECRETS: [&str; 4] = [
    crate::azure::SECRETS[0],
    crate::azure::SECRETS[1],
    crate::azure::KEY,
    "secret-key",
];

/// the row of `account` signed in with what `from` gives, as `auth --json` lists it.
fn signed_in_row(account: &str, from: &str, user: Option<&str>, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "account": account, "signed_in": true, "from": from, "user": user, "valid": null,
        "kind": kind,
    })
}

/// a SAS or key kept to the account `AZURE_STORAGE_ACCOUNT` names is listed as that account's;
/// the az cli never runs, the managed identity is never asked, and neither a SAS nor a key shows.
#[test]
fn auth_names_where_azure_credentials_would_come_from() -> Result {
    let sandbox = Sandbox::new()?;
    let (az, signed_in, signed_out) = az_settings(&sandbox)?;
    let vars = |pairs: &[(&'static str, &str)]| {
        let pairs = pairs
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()));
        pairs.collect::<Vec<_>>()
    };
    let bin = az.parent().unwrap_or(&az).display().to_string();
    let cli = |profile: &std::path::Path| {
        vars(&[
            ("PATH", &bin),
            ("AZURE_CONFIG_DIR", &profile.display().to_string()),
        ])
    };
    let row = signed_in_row;
    let none = serde_json::json!({
        "account": "azure", "signed_in": false, "from": null, "user": null, "valid": null,
        "kind": null,
    });
    let later = "when a container needs a token";
    let identity = "0b4f5c1e-1a2b-4c3d-8e9f-000000000001";
    let shown = az.display();
    for (env, rows, said) in [
        (
            vars(&[("AZURE_STORAGE_SAS_TOKEN", crate::azure::SAS)]),
            vec![row("azure", "AZURE_STORAGE_SAS_TOKEN", None, "sas")],
            vec!["Signed in to azure with a SAS \u{b7} AZURE_STORAGE_SAS_TOKEN".to_owned()],
        ),
        (
            [
                cli(&signed_out),
                vars(&[
                    ("AZURE_STORAGE_KEY", crate::azure::KEY),
                    ("AZURE_STORAGE_ACCOUNT", "lab"),
                ]),
            ]
            .concat(),
            vec![row("azure:lab", "AZURE_STORAGE_KEY", None, "key"), none.clone()],
            vec![
                "Signed in to azure account lab with its key \u{b7} AZURE_STORAGE_KEY".to_owned(),
                "Signed in to none of hf, kaggle, zenodo, figshare, osf, dryad, github, gitlab, earthdata, physionet, aws, google, azure".to_owned(),
            ],
        ),
        (
            [
                cli(&signed_in),
                vars(&[
                    ("AZURE_STORAGE_SAS_TOKEN", crate::azure::SAS),
                    ("AZURE_STORAGE_KEY", crate::azure::KEY),
                    ("AZURE_STORAGE_ACCOUNT", "Lab"),
                ]),
            ]
            .concat(),
            vec![
                row("azure:lab", "AZURE_STORAGE_SAS_TOKEN", None, "sas"),
                row("azure", &shown.to_string(), Some("ada@lab.example"), "az_cli"),
            ],
            vec![
                "Signed in to azure account lab with a SAS \u{b7} AZURE_STORAGE_SAS_TOKEN".to_owned(),
                format!("Signed in to azure as ada@lab.example \u{b7} the az cli at {shown}, which runs {later}"),
            ],
        ),
        (
            [
                cli(&signed_in),
                vars(&[
                    ("AZURE_TOKEN_CREDENTIALS", "ManagedIdentityCredential"),
                    ("AZURE_CLIENT_ID", identity),
                ]),
            ]
            .concat(),
            vec![row("azure", "AZURE_TOKEN_CREDENTIALS", Some(identity), "managed_identity")],
            vec![format!(
                "Signed in to azure as managed identity {identity} \u{b7} AZURE_TOKEN_CREDENTIALS, asked {later}"
            )],
        ),
        (
            vars(&[("AZURE_TOKEN_CREDENTIALS", "prod")]),
            vec![row("azure", "AZURE_TOKEN_CREDENTIALS", None, "managed_identity")],
            vec![format!(
                "Signed in to azure as this machine's managed identity \u{b7} AZURE_TOKEN_CREDENTIALS, asked {later}"
            )],
        ),
    ] {
        let (accounts, story) = auth_says(&sandbox, &env, &AZURE_SECRETS)?;
        let azure: Vec<_> = accounts
            .into_iter()
            .filter(|row| row["account"].as_str().is_some_and(|a| a.starts_with("azure")))
            .collect();
        assert_eq!(azure, rows, "{said:?}");
        for said in said {
            assert!(story.contains(&format!("  {said}\n")), "{said}: {story}");
        }
        assert!(!sandbox.work().join("ran").exists(), "{env:?} ran the az cli");
    }
    Ok(())
}

/// a key without its account leaves azure with no credential, while a sign-in fetchloom does not
/// read still leaves the key of the account it is kept to.
#[test]
fn auth_warns_of_azure_settings_it_cannot_use() -> Result {
    let sandbox = Sandbox::new()?;
    for (env, warned, signed_in) in [
        (
            &[("AZURE_STORAGE_KEY", crate::azure::KEY)][..],
            "AZURE_STORAGE_KEY is set, but AZURE_STORAGE_ACCOUNT is not",
            Vec::<&str>::new(),
        ),
        (
            &[
                ("AZURE_STORAGE_KEY", crate::azure::KEY),
                ("AZURE_STORAGE_ACCOUNT", "lab"),
                ("AZURE_TOKEN_CREDENTIALS", "EnvironmentCredential"),
            ],
            "AZURE_TOKEN_CREDENTIALS asks for EnvironmentCredential, which fetchloom does not read",
            vec!["azure:lab"],
        ),
    ] {
        let output = sandbox
            .command(&["auth", "--json"])
            .envs(env.iter().copied())
            .output()?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
        let document = json(&output)?;
        let warning = &document["warnings"][0];
        assert_eq!(warning["kind"], "auth.unusable", "{document}");
        assert_eq!(warning["message"], warned, "{document}");
        let accounts = document["result"]["accounts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let azure: Vec<_> = accounts
            .iter()
            .filter(|row| row["signed_in"] == true)
            .filter_map(|row| row["account"].as_str())
            .collect();
        assert_eq!(azure, signed_in, "{document}");
        assert_eq!(row(&accounts, "azure")["signed_in"], false, "{document}");
    }
    Ok(())
}
