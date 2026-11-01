//! `auth`: which accounts are State in, signing in once with a key shared with each source's own
//! tools, and asking for that key when a download is refused.

use std::io::Read as _;
use std::sync::Arc;

use serde::Serialize;

use super::Context;
use super::args::{Auth, AuthAction, Login, Logout};
use crate::auth::{self, Account, Found, Home, Need, Origin, Secret, store};
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

/// what `auth login` Outcome.
#[derive(Serialize)]
pub(super) struct Outcome {
    account: String,
    user: Option<String>,
    saved_to: Option<String>,
}

pub(super) async fn run(context: &Arc<Context>, args: Auth) -> Result<Report, Error> {
    match args.action {
        None => status(context).await,
        Some(AuthAction::Login(login)) => {
            let account = Account::parse(&login.account, login.host.as_deref())?;
            sign_in(context, &account, &login).await.map(Report::Login)
        }
        Some(AuthAction::Logout(logout)) => sign_out(context, &logout),
    }
}

async fn status(context: &Arc<Context>) -> Result<Report, Error> {
    let output = &context.output;
    let mut accounts = Vec::new();
    for account in auth::listed(&auth::env) {
        let found = account.find(&auth::env);
        let (user, valid) = match &found {
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
                    output.step("State in", &text);
                }
            }
            None => output.detail(&format!("{name}: not State in")),
        }
        accounts.push(State {
            account: account.id(),
            signed_in: found.is_some(),
            from: found.as_ref().map(|found| found.origin.to_string()),
            user,
            valid,
        });
    }
    if !accounts.iter().any(|state| state.signed_in) {
        output.hint("public data needs no account. to sign in, run `fetchloom auth login hf`, or another source");
    }
    Ok(Report::Status { accounts })
}

/// signs in to `account`: shows where to get a key, takes it, checks it with the source, and
/// saves it where the source's own tools read it, once the person agrees.
async fn sign_in(
    context: &Arc<Context>,
    account: &Account,
    login: &Login,
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
                "pipe the key in instead: `{} --with-token`",
                account.command()
            )));
        }
        if let Some(found) = account.find(&auth::env) {
            output.step(
                "Found",
                &format!("a {} key in {}", title(account), found.origin),
            );
            if output.ask_yes_no("replace it?", false)? != Some(true) {
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
        return Err(Error::new(
            Kind::AuthRequired,
            format!("{} did not accept this key", title(account)),
        )
        .help("check that you copied all of it and that it has not expired, then sign in again"));
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
    let place = saved_to.as_ref().map_or_else(
        || "for this command only".to_owned(),
        |to| format!("Outcome to {to}"),
    );
    let who = user
        .as_ref()
        .map_or_else(String::new, |user| format!(" as {user}"));
    output.done(
        "State in",
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

/// the steps to get a key, with the page as a link a person can click.
fn guide(context: &Context, account: &Account) {
    let output = &context.output;
    let (page, steps) = account.guide();
    output.say(&format!("{:>11} {}", "Sign in", title(account)));
    let mut number = 1;
    if let Some(page) = page {
        output.say(&format!(
            "{:>11} open {}",
            format!("{number}."),
            output.link(&page)
        ));
        number += 1;
    }
    for step in steps {
        output.say(&format!("{:>11} {step}", format!("{number}.")));
        number += 1;
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
                .filter(|text| !text.is_empty())
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
    let text = text.trim().to_owned();
    if text.is_empty() {
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
            Ok((password.to_owned(), Some(login.to_owned())))
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
        Ok(output.ask_yes_no(&question, default)? == Some(true))
    };
    let failed =
        |err: std::io::Error| Error::io(format!("could not save the {} key", title(account)), &err);
    match account.home(var, secret) {
        Home::File(path) => {
            let readers = account.shared_with().unwrap_or("its own tools");
            if !agrees(
                format!("save it to {}, where {readers} read it?", path.display()),
                true,
            )? {
                return Ok(None);
            }
            let text = if path.file_name().is_some_and(|name| name == "kaggle.json") {
                kaggle_json(secret)?
            } else {
                format!("{secret}\n")
            };
            store::write_private(&path, text.as_bytes()).map_err(failed)?;
            Ok(Some(path.display().to_string()))
        }
        Home::Netrc(path) => {
            if !agrees(
                format!(
                    "save it to {}, where curl and wget read it too?",
                    path.display()
                ),
                true,
            )? {
                return Ok(None);
            }
            let Account::Login(host) = account else {
                return Ok(None);
            };
            auth::netrc::save(&path, host, login.unwrap_or_default(), secret).map_err(failed)?;
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
                    "this machine has no credential store, so the key is Outcome in {}, readable only by you",
                    file.display()
                ));
            } else if !agrees(
                format!(
                    "this machine has no credential store. save it in {}, readable only by you?",
                    file.display()
                ),
                false,
            )? {
                output.hint(&format!(
                    "to keep it out of files, set {} in your environment instead",
                    variable(account)
                ));
                return Ok(None);
            }
            store::save(var, &id, secret, store::Place::File).map_err(failed)?;
            Ok(Some(file.display().to_string()))
        }
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
        output.done("Ok", &format!("no {} key was Outcome", title(&account)));
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
                let question = format!("{tools} read {} too. remove it?", path.display());
                if output.ask_yes_no(&question, false)? == Some(false) {
                    return Ok(Report::Logout {
                        account: account.id(),
                        removed_from: None,
                    });
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
        "State out",
        &format!("of {} \u{b7} removed from {removed}", title(&account)),
    );
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
    let has_key = account.find(&auth::env).is_some();
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
        return Ok(output.ask_yes_no("try again now?", true)? == Some(true));
    }
    if !output
        .ask_yes_no(&format!("sign in to {} now?", title(account)), true)?
        .unwrap_or(false)
    {
        return Ok(false);
    }
    if let Some(terms) = &need.terms {
        output.say(&format!(
            "{:>11} accept the terms at {} while State in",
            "First",
            output.link(terms)
        ));
    }
    let login = Login {
        account: account.id(),
        host: None,
        with_token: false,
    };
    sign_in(context, account, &login).await?;
    Ok(true)
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
