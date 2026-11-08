//! `auth`: which accounts are signed in, signing in once with a key shared with each source's own
//! tools, and asking for that key when a download is refused.

use std::io::Read as _;
use std::sync::Arc;

use serde::Serialize;

use super::Context;
use super::args::{Auth, AuthAction, Login, Logout};
use crate::auth::{self, Account, Found, Home, Need, Origin, Secret, Step, store};
use crate::error::{Error, Kind};
use crate::net::Api;

/// what `auth` reports.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Report {
    Status {
        accounts: Vec<State>,
    },
    Login(Outcome),
    Logout {
        account: String,
        removed_from: Option<String>,
    },
}

/// one account in `auth status`.
#[derive(Serialize)]
pub(super) struct State {
    account: String,
    signed_in: bool,
    from: Option<String>,
    /// who the key belongs to, when the source says
    user: Option<String>,
    /// whether the source accepted the key: none when it was not asked
    valid: Option<bool>,
}

/// what `auth login` saved.
#[derive(Serialize)]
pub(super) struct Outcome {
    account: String,
    user: Option<String>,
    saved_to: Option<String>,
}

pub(super) async fn run(context: &Arc<Context>, args: Auth) -> Result<Report, Error> {
    match args.action {
        None | Some(AuthAction::Status) => status(context).await,
        Some(AuthAction::Login(login)) => {
            let account = Account::parse(&login.account, login.host.as_deref())?;
            sign_in(context, &account, &login, false)
                .await
                .map(Report::Login)
                .map_err(|error| {
                    error.cancelling(format!(
                        "signing in to {}, so nothing changed",
                        title(&account)
                    ))
                })
        }
        Some(AuthAction::Logout(logout)) => sign_out(context, &logout),
    }
}

async fn status(context: &Arc<Context>) -> Result<Report, Error> {
    let output = &context.output;
    let mut accounts = Vec::new();
    let mut missing = Vec::new();
    // this command tells each problem itself, account by account
    context
        .cautioned
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(path) = auth::netrc::path(&auth::env).filter(|path| auth::netrc::too_open(path)) {
        output.warn_with(
            &format!(
                "other users of this machine can open {}, which holds logins",
                path.display()
            ),
            &format!("limit it to yourself with `chmod 600 {}`", path.display()),
        );
    }
    for account in auth::listed(&auth::env) {
        let found = account.find(&auth::env);
        let (user, valid) = match &found {
            // asking with a key that is never sent would only say the source refused it
            Some(found) if !account.sendable(found) => {
                warn_unsendable(output, &account, found);
                (None, None)
            }
            Some(found) if !context.offline.enabled() => check(context, &account, found).await,
            _ => (None, None),
        };
        let name = title(&account);
        match &found {
            Some(found) => {
                let who = user
                    .as_ref()
                    .map_or_else(String::new, |user| format!(" as {user}"));
                let text = format!("{name}{who} \u{b7} {}", found.origin);
                if valid == Some(false) {
                    output.mark(
                        "Refused",
                        &format!("{text}; sign in again with `{}`", account.command()),
                    );
                } else {
                    output.step("Signed in", &text);
                }
            }
            None => missing.push(name),
        }
        accounts.push(State {
            account: account.id(),
            signed_in: found.is_some(),
            from: found.as_ref().map(|found| found.origin.to_string()),
            user,
            valid,
        });
    }
    if !missing.is_empty() {
        output.faint_step("Signed out", &missing.join(", "));
    }
    if !accounts.iter().any(|state| state.signed_in) {
        output.hint("public data needs no account. to sign in, run `fetchloom auth login hf`, or another source");
    }
    Ok(Report::Status { accounts })
}

/// signs in to `account`: shows where to get a key, takes it, checks it with the source, and
/// saves it where the source's own tools read it, once the person agrees. `rerun` says the
/// command runs again after signing in, which a key kept out of files still serves.
async fn sign_in(
    context: &Arc<Context>,
    account: &Account,
    login: &Login,
    rerun: bool,
) -> Result<Outcome, Error> {
    let output = &context.output;
    let (secret, user_name) = if login.with_token {
        read_stdin(account)?
    } else {
        if !output.can_ask() {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "signing in to {} needs a terminal to paste the key into",
                    title(account)
                ),
            )
            .help(format!(
                "in your own terminal, run `{}`. a script can pipe the key in, as in `{} --with-token`",
                account.command(),
                account.command()
            )));
        }
        if let Some(found) = account.find(&auth::env) {
            output.step(
                "Found",
                &format!("a {} key in {}", title(account), found.origin),
            );
            if agree(context, "replace it?", false)? != Some(true) {
                after_no(rerun)?;
                return Ok(Outcome {
                    account: account.id(),
                    user: None,
                    saved_to: Some(found.origin.to_string()),
                });
            }
        }
        guide(context, account);
        ask(context, account)?
    };
    let found = Found {
        origin: Origin::Keyring,
        secret: secret.clone(),
        login: user_name.clone(),
    };
    let (user, valid) = if context.offline.enabled() {
        (None, None)
    } else {
        check(context, account, &found).await
    };
    if valid == Some(false) {
        return Err(refused(account));
    }
    if valid == Some(true) {
        let who = user
            .as_ref()
            .map_or_else(String::new, |user| format!(" for {user}"));
        output.step("Checked", &format!("the key works{who}"));
    }
    let saved_to = save(
        context,
        account,
        &secret,
        user_name.as_deref(),
        login.with_token,
    )?;
    if saved_to.is_none() {
        after_no(rerun)?;
        // a key kept out of files still serves this command, as when it runs again after signing in
        context
            .unsaved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(account.credentials(&found));
    }
    let place = saved_to.as_ref().map_or_else(
        || "not saved, so it ends with this command".to_owned(),
        |to| format!("saved to {to}"),
    );
    let who = user
        .as_ref()
        .map_or_else(String::new, |user| format!(" as {user}"));
    output.done(
        "Signed in",
        &format!("to {}{who} \u{b7} {place}", title(account)),
    );
    if saved_to.is_some()
        && let Some(tools) = account.shared_with()
    {
        output.hint(&format!("{tools} use this key too"));
    }
    Ok(Outcome {
        account: account.id(),
        user,
        saved_to,
    })
}

/// what a no while signing in, to replacing a key or to saving it, leaves: a command that runs
/// again after signing in, `rerun`, goes on with the key it has. nothing runs after `auth login`
/// to use a key, so there the no is a cancel that changed nothing.
fn after_no(rerun: bool) -> Result<(), Error> {
    if rerun {
        return Ok(());
    }
    Err(answered_no())
}

/// a no to a question about a key, as a cancel of what its caller names. like ctrl-c at the same
/// question, it changed nothing.
fn answered_no() -> Error {
    Error::new(Kind::Unconfirmed, "answered no").cancelled("the question")
}

/// the error for a key, or a login's username and password, the source refused.
fn refused(account: &Account) -> Error {
    let (secret, help) = match account.secret() {
        Secret::Password => (
            "username and password",
            "check the username and the password, then sign in again",
        ),
        Secret::Token => (
            "key",
            "check that you copied all of it and that it has not expired, then sign in again",
        ),
    };
    Error::new(
        Kind::AuthRequired,
        format!("{} did not accept this {secret}", title(account)),
    )
    .help(help)
}

/// the steps to get a key, with each page as a link a person can click.
fn guide(context: &Context, account: &Account) {
    let output = &context.output;
    output.say(&format!("{:>11} {}", "Sign in", title(account)));
    for (number, step) in account.guide().into_iter().enumerate() {
        let text = match step {
            Step::Open(page) => format!("open {}", output.link(&page)),
            Step::Do(text) => text,
        };
        output.say(&format!("{:>11} {text}", format!("{}.", number + 1)));
    }
}

fn ask(context: &Context, account: &Account) -> Result<(String, Option<String>), Error> {
    let output = &context.output;
    let stopped = || Error::new(Kind::Interrupted, "stopped signing in");
    match account.secret() {
        Secret::Password => {
            let login = output
                .ask_text("username", false)?
                .filter(|text| !text.is_empty())
                .ok_or_else(stopped)?;
            let password = output
                .ask_text("password", true)?
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(stopped)?;
            Ok((password, Some(login)))
        }
        Secret::Token => {
            let pasted = output
                .ask_text("key", true)?
                .filter(|text| !text.is_empty())
                .ok_or_else(stopped)?;
            Ok(token_from(account, &pasted))
        }
    }
}

/// a pasted key, or for kaggle the contents of a `kaggle.json` whose path was pasted.
fn token_from(account: &Account, pasted: &str) -> (String, Option<String>) {
    let pasted = pasted.trim().trim_matches('"');
    if *account == Account::Kaggle {
        let path = std::path::Path::new(pasted);
        if path.is_file()
            && let Ok(text) = std::fs::read_to_string(path)
        {
            return (text.trim().to_owned(), None);
        }
    }
    (pasted.to_owned(), None)
}

fn read_stdin(account: &Account) -> Result<(String, Option<String>), Error> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|err| Error::io("could not read the key from stdin", &err))?;
    // only the line ending a pipe adds goes; a password may start or end with a space
    let text = text.trim_end_matches(['\r', '\n']).to_owned();
    if text.trim().is_empty() {
        return Err(
            Error::new(Kind::Usage, "--with-token read nothing from stdin").help(format!(
                "pipe the key in, as in `echo $KEY | {} --with-token`",
                account.command()
            )),
        );
    }
    match account.secret() {
        Secret::Password => {
            let (login, password) = text.split_once(':').ok_or_else(|| {
                Error::new(Kind::Usage, "a login reads username:password from stdin").help(format!(
                    "as in `echo \"$USER:$PASSWORD\" | {} --with-token`",
                    account.command()
                ))
            })?;
            Ok((password.to_owned(), Some(login.trim().to_owned())))
        }
        Secret::Token => Ok(token_from(account, &text)),
    }
}

/// asks the source who the key belongs to: the name and whether it was accepted, or nothing
/// when the source has no such check or could not be reached.
async fn check(
    context: &Arc<Context>,
    account: &Account,
    found: &Found,
) -> (Option<String>, Option<bool>) {
    let Some(check) = account.check() else {
        return (None, None);
    };
    let Some(credential) = account.credential(found) else {
        return (None, None);
    };
    let net = context.net_with(vec![credential]);
    // a dataverse host came from the person, through --host or their own earlier sign-in, so it
    // may be on a private network, as `add` allows
    if let Account::Dataverse(_) = account
        && let Some(host) = check.url.host_str()
    {
        net.trust([host.to_owned()]);
    }
    match net.get(&check.url).await {
        Ok(response) if (200..300).contains(&response.status) => {
            let user = response
                .json::<serde_json::Value>()
                .ok()
                .and_then(|value| (check.who)(&value));
            (user, Some(true))
        }
        Ok(response) if matches!(response.status, 401 | 403) => (None, Some(false)),
        Ok(_) | Err(_) => (None, None),
    }
}

/// saves the key where it belongs, asking first. returns where it went, or none when it was
/// kept for this command only.
/// asks `question`, unless `--yes` answered it, as it answers every question a person could
/// have been asked. nobody to ask still answers none.
fn agree(context: &Context, question: &str, default: bool) -> Result<Option<bool>, Error> {
    if context.yes && context.output.can_ask() {
        return Ok(Some(true));
    }
    context.output.ask_yes_no(question, default)
}

fn save(
    context: &Context,
    account: &Account,
    secret: &str,
    login: Option<&str>,
    told: bool,
) -> Result<Option<String>, Error> {
    let output = &context.output;
    let var = &auth::env;
    let agrees = |question: String, default: bool| -> Result<bool, Error> {
        if told {
            return Ok(true);
        }
        Ok(agree(context, &question, default)? == Some(true))
    };
    let failed =
        |err: std::io::Error| Error::io(format!("could not save the {} key", title(account)), &err);
    match account.home(var, secret) {
        Home::File(path) => {
            let (question, default) = save_question(account, &path);
            if !agrees(question, default)? {
                return Ok(None);
            }
            let text = if path.file_name().is_some_and(|name| name == "kaggle.json") {
                kaggle_json(secret)?
            } else {
                format!("{secret}\n")
            };
            store::write_private(&path, text.as_bytes()).map_err(failed)?;
            warn_if_shared(output, &path);
            Ok(Some(path.display().to_string()))
        }
        Home::Netrc(path) => {
            let (question, default) = save_question(account, &path);
            if !agrees(question, default)? {
                return Ok(None);
            }
            let Account::Login(host) = account else {
                return Ok(None);
            };
            auth::netrc::save(&path, host, login.unwrap_or_default(), secret).map_err(failed)?;
            warn_if_shared(output, &path);
            Ok(Some(path.display().to_string()))
        }
        Home::Store => {
            let id = account.id();
            if store::keyring_usable(var) {
                if !agrees(format!("save it in {}?", auth::keyring_name()), true)? {
                    return Ok(None);
                }
                store::save(var, &id, secret, store::Place::Keyring).map_err(failed)?;
                return Ok(Some(auth::keyring_name().to_owned()));
            }
            let file = store::folder(var)
                .unwrap_or_default()
                .join("credentials.json");
            if told {
                output.warn(&format!(
                    "this machine has no credential store, so the key is saved in {}, readable only by you",
                    file.display()
                ));
            } else {
                let (question, default) = save_question(account, &file);
                if !agrees(question, default)? {
                    output.hint(&format!(
                        "to keep it out of files, set {} in your environment instead",
                        variable(account)
                    ));
                    return Ok(None);
                }
            }
            store::save(var, &id, secret, store::Place::File).map_err(failed)?;
            Ok(Some(file.display().to_string()))
        }
    }
}

/// the question before saving `account`'s key to the file `path`, and the answer enter gives:
/// yes for a file the source's own tools read, no for the file a machine with no credential store
/// keeps keys in. the tools are named once the key is saved, so the question fits a terminal.
fn save_question(account: &Account, path: &std::path::Path) -> (String, bool) {
    let save = format!("save it to {}?", path.display());
    if account.shared_with().is_some() {
        (save, true)
    } else {
        (
            format!("no credential store here, so it would go in a file only you can read\n{save}"),
            false,
        )
    }
}

/// the question before removing a key from `path`, a file `tools` read too.
fn remove_question(tools: &str, path: &std::path::Path) -> String {
    format!(
        "{tools} use this key too\nremove it from {}?",
        path.display()
    )
}

/// on windows a saved file takes its folder's permissions. the profile keeps other users out, but
/// a folder elsewhere, such as an `HF_HOME` on another drive, may let them read the key.
fn warn_if_shared(output: &crate::output::Output, path: &std::path::Path) {
    if !cfg!(windows) {
        return;
    }
    // compared part by part, so C:\Users\bob-shared is not inside C:\Users\bob
    let lower =
        |path: &std::path::Path| std::path::PathBuf::from(path.to_string_lossy().to_lowercase());
    let inside = auth::env("USERPROFILE")
        .map(std::path::PathBuf::from)
        .is_some_and(|profile| lower(path).starts_with(lower(&profile)));
    if !inside {
        output.warn_with(
            &format!(
                "{} is outside your user folder, so other users of this machine may be able to read it",
                path.display()
            ),
            "keep the key in your profile, or limit the folder's permissions",
        );
    }
}

/// a `kaggle.json` as the kaggle tools write it, from its pasted contents.
fn kaggle_json(secret: &str) -> Result<String, Error> {
    let value: serde_json::Value = serde_json::from_str(secret).map_err(|_| {
        Error::new(Kind::Usage, "that is not a kaggle.json")
            .help("paste the key that starts with KGAT_, or the path to kaggle.json")
    })?;
    if value["username"].as_str().is_none() || value["key"].as_str().is_none() {
        return Err(Error::new(
            Kind::Usage,
            "kaggle.json needs a username and a key",
        ));
    }
    Ok(format!("{value}\n"))
}

fn sign_out(context: &Context, logout: &Logout) -> Result<Report, Error> {
    let output = &context.output;
    let account = Account::parse(&logout.account, logout.host.as_deref())?;
    let var = &auth::env;
    let Some(found) = account.find(var) else {
        output.done("Ok", &format!("no {} key was saved", title(&account)));
        return Ok(Report::Logout {
            account: account.id(),
            removed_from: None,
        });
    };
    let failed = |err: std::io::Error| {
        Error::io(
            format!("could not remove the {} key", title(&account)),
            &err,
        )
    };
    let removed = match &found.origin {
        Origin::Variable(name) => {
            return Err(Error::new(
                Kind::Usage,
                format!("the {} key comes from {name}", title(&account)),
            )
            .help(format!("unset {name} in your environment to sign out")));
        }
        Origin::Keyring => {
            store::forget(var, &account.id()).map_err(failed)?;
            auth::keyring_name().to_owned()
        }
        Origin::File(path) => {
            let shared = account.shared_with();
            if let Some(tools) = shared {
                let question = remove_question(tools, path);
                let removing = format!("removing the {} key, so nothing changed", title(&account));
                match agree(context, &question, false)
                    .map_err(|error| error.cancelling(&removing))?
                {
                    Some(true) => {}
                    None if context.yes => {}
                    Some(false) => return Err(answered_no().cancelling(removing)),
                    // nobody can be asked, so a file other tools need stays unless --yes says so
                    None => {
                        return Err(Error::new(
                            Kind::Usage,
                            format!("{tools} read {} too", path.display()),
                        )
                        .help(format!(
                            "to sign them out as well, run `fetchloom auth logout {} --yes`",
                            logout.account
                        )));
                    }
                }
            }
            match &account {
                Account::Login(host) => {
                    auth::netrc::remove(path, host).map_err(failed)?;
                }
                _ if store::saved(var).contains_key(&account.id()) => {
                    store::forget(var, &account.id()).map_err(failed)?;
                }
                _ => std::fs::remove_file(path).map_err(failed)?,
            }
            path.display().to_string()
        }
    };
    output.done(
        "Signed out",
        &format!("of {} \u{b7} removed from {removed}", title(&account)),
    );
    // another configured location may still supply a key for the same source
    if let Some(left) = account.find(var) {
        output.warn_with(
            &format!(
                "a {} key is still read from {}",
                title(&account),
                left.origin
            ),
            &format!(
                "run `fetchloom auth logout {}` again to remove it too",
                logout.account
            ),
        );
    }
    Ok(Report::Logout {
        account: account.id(),
        removed_from: Some(removed),
    })
}

/// when a command failed for want of an account and a person is at the terminal: offers to sign
/// in, or to accept a gated dataset's terms, then says whether running the command again could
/// now work.
pub(super) async fn offer(context: &Arc<Context>, need: &Need) -> Result<bool, Error> {
    let output = &context.output;
    let account = &need.account;
    let found = account.find(&auth::env);
    if let Some(found) = found.as_ref().filter(|found| !account.sendable(found)) {
        warn_unsendable(output, account, found);
    }
    let has_key = found.is_some();
    if let Some(terms) = need.terms.as_ref().filter(|_| has_key) {
        output.say(&format!("{:>11} {}", "Access", title(account)));
        output.say(&format!(
            "{:>11} accept the terms at {}",
            "1.",
            output.link(terms)
        ));
        output.say(&format!(
            "{:>11} some datasets wait for the owner's approval",
            "2."
        ));
        let again = agree(context, "try again now?", true)
            .map_err(|error| error.cancelling("trying again"))?;
        return Ok(again == Some(true));
    }
    let signing = format!("signing in to {}", title(account));
    if !agree(
        context,
        &format!("sign in to {} now?", title(account)),
        true,
    )
    .map_err(|error| error.cancelling(&signing))?
    .unwrap_or(false)
    {
        return Ok(false);
    }
    if let Some(terms) = &need.terms {
        output.say(&format!(
            "{:>11} accept the terms at {} while signed in",
            "First",
            output.link(terms)
        ));
    }
    let login = Login {
        account: account.id(),
        host: None,
        with_token: false,
    };
    sign_in(context, account, &login, true)
        .await
        .map_err(|error| error.cancelling(signing))?;
    Ok(true)
}

/// says a key was found but no request can carry it, naming where it is but never the key.
fn warn_unsendable(output: &crate::output::Output, account: &Account, found: &Found) {
    let fix = match &found.origin {
        Origin::Variable(name) => format!("set {name} to the key alone"),
        _ => format!("sign in again with `{}`", account.command()),
    };
    output.warn_with(
        &format!(
            "the {} key in {} is never sent, since it holds a character no request header can carry, such as a line break",
            title(account),
            found.origin,
        ),
        &fix,
    );
}

fn title(account: &Account) -> String {
    match account {
        Account::HuggingFace => "hugging face".to_owned(),
        Account::Kaggle => "kaggle".to_owned(),
        Account::Zenodo => "zenodo".to_owned(),
        Account::Figshare => "figshare".to_owned(),
        Account::Osf => "osf".to_owned(),
        Account::Dataverse(host) => format!("dataverse at {host}"),
        Account::Login(host) if host == "urs.earthdata.nasa.gov" => "nasa earthdata".to_owned(),
        Account::Login(host) if host == "physionet.org" => "physionet".to_owned(),
        Account::Login(host) => host.clone(),
    }
}

fn variable(account: &Account) -> &'static str {
    match account {
        Account::Zenodo => "ZENODO_TOKEN",
        Account::Figshare => "FIGSHARE_TOKEN",
        Account::Osf => "OSF_TOKEN",
        Account::Dataverse(_) => "DATAVERSE_URL and DATAVERSE_API_TOKEN",
        Account::HuggingFace => "HF_TOKEN",
        Account::Kaggle => "KAGGLE_API_TOKEN",
        Account::Login(_) => "NETRC",
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::time::Duration;

    use super::*;
    use crate::output::{Level, Output, Style};
    use crate::source::recorded::block_on;

    fn context() -> Arc<Context> {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
        };
        Arc::new(Context::new(Output::new(style, Level::Quiet, false, false)))
    }

    /// a dataverse host a person typed may be on a private network, as `add` allows, so its key
    /// is checked there instead of the check being skipped.
    #[test]
    fn a_dataverse_key_is_checked_on_the_private_host_the_person_typed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let context = context();
        let stop = context.stop.clone();
        let (reached, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if listener.accept().is_ok() {
                // the server speaks no TLS, so the retries its failed handshake starts can stop
                stop.ask();
                let _ = reached.send(());
            }
        });
        let account = Account::parse("dataverse", Some(&format!("127.0.0.1:{port}"))).unwrap();
        let found = Found {
            origin: Origin::Keyring,
            secret: "key".to_owned(),
            login: None,
        };
        assert_eq!(block_on(check(&context, &account, &found)), (None, None));
        assert!(
            heard.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the key was never checked on the host the person typed"
        );
    }

    /// a login is a username and a password a person types, so its refusal names those rather
    /// than a key copied from a page.
    #[test]
    fn a_refused_login_names_the_username_and_password_not_a_key() {
        let login = refused(&Account::parse("earthdata", None).unwrap());
        assert_eq!(
            login.message,
            "nasa earthdata did not accept this username and password"
        );
        assert_eq!(
            login.help.as_deref(),
            Some("check the username and the password, then sign in again")
        );
        let key = refused(&Account::HuggingFace);
        assert_eq!(key.message, "hugging face did not accept this key");
        assert_eq!(
            key.help.as_deref(),
            Some("check that you copied all of it and that it has not expired, then sign in again")
        );
    }

    /// a no while signing in, to replacing a key or to saving it, ends `auth login` as a cancel
    /// that changed nothing, with exit 1 as any no does, since nothing runs after it to use the
    /// key. a command that runs again after signing in goes on with the key it has.
    #[test]
    fn a_no_ends_auth_login_as_a_cancel_but_lets_a_rerun_go_on() {
        assert!(after_no(true).is_ok());
        let cancel = after_no(false)
            .unwrap_err()
            .cancelling("signing in to hugging face, so nothing changed");
        assert_eq!(cancel.kind.exit_code(), 1);
        assert_eq!(
            cancel.cancelled.as_deref(),
            Some("signing in to hugging face, so nothing changed")
        );
    }

    /// each line of a question about saving or removing a key fits an 80-column terminal at the
    /// paths a home folder usually gives, so its `[y/N]` never splits.
    #[test]
    fn the_questions_about_a_saved_key_fit_an_80_column_terminal() {
        let earthdata = Account::parse("earthdata", None).unwrap();
        let mut questions = Vec::new();
        for (account, path) in [
            (&Account::HuggingFace, "/home/ada/.cache/huggingface/token"),
            (&Account::Kaggle, "/home/ada/.kaggle/access_token"),
            (&earthdata, "/home/ada/.netrc"),
            (
                &Account::Zenodo,
                "/home/ada/.config/fetchloom/credentials.json",
            ),
        ] {
            let path = std::path::Path::new(path);
            questions.push(save_question(account, path).0);
            if let Some(tools) = account.shared_with() {
                questions.push(remove_question(tools, path));
            }
        }
        for question in questions {
            for line in format!("{question} [y/N] ").lines() {
                assert!(
                    line.chars().count() < 80,
                    "{} columns: {line}",
                    line.chars().count()
                );
            }
        }
    }
}
