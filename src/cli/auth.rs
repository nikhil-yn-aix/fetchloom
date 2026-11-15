//! `auth`: which accounts are signed in, signing in once with a key shared with each source's own
//! tools, and asking for that key when a download is refused.

use std::io::{IsTerminal as _, Read as _};
use std::sync::Arc;

use serde::Serialize;

use super::Context;
use super::args::{Auth, AuthAction, Login, Logout};
use crate::account::Account;
use crate::auth::{self, Found, Home, Origin, Secret, Step, store};
use crate::error::{Error, Kind, Need};
use crate::net::Api;
use crate::output::Warning;
use crate::units;

mod site;

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

/// one account in `auth`.
#[derive(Serialize)]
pub(super) struct State {
    account: String,
    signed_in: bool,
    from: Option<String>,
    /// who the key belongs to, when the source says.
    user: Option<String>,
    /// whether the source accepted the key: none when it was not asked.
    valid: Option<bool>,
    /// the key's kind: `keys`, `credential_process`, `sso`, `role`, `service_account`, `sas`,
    /// `authorized_user`, `key`, `az_cli`, `managed_identity`, `token`, `header` or `cookies`.
    kind: Option<&'static str>,
    /// when the soonest of a website's saved cookies expires, left out of every other row.
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<String>,
}

impl State {
    /// an account with no key on this machine.
    fn none(account: &str) -> State {
        State {
            account: account.to_owned(),
            signed_in: false,
            from: None,
            user: None,
            valid: None,
            kind: None,
            expires: None,
        }
    }
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
        None => status(context).await,
        Some(AuthAction::Login(login)) if site::wanted(&login) => {
            site::sign_in(context, &login).map(Report::Login)
        }
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
    if let Some(path) =
        auth::netrc::path(&crate::machine::env).filter(|path| auth::netrc::too_open(path))
    {
        output.warn_with(
            Warning::AuthReadable,
            &format!(
                "other users of this machine can open {}, which holds logins",
                path.display()
            ),
            &format!("limit it to yourself with `chmod 600 {}`", path.display()),
        );
    }
    if let Some(error) = store::unreadable(&crate::machine::env) {
        unusable(output, &error);
    }
    tell_kept_out(output, &crate::machine::env);
    if context.mirrors.mirrors.only {
        output.hint(
            "mirrors_only lets fetchloom ask its mirrors alone, so no key is checked with its site",
        );
    }
    for account in auth::listed(&crate::machine::env) {
        let found = account.find(&crate::machine::env);
        let (user, valid, unanswered) = match &found {
            // asking with a key that is never sent would only say the source refused it
            Some(found) if !account.sendable(found) => {
                warn_unsendable(output, &account, found);
                (None, None, None)
            }
            Some(found) => asked_about(context, &account, found, true).await,
            None => (None, None, None),
        };
        let name = login_name(&account);
        match &found {
            Some(found) => {
                let who = user
                    .as_ref()
                    .map_or_else(String::new, |user| format!(" as {user}"));
                let text = format!("to {name}{who} \u{b7} {}", found.origin.shown());
                if valid == Some(false) {
                    output.mark(
                        "Refused",
                        &format!(
                            "{} for {name}; {}",
                            found.origin.named(),
                            replacing(&account, &found.origin)
                        ),
                    );
                } else if let Some(host) = &unanswered {
                    output.step(
                        "Signed in",
                        &format!("{text}, not checked, as {host} gave no answer"),
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
            kind: None,
            expires: None,
        });
    }
    accounts.extend(site::listed(output));
    accounts.extend(aws_keys(output, &mut missing));
    accounts.push(google_credentials(output, &mut missing));
    accounts.extend(azure_credentials(
        output,
        &mut missing,
        &context.settings.azure_containers.value,
    ));
    if !missing.is_empty() {
        output.faint_step("Signed in", &format!("to none of {}", missing.join(", ")));
    }
    if !accounts.iter().any(|state| state.signed_in) {
        output.hint("public data needs no account. to sign in, run `fetchloom auth login hf`, or another source");
    }
    Ok(Report::Status { accounts })
}

/// the rows of the AWS keys buckets are signed with, or the settings' S3 endpoint's instead, naming
/// but never running a key program and asking nothing; unusable settings are warned of.
fn aws_keys(output: &crate::output::Output, missing: &mut Vec<String>) -> Vec<State> {
    use crate::auth::aws::How;
    let described = auth::aws::describe(&crate::machine::env).unwrap_or_else(|error| {
        unusable(output, &error);
        None
    });
    let Some(described) = described else {
        missing.push("aws".to_owned());
        return vec![State::none("aws")];
    };
    let mut rows = Vec::new();
    let (account, name) = match &described.endpoint {
        Some(endpoint) => {
            missing.push("aws".to_owned());
            rows.push(State::none("aws"));
            let endpoint = endpoint.ascii_serialization();
            (format!("s3:{endpoint}"), format!("s3 at {endpoint}"))
        }
        None => ("aws".to_owned(), "aws".to_owned()),
    };
    let shown = &described.shown;
    let later = "when a bucket needs keys";
    output.step(
        "Signed in",
        &match &described.how {
            How::Keys(key) => format!("to {name} with key {key} \u{b7} {shown}"),
            How::Process => format!("to {name} \u{b7} {shown}, which runs {later}"),
            How::Cli { signs_in: true } => {
                format!("to {name} \u{b7} {shown}, which signs in to IAM identity center {later}")
            }
            How::Cli { signs_in: false } => {
                format!("to {name} \u{b7} {shown}, which takes on a role {later}")
            }
        },
    );
    rows.push(State {
        account,
        signed_in: true,
        from: Some(described.origin),
        user: None,
        valid: None,
        kind: Some(described.how.name()),
        expires: None,
    });
    rows
}

/// the row of google's application default credentials, which buckets ask a token with: the
/// file, and a service account's email or a person's login. nothing is asked.
fn google_credentials(output: &crate::output::Output, missing: &mut Vec<String>) -> State {
    let found = auth::google::find(&crate::machine::env).unwrap_or_else(|error| {
        unusable(output, &error);
        None
    });
    let Some(credentials) = found else {
        missing.push("google".to_owned());
        return State::none("google");
    };
    let (path, email) = credentials.described();
    let file = units::path(path);
    output.step(
        "Signed in",
        &match email {
            Some(email) => format!("to google as {email} \u{b7} {file}"),
            None => format!("to google with a person's login \u{b7} {file}"),
        },
    );
    State {
        account: "google".to_owned(),
        signed_in: true,
        from: Some(path.display().to_string()),
        user: email.map(str::to_owned),
        valid: None,
        kind: Some(if email.is_some() {
            "service_account"
        } else {
            "authorized_user"
        }),
        expires: None,
    }
}

/// the rows of azure's credentials: a SAS or key kept to one account, as `azure:<account>`, then
/// `azure` for the others. nothing runs and nothing is asked; unusable settings are warned of.
fn azure_credentials(
    output: &crate::output::Output,
    missing: &mut Vec<String>,
    listed: &[crate::auth::azure::AzureContainer],
) -> Vec<State> {
    use crate::auth::azure::{Kept, Rest};
    let described = match auth::azure::describe(&crate::machine::env, listed) {
        Ok(described) => described,
        Err(error) => {
            unusable(output, &error);
            missing.push("azure".to_owned());
            return vec![State::none("azure")];
        }
    };
    let signed_in = |account: String, from: String, user: Option<String>, kind| State {
        account,
        signed_in: true,
        from: Some(from),
        user,
        valid: None,
        kind: Some(kind),
        expires: None,
    };
    let mut rows = Vec::new();
    if let Some((account, kept)) = described.kept {
        let (from, with, kind) = match kept {
            Kept::Sas => ("AZURE_STORAGE_SAS_TOKEN", "a SAS", "sas"),
            Kept::Key => ("AZURE_STORAGE_KEY", "its key", "key"),
        };
        output.step(
            "Signed in",
            &format!("to azure account {account} with {with} \u{b7} {from}"),
        );
        rows.push(signed_in(
            format!("azure:{account}"),
            from.to_owned(),
            None,
            kind,
        ));
    }
    let later = "when a container needs a token";
    let rest = described.rest.unwrap_or_else(|error| {
        unusable(output, &error);
        Rest::None
    });
    let azure = "azure".to_owned();
    match rest {
        Rest::None => {
            missing.push(azure);
            rows.push(State::none("azure"));
        }
        Rest::Sas => {
            let from = "AZURE_STORAGE_SAS_TOKEN";
            output.step("Signed in", &format!("to azure with a SAS \u{b7} {from}"));
            rows.push(signed_in(azure, from.to_owned(), None, "sas"));
        }
        Rest::Cli { path, user } => {
            let who = user
                .as_ref()
                .map_or_else(String::new, |user| format!(" as {user}"));
            output.step(
                "Signed in",
                &format!(
                    "to azure{who} \u{b7} the az cli at {}, which runs {later}",
                    units::path(&path)
                ),
            );
            rows.push(signed_in(azure, path.display().to_string(), user, "az_cli"));
        }
        Rest::Managed { client_id } => {
            let from = "AZURE_TOKEN_CREDENTIALS";
            let who = client_id.as_ref().map_or_else(
                || "this machine's managed identity".to_owned(),
                |id| format!("managed identity {id}"),
            );
            output.step(
                "Signed in",
                &format!("to azure as {who} \u{b7} {from}, asked {later}"),
            );
            rows.push(signed_in(
                azure,
                from.to_owned(),
                client_id,
                "managed_identity",
            ));
        }
    }
    rows
}

/// warns of keys or credentials set up in a way fetchloom cannot use, as `error` says.
fn unusable(output: &crate::output::Output, error: &Error) {
    output.warn_detailed(
        Warning::AuthUnusable,
        &error.message,
        error.help.as_deref(),
        std::iter::empty::<&str>(),
    );
}

/// signs in to `account`, saving the checked key where the source's own tools read it once the
/// person agrees; with `rerun` the command runs again after, which an unsaved key still serves.
async fn sign_in(
    context: &Arc<Context>,
    account: &Account,
    login: &Login,
    rerun: bool,
) -> Result<Outcome, Error> {
    let output = &context.output;
    let (secret, user_name) = if login.with_token {
        read_stdin(account, output)?
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
        if let Some(found) = account.find(&crate::machine::env) {
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
    // a host only a refusal named, as a project's data.toml can, is not trusted as one typed
    let (user, valid, unanswered) = asked_about(context, account, &found, !rerun).await;
    if valid == Some(false) {
        return Err(refused(account));
    }
    if valid == Some(true) {
        let who = user
            .as_ref()
            .map_or_else(String::new, |user| format!(" for {user}"));
        output.step("Checked", &format!("the key works{who}"));
    } else if context.mirrors.mirrors.only {
        output.hint("mirrors_only lets fetchloom ask its mirrors alone, so the key was not checked with its site");
    }
    // the key checked out, so a ctrl-c at the question below cancels it and any other stop lets it
    // finish; a sign-in after a refusal runs its command again, which a stop must still reach
    if !rerun {
        context.stop.finish();
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
    told_signed_in(
        output,
        account,
        user.as_deref(),
        saved_to.as_deref(),
        unanswered.as_deref(),
    );
    if saved_to.is_some()
        && let Some(told) = account.told_shared()
    {
        output.hint(&told);
    }
    Ok(Outcome {
        account: account.id(),
        user,
        saved_to,
    })
}

/// the line that ends a sign-in: who signed in and where the key went, or, when the source was
/// asked and gave no answer from `unanswered`, that the key was kept without its check.
fn told_signed_in(
    output: &crate::output::Output,
    account: &Account,
    user: Option<&str>,
    saved_to: Option<&str>,
    unanswered: Option<&str>,
) {
    if let Some(host) = unanswered {
        let told = match saved_to {
            Some(to) => format!(
                "a key for {} to {}, not checked, as {host} gave no answer",
                title(account),
                units::path(std::path::Path::new(to))
            ),
            None => format!(
                "a key for {} for this command alone, not checked, as {host} gave no answer",
                title(account)
            ),
        };
        output.done(if saved_to.is_some() { "Saved" } else { "Kept" }, &told);
        output.hint(&format!("`fetchloom auth` checks it once {host} answers"));
        return;
    }
    let place = saved_to.map_or_else(
        || "not saved, so it ends with this command".to_owned(),
        |to| format!("saved to {}", units::path(std::path::Path::new(to))),
    );
    let who = user.map_or_else(String::new, |user| format!(" as {user}"));
    output.done(
        "Signed in",
        &format!("to {}{who} \u{b7} {place}", login_name(account)),
    );
}

/// what a no while signing in leaves: with `rerun` the command goes on with the key it has, while
/// after `auth login`, where nothing uses the key, it is a cancel that changed nothing.
fn after_no(rerun: bool) -> Result<(), Error> {
    if rerun {
        return Ok(());
    }
    Err(answered_no())
}

/// a no to a key question: a cancel of what its caller names that, like ctrl-c, changed nothing.
fn answered_no() -> Error {
    Error::new(Kind::Unconfirmed, "answered no").cancelled("the question")
}

/// the error once a prompt got nothing three times, which cancels the question as a no does: a
/// person who stops pasting sends no ctrl-c.
fn nothing_pasted() -> Error {
    Error::new(Kind::Unconfirmed, "nothing was pasted").cancelled("the question")
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
    match account.secret() {
        Secret::Password => {
            let login = output
                .ask_text("username", false)?
                .filter(|text| !text.is_empty())
                .ok_or_else(nothing_pasted)?;
            let password = output
                .ask_text("password", true)?
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(nothing_pasted)?;
            Ok((password, Some(login)))
        }
        Secret::Token => {
            let pasted = output
                .ask_text("key", true)?
                .filter(|text| !text.is_empty())
                .ok_or_else(nothing_pasted)?;
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

fn read_stdin(
    account: &Account,
    output: &crate::output::Output,
) -> Result<(String, Option<String>), Error> {
    let text = if std::io::stdin().is_terminal() {
        // a key typed or pasted at a terminal stays hidden, as an interactive sign-in keeps it
        let prompt = match account.secret() {
            Secret::Password => "login",
            Secret::Token => "key",
        };
        output.read_hidden(prompt)?
    } else {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| Error::io("could not read the key from stdin", &err))?;
        text
    };
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

/// what the source said of `found`: whose key it is and whether it works, as [`check`] tells, and
/// the host that was asked and said nothing either way, as one this machine cannot reach. an
/// offline run, or one that may ask only mirrors, asks nothing.
async fn asked_about(
    context: &Arc<Context>,
    account: &Account,
    found: &Found,
    chosen: bool,
) -> (Option<String>, Option<bool>, Option<String>) {
    if context.offline.enabled() || context.mirrors.mirrors.only {
        return (None, None, None);
    }
    let (user, valid) = check(context, account, found, chosen).await;
    let unanswered = valid
        .is_none()
        .then(|| {
            account
                .check()
                .filter(|_| account.credential(found).is_some())
        })
        .flatten()
        .and_then(|check| check.url.host_str().map(str::to_owned));
    (user, valid, unanswered)
}

/// asks the source who the key belongs to: the name and whether it was accepted, or nothing
/// when the source has no such check or could not be reached. `chosen` says the person named the
/// account's host, as `--host` does, rather than a refusal.
async fn check(
    context: &Arc<Context>,
    account: &Account,
    found: &Found,
    chosen: bool,
) -> (Option<String>, Option<bool>) {
    let Some(check) = account.check() else {
        return (None, None);
    };
    let Some(credential) = account.credential(found) else {
        return (None, None);
    };
    let net = context.net_with(vec![credential]);
    // a dataverse host the person named, through --host or their own earlier sign-in, may be on
    // a private network, as `add` allows. the origin asked is trusted, and no other
    if chosen && let Account::Dataverse(_) = account {
        net.trust([check.url.origin()]);
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

/// asks `question`, unless `--yes` answered it, as it answers every question a person could have
/// been asked. nobody to ask still answers none.
fn agree(context: &Context, question: &str, default: bool) -> Result<Option<bool>, Error> {
    if context.yes && context.output.can_ask() {
        return Ok(Some(true));
    }
    context.output.ask_yes_no(question, default)
}

/// saves the key where it belongs, asking first. returns where it went, or none when it was kept
/// for this command only.
fn save(
    context: &Context,
    account: &Account,
    secret: &str,
    login: Option<&str>,
    told: bool,
) -> Result<Option<String>, Error> {
    let output = &context.output;
    let var = &crate::machine::env;
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
            let (question, default) = save_question(account, &path, false, Privacy::of(var, &path));
            if !agrees(question, default)? {
                return Ok(None);
            }
            let text = if path.file_name().is_some_and(|name| name == "kaggle.json") {
                kaggle_json(secret)?
            } else {
                format!("{secret}\n")
            };
            store::write_private(&path, text.as_bytes()).map_err(failed)?;
            warn_if_shared(output, &path, PROFILE_OR_PERMISSIONS);
            Ok(Some(path.display().to_string()))
        }
        Home::Netrc(path) => {
            let (question, default) = save_question(account, &path, false, Privacy::of(var, &path));
            if !agrees(question, default)? {
                return Ok(None);
            }
            let Account::Login(host) = account else {
                return Ok(None);
            };
            auth::netrc::save(var, &path, host, login.unwrap_or_default(), secret)
                .map_err(failed)?;
            warn_if_shared(output, &path, PROFILE_OR_PERMISSIONS);
            Ok(Some(path.display().to_string()))
        }
        Home::Store => {
            let id = account.id();
            if store::KEYRING.usable(var) {
                if !agrees(format!("save it in {}?", auth::keyring_name()), true)? {
                    return Ok(None);
                }
                asking_store(output, || {
                    store::save(var, &id, secret, store::Place::Keyring)
                })
                .map_err(|err| {
                    let locked = store::locked(&err);
                    failed(err).help(elsewhere("the key", locked))
                })?;
                return Ok(Some(auth::keyring_name().to_owned()));
            }
            let file = crate::machine::settings_folder(var)
                .unwrap_or_default()
                .join("credentials.json");
            if !told {
                let (question, default) = save_question(
                    account,
                    &file,
                    !store::keyring_allowed(var),
                    Privacy::of(var, &file),
                );
                if !agrees(question, default)? {
                    output.hint(&format!(
                        "to keep it out of files, set {} in your environment instead",
                        variable(account)
                    ));
                    return Ok(None);
                }
            }
            let left = store::kept_out(var).contains(&id);
            store::save(var, &id, secret, store::Place::File).map_err(failed)?;
            if left {
                tell_left_in_store(output, &format!("the {} key", title(account)));
            }
            if told {
                output.warn(
                    Warning::AuthNoKeyring,
                    &format!(
                        "{}, so the key is saved in {}{}",
                        no_store(var),
                        units::path(&file),
                        Privacy::of(var, &file).saved()
                    ),
                );
            }
            warn_if_shared(output, &file, KEYRING_OR_PROFILE);
            Ok(Some(file.display().to_string()))
        }
    }
}

/// how fetchloom's own key file is kept from other users of this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Privacy {
    /// unix makes the file its owner's alone.
    Owner,
    /// on windows a file takes its folder's permissions, which inside the user profile are the
    /// user's own.
    Profile,
    /// on windows outside the profile, where its folder's permissions may let others read it.
    Open,
}

impl Privacy {
    /// how the key file at `file` is kept: fetchloom changes no windows permissions, since that
    /// takes unsafe code or a program that rewrites them, as ransomware does.
    pub(super) fn of(var: crate::machine::Var<'_>, file: &std::path::Path) -> Privacy {
        if !cfg!(windows) {
            Privacy::Owner
        } else if crate::machine::in_profile(var, file) {
            Privacy::Profile
        } else {
            Privacy::Open
        }
    }

    /// what a line says of the file after its name, once keys are saved there; none outside the
    /// profile, where [`warn_if_shared`] says who may read it.
    pub(super) fn saved(self) -> &'static str {
        match self {
            Privacy::Owner => ", readable only by you",
            Privacy::Profile => ", protected by your user profile's permissions",
            Privacy::Open => "",
        }
    }

    /// the file as the question before saving names it, short enough for one terminal line.
    pub(super) fn file(self) -> &'static str {
        match self {
            Privacy::Owner => "a file only you can read",
            Privacy::Profile => "a file your user profile protects",
            Privacy::Open => "a file other users here may read",
        }
    }
}

/// the help when fetchloom's own key file is outside the windows user profile.
pub(super) const KEYRING_OR_PROFILE: &str = "to keep it private, leave FETCHLOOM_KEYRING on, so windows credential manager keeps it, or keep APPDATA inside your user profile";

/// where the key of `account` is kept: where it is found, or where the index of saved keys still
/// lists one the store would not give, as a locked keyring keeps it. none when nothing is.
fn kept(account: &Account, var: crate::machine::Var<'_>) -> Option<Origin> {
    account.find(var).map(|found| found.origin).or_else(|| {
        let place = *store::saved(var).get(&account.id())?;
        Some(Origin::saved(var, place))
    })
}

/// the error for a key of `typed`, an account as typed, that `doing` could not remove for `err`;
/// a locked credential store's says to unlock it first.
fn not_removed(doing: &str, typed: &str, err: &std::io::Error) -> Error {
    let error = Error::io(doing, err);
    if store::locked(err) {
        error.help(format!(
            "unlock {}, then run `fetchloom auth logout {}` again",
            auth::keyring_name(),
            crate::suggested::shell_word(typed)
        ))
    } else {
        error
    }
}

/// the help when the credential store would not take `it`: to keep it in fetchloom's own file
/// instead, after unlocking the store when it is `locked`. only unix makes that file its owner's
/// alone; on windows it takes its folder's permissions, which the save then tells.
fn elsewhere(it: &str, locked: bool) -> String {
    let only = if cfg!(windows) {
        ""
    } else {
        ", readable only by you"
    };
    let file = format!(
        "set FETCHLOOM_KEYRING=false to keep {it} in fetchloom's own file{only}, then run it again"
    );
    if !locked {
        return file;
    }
    // macos's own command asks for the keychain's password in the terminal, over ssh too
    let unlock = if cfg!(target_os = "macos") {
        "unlock the macos keychain with `security unlock-keychain`".to_owned()
    } else {
        format!("unlock {}", auth::keyring_name())
    };
    format!("{unlock}, or {file}")
}

/// what a run waits for while the credential store asks for its password.
const PROMPT: &str = if cfg!(target_os = "macos") {
    "for the macos keychain's password prompt"
} else if cfg!(windows) {
    "for windows credential manager"
} else {
    "for the system keyring's password prompt"
};

/// `call`, a request to the credential store, said to wait for the store's password prompt once
/// it lasts a second: a locked macos keychain asks in a window that may be behind others.
pub(super) fn asking_store<T>(output: &crate::output::Output, call: impl FnOnce() -> T) -> T {
    let _waiting = output.setup("Waiting", PROMPT);
    call()
}

/// the question before saving `account`'s key to the file `path`, and the answer enter gives:
/// yes for a file the source's own tools read, no for the file keys go in without a credential
/// store, which `off` says `FETCHLOOM_KEYRING` turned off and `privacy` says how it is kept. the
/// tools are named once the key is saved, so the question fits a terminal.
fn save_question(
    account: &Account,
    path: &std::path::Path,
    off: bool,
    privacy: Privacy,
) -> (String, bool) {
    let save = format!("save it to {}?", units::path(path));
    if account.shared_with().is_some() || matches!(account, Account::Login(_)) {
        (save, true)
    } else {
        let why = if off {
            "FETCHLOOM_KEYRING is off"
        } else {
            "no credential store here"
        };
        (
            format!("{why}, so it would go in {}\n{save}", privacy.file()),
            false,
        )
    }
}

/// why a key goes in fetchloom's own file, as a warning begins: `FETCHLOOM_KEYRING` turned the
/// system's credential store off, or the machine has none.
pub(super) fn no_store(var: crate::machine::Var<'_>) -> &'static str {
    if store::keyring_allowed(var) {
        "this machine has no credential store"
    } else {
        "FETCHLOOM_KEYRING is off"
    }
}

/// the hint for keys the index lists in the credential store while `FETCHLOOM_KEYRING` keeps
/// fetchloom out of it, which read as none.
pub(super) fn tell_kept_out(output: &crate::output::Output, var: crate::machine::Var<'_>) {
    let names: Vec<String> = store::kept_out(var)
        .iter()
        .map(|id| {
            id.strip_prefix("site:")
                .or_else(|| id.strip_prefix("dataverse:"))
                .unwrap_or(id)
                .to_owned()
        })
        .collect();
    let (keys, them) = match names.len() {
        0 => return,
        1 => ("the key", "it"),
        _ => ("the keys", "them"),
    };
    output.hint(&format!(
        "FETCHLOOM_KEYRING is off, so fetchloom leaves out {keys} for {} in {}; unset FETCHLOOM_KEYRING to use {them}",
        units::listed(&names, "and"),
        auth::keyring_name()
    ));
}

/// the refusal to remove `what`, a key kept in the credential store while `FETCHLOOM_KEYRING`
/// keeps fetchloom out of it, which `typed`, the account as typed, signs out of.
pub(super) fn left_in_store(what: &str, typed: &str) -> Error {
    Error::new(
        Kind::Usage,
        format!(
            "{what} is in {}, which fetchloom leaves alone while FETCHLOOM_KEYRING is off",
            auth::keyring_name()
        ),
    )
    .help(format!(
        "unset FETCHLOOM_KEYRING, then run `fetchloom auth logout {}` again",
        crate::suggested::shell_word(typed)
    ))
}

/// the hint once a key is saved in fetchloom's own file in place of one in the credential store,
/// which `FETCHLOOM_KEYRING` keeps fetchloom from removing.
pub(super) fn tell_left_in_store(output: &crate::output::Output, what: &str) {
    output.hint(&format!(
        "{what} saved before stays in {}, since FETCHLOOM_KEYRING is off",
        auth::keyring_name()
    ));
}

/// the question before removing a key from `path`, a file `tools` read too.
fn remove_question(tools: &str, path: &std::path::Path) -> String {
    format!(
        "{tools} use this key too\nremove it from {}?",
        units::path(path)
    )
}

/// on windows a saved file takes its folder's permissions. the profile keeps other users out, but
/// a folder elsewhere, such as an `HF_HOME` on another drive, may let them read the key, which a
/// warning with `help` says.
fn warn_if_shared(output: &crate::output::Output, path: &std::path::Path, help: &str) {
    if cfg!(windows) && !crate::machine::in_profile(&crate::machine::env, path) {
        output.warn_with(
            Warning::AuthReadable,
            &format!(
                "{} is outside your user profile, so other users of this machine may be able to read it",
                path.display()
            ),
            help,
        );
    }
}

/// the help when a source's own key file is outside the windows user profile.
const PROFILE_OR_PERMISSIONS: &str =
    "keep the key in your user profile, or limit the folder's permissions";

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
    if let Some(report) = site::sign_out(context, logout)? {
        return Ok(report);
    }
    let output = &context.output;
    let account = Account::parse(&logout.account, logout.host.as_deref())?;
    let var = &crate::machine::env;
    let Some(origin) = kept(&account, var) else {
        output.done("Ok", &format!("no {} key was saved", title(&account)));
        return Ok(Report::Logout {
            account: account.id(),
            removed_from: None,
        });
    };
    // a sign-out has nothing to stop, one key removed after a question at most, which a ctrl-c
    // there cancels as a key, so a stop asked otherwise lets it finish
    context.stop.finish();
    let failed = |err: std::io::Error| {
        let doing = format!("could not remove the {} key", title(&account));
        not_removed(&doing, &logout.account, &err)
    };
    let removed = match &origin {
        Origin::Variable(name) => {
            return Err(Error::new(
                Kind::Usage,
                format!("the {} key comes from {name}", title(&account)),
            )
            .help(format!("unset {name} in your environment to sign out")));
        }
        Origin::Keyring if !store::keyring_allowed(var) => {
            let what = format!("the {} key", title(&account));
            return Err(left_in_store(&what, &logout.account));
        }
        Origin::Keyring => {
            asking_store(output, || store::forget(var, &account.id())).map_err(failed)?;
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
                            Kind::Unconfirmed,
                            format!(
                                "{tools} read {} too, and fetchloom cannot ask you here whether to sign them out",
                                path.display()
                            ),
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
                    auth::netrc::remove(var, path, host).map_err(failed)?;
                }
                _ if store::saved(var).contains_key(&account.id()) => {
                    asking_store(output, || store::forget(var, &account.id())).map_err(failed)?;
                }
                _ => std::fs::remove_file(path).map_err(failed)?,
            }
            path.display().to_string()
        }
    };
    output.done(
        "Signed out",
        &format!(
            "of {} \u{b7} removed from {}",
            login_name(&account),
            units::path(std::path::Path::new(&removed))
        ),
    );
    // another configured location may still supply a key for the same source
    if let Some(left) = account.find(var) {
        output.warn_with(
            Warning::AuthKeyRemains,
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

/// offers a person at the terminal, after a command failed for want of an account, to sign in or
/// accept a gated dataset's terms, then says whether running it again could work. `refusal`, the
/// command's error when not told yet, gets a line first, and a no then ends the run as a cancel.
pub(super) async fn offer(
    context: &Arc<Context>,
    need: &Need,
    refusal: Option<&Error>,
) -> Result<bool, Error> {
    let output = &context.output;
    if let Some(refusal) = refusal {
        output.mark("Refused", &refusal.message);
        for detail in &refusal.details {
            output.item(detail);
        }
    }
    let declined = |what: &str| match refusal {
        Some(_) => Err(answered_no().cancelling(what)),
        None => Ok(false),
    };
    let account = &need.account;
    let found = account.find(&crate::machine::env);
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
        return match again {
            Some(true) => Ok(true),
            _ => declined("trying again"),
        };
    }
    let signing = format!("signing in to {}", title(account));
    // a reflex enter starts no sign-in, which cannot help when the name is wrong or terms come
    // first, so what else may be wrong is said before the question
    if let Some(page) = &need.maybe_missing {
        output.say(&format!(
            "{:>11} the name first at {}",
            "Check",
            output.link(page)
        ));
        output.item(&format!(
            "{} answers alike for a name that does not exist and for a private one",
            title(account)
        ));
    }
    if let Some(terms) = &need.terms {
        output.say(&format!(
            "{:>11} accept the terms at {} while signed in there",
            "First",
            output.link(terms)
        ));
    }
    if let Account::Dataverse(host) = account
        && named_here(account)
    {
        output.say(&format!(
            "{:>11} that you trust {host}: this project's data.toml named it",
            "Check"
        ));
    }
    if need.site {
        let signed = site::offered(context, account)
            .await
            .map_err(|error| error.cancelling(&signing))?;
        return if signed { Ok(true) } else { declined(&signing) };
    }
    if !agree(
        context,
        &format!("sign in to {} now?", title(account)),
        false,
    )
    .map_err(|error| error.cancelling(&signing))?
    .unwrap_or(false)
    {
        return declined(&signing);
    }
    let login = Login {
        account: account.id(),
        ..Login::default()
    };
    sign_in(context, account, &login, true)
        .await
        .map_err(|error| error.cancelling(signing))?;
    Ok(true)
}

/// whether the data.toml of the project here names a dataset on the dataverse `account` signs in
/// to, so the host a refusal offers a sign-in for came from a file, not from the person.
fn named_here(account: &Account) -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|here| crate::project::Project::find(&here))
        .and_then(|project| project.manifest().ok())
        .is_some_and(|(_, manifest)| names(&manifest, account))
}

/// whether `manifest` names a dataset on the dataverse `account` signs in to.
fn names(manifest: &crate::project::manifest::Manifest, account: &Account) -> bool {
    use crate::source::Reference;
    manifest.datasets.values().any(|entry| {
        let crate::project::manifest::Origin::Source(source) = &entry.origin else {
            return false;
        };
        Reference::parse(&source.reference).is_ok_and(|parsed| {
            matches!(&parsed.reference, Reference::Dataverse(dataset) if dataset.account() == *account)
        })
    })
}

/// how to replace a key the source refused: a variable wins over the key a sign-in saves, so a
/// sign-in helps only once the variable is unset.
fn replacing(account: &Account, origin: &Origin) -> String {
    match origin {
        Origin::Variable(name) => format!(
            "set {name} to a current key, or unset {name} to use the key `{}` saves",
            account.command()
        ),
        _ => format!("sign in again with `{}`", account.command()),
    }
}

/// says a key was found but no request can carry it, naming where it is but never the key.
fn warn_unsendable(output: &crate::output::Output, account: &Account, found: &Found) {
    let fix = match &found.origin {
        Origin::Variable(name) => format!("set {name} to the key alone"),
        _ => format!("sign in again with `{}`", account.command()),
    };
    output.warn_with(
        Warning::AuthKeyUnsendable,
        &format!(
            "the {} key in {} is never sent, since it holds a character no request header can carry, such as a line break",
            title(account),
            found.origin,
        ),
        &fix,
    );
}

/// the account as `fetchloom auth login` takes it, such as hf, so the listing names what to type.
/// a name no command on windows holds safely shows as it is: a listing runs nothing.
fn login_name(account: &Account) -> String {
    match account {
        Account::Dataverse(_) => title(account),
        _ => crate::suggested::unmarked(&account.command())
            .trim_start_matches("fetchloom auth login ")
            .to_owned(),
    }
}

fn title(account: &Account) -> String {
    match account {
        Account::HuggingFace => "hugging face".to_owned(),
        Account::Kaggle => "kaggle".to_owned(),
        Account::Zenodo => "zenodo".to_owned(),
        Account::Figshare => "figshare".to_owned(),
        Account::Osf => "osf".to_owned(),
        Account::Dryad => "dryad".to_owned(),
        Account::GitHub => "github".to_owned(),
        Account::GitLab => "gitlab".to_owned(),
        Account::S2 => "semantic scholar".to_owned(),
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
        Account::Dryad => "DRYAD_TOKEN",
        Account::GitHub => "GH_TOKEN or GITHUB_TOKEN",
        Account::GitLab => "GITLAB_TOKEN",
        Account::S2 => "S2_API_KEY",
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

    /// kaggle.json pasted at the prompt is kept as kaggle's tools write it; anything else is
    /// refused, saying what kaggle.json needs.
    #[test]
    fn a_pasted_kaggle_json_is_kept_as_kaggle_writes_it() {
        let kept = kaggle_json(r#"{"username": "ada", "key": "0123abcd"}"#).unwrap();
        let read: serde_json::Value = serde_json::from_str(&kept).unwrap();
        assert_eq!(
            (read["username"].as_str(), read["key"].as_str()),
            (Some("ada"), Some("0123abcd"))
        );
        assert!(kept.ends_with('\n'), "{kept}");
        for (pasted, said) in [
            ("KGAT_0123", "that is not a kaggle.json"),
            (
                r#"{"username": "ada"}"#,
                "kaggle.json needs a username and a key",
            ),
        ] {
            let error = kaggle_json(pasted).unwrap_err();
            assert_eq!((error.kind, error.message.as_str()), (Kind::Usage, said));
        }
    }

    fn context() -> Arc<Context> {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        Arc::new(Context::new(Output::new(style, Level::Quiet, false, false)))
    }

    /// a store that is locked names how to unlock it: on macos the command that asks for the
    /// keychain's password in a terminal, over ssh too. the file it offers instead is said to be
    /// the user's alone only where fetchloom makes it so: windows gives it its folder's permissions.
    #[test]
    fn a_locked_store_says_how_to_unlock_it() {
        let file = if cfg!(windows) {
            "set FETCHLOOM_KEYRING=false to keep the key in fetchloom's own file, then run it again"
        } else {
            "set FETCHLOOM_KEYRING=false to keep the key in fetchloom's own file, readable only by you, then run it again"
        };
        let unlock = if cfg!(target_os = "macos") {
            "unlock the macos keychain with `security unlock-keychain`"
        } else if cfg!(windows) {
            "unlock windows credential manager"
        } else {
            "unlock the system keyring"
        };
        assert_eq!(elsewhere("the key", true), format!("{unlock}, or {file}"));
        assert_eq!(elsewhere("the key", false), file);
    }

    /// a call to the credential store that has not returned says what it waits for, as a locked
    /// macos keychain keeps it waiting on a password prompt that may be behind other windows.
    #[test]
    fn a_store_call_that_waits_says_it_waits_for_the_password_prompt() {
        let screen = crate::output::screen::Screen::default();
        let output = crate::output::Output::on_screen(screen.clone());
        let output = &output;
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        // the store answers once released, or once a failed check drops `release`
        std::thread::scope(move |scope| {
            let call = scope.spawn(move || asking_store(output, move || blocked.recv().is_ok()));
            assert!(
                screen.shows(&format!("Waiting {PROMPT}")),
                "{:?}",
                screen.records()
            );
            release.send(()).unwrap();
            assert!(call.join().unwrap(), "the store's answer comes back");
        });
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
        assert_eq!(
            block_on(check(&context, &account, &found, true)),
            (None, None)
        );
        assert!(
            heard.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the key was never checked on the host the person typed"
        );
    }

    /// a dataverse host only a refusal named, as one a project's data.toml names, is no host the
    /// person chose, so a sign-in offered after it never checks the key on a private address.
    #[test]
    fn an_offered_dataverse_sign_in_never_trusts_a_private_host() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let context = context();
        let stop = context.stop.clone();
        let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let heard = Arc::clone(&reached);
        std::thread::spawn(move || {
            if listener.accept().is_ok() {
                heard.store(true, std::sync::atomic::Ordering::SeqCst);
                stop.ask();
            }
        });
        let account = Account::parse("dataverse", Some(&format!("127.0.0.1:{port}"))).unwrap();
        let found = Found {
            origin: Origin::Keyring,
            secret: "key".to_owned(),
            login: None,
        };
        assert_eq!(
            block_on(check(&context, &account, &found, false)),
            (None, None)
        );
        assert!(
            !reached.load(std::sync::atomic::Ordering::SeqCst),
            "the key went to a private host the person never named"
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

    /// a no at `auth login` exits 1 as any no does, since nothing runs after it to use the key.
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
            for privacy in [Privacy::Owner, Privacy::Profile, Privacy::Open] {
                questions.push(save_question(account, path, false, privacy).0);
                questions.push(save_question(account, path, true, privacy).0);
            }
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
