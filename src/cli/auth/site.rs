//! `auth login <host>` with a website's own credential: a token or a header, kept for that host
//! alone, as `auth` lists it and `auth logout` removes it.

use std::io::{IsTerminal as _, Read as _};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Outcome, Report, State, agree, answered_no};
use crate::auth::site::{self, Saved, Taken};
use crate::auth::{self, Account, Origin, store};
use crate::cli::Context;
use crate::cli::args::{Login, Logout};
use crate::error::{Error, Kind};
use crate::output::{Output, Warning};

/// whether `login` saves a website's own credential rather than an account's key.
pub(super) fn wanted(login: &Login) -> bool {
    login.token || login.header.is_some()
}

/// the key a website's sign-in asks for: a token, or the value of a header the site names.
enum Asked {
    Token,
    Header(String),
}

impl Asked {
    /// how the key is asked for, and the option that names it in a command.
    fn named(&self) -> (&str, String) {
        match self {
            Asked::Token => ("token", "--token".to_owned()),
            Asked::Header(name) => (
                name.as_str(),
                format!("--header {}", crate::suggested::shell_word(name)),
            ),
        }
    }

    /// what is saved, once the key is in hand.
    fn saved(self, key: String) -> Saved {
        match self {
            Asked::Token => Saved::Token { token: key },
            Asked::Header(name) => Saved::Header { name, value: key },
        }
    }
}

/// the host `typed` names for a website's credential: a host name with its port when that is
/// not 443, or what a login's name stands for, as earthdata does. an account that takes a key of
/// its own, such as hf, has none.
fn site_host(typed: &str) -> Result<String, Error> {
    match Account::parse(typed, None) {
        Ok(Account::Login(host)) => Ok(host),
        Ok(account) => Err(Error::new(
            Kind::Usage,
            format!("{typed} has an account of its own, which takes its own kind of key"),
        )
        .help(format!(
            "run `{}`. --token and --header are for websites fetchloom has no account for",
            account.command()
        ))),
        Err(error) => site::host(typed).ok_or(error),
    }
}

/// signs in to the website `login` names with its own credential, saved for its host alone.
pub(super) fn sign_in(context: &Arc<Context>, login: &Login) -> Result<Outcome, Error> {
    if login.host.is_some() {
        return Err(
            Error::new(Kind::Usage, "--host names a dataverse installation").help(
                "name the website as the account, as in `fetchloom auth login data.example.org --token`",
            ),
        );
    }
    let host = site_host(&login.account)?;
    let asked = match &login.header {
        Some(typed) => {
            site::header_name(typed)?;
            Asked::Header(typed.trim().to_owned())
        }
        None => Asked::Token,
    };
    let cancelled = format!("signing in to {host}, so nothing changed");
    let told = login.with_token;
    let run = || -> Result<Origin, Error> {
        if !told {
            replacing(context, &host)?;
        }
        let key = secret(context, login, &host, &asked)?;
        let saved = asked.saved(key);
        // the key is in hand, so a ctrl-c at the question below cancels it and any other stop
        // lets it finish
        context.stop.finish();
        let origin = keep(context, &host, &saved, told)?;
        context.output.done(
            "Signed in",
            &format!(
                "to {host} with {} \u{b7} saved to {}",
                described(&saved),
                origin.shown()
            ),
        );
        Ok(origin)
    };
    let origin = run().map_err(|error| error.cancelling(&cancelled))?;
    Ok(Outcome {
        account: format!("site:{host}"),
        user: None,
        saved_to: Some(origin.to_string()),
    })
}

/// asks before a sign-in replaces what is saved for `host`, as a person at the terminal may
/// not know it is there.
fn replacing(context: &Context, host: &str) -> Result<(), Error> {
    let Some((before, origin)) = site::find(&auth::env, host) else {
        return Ok(());
    };
    context.output.step(
        "Found",
        &format!("{} for {host} in {}", described(&before), origin.shown()),
    );
    agrees(context, host, &before, "replace it?", false)
}

/// the key `asked` for `host`: read from stdin with `--with-token`, else typed at the terminal.
fn secret(context: &Context, login: &Login, host: &str, asked: &Asked) -> Result<String, Error> {
    let output = &context.output;
    let (prompt, option) = asked.named();
    let command = format!(
        "fetchloom auth login {} {option}",
        crate::suggested::shell_word(host)
    );
    let text = if login.with_token {
        read_stdin(output, prompt)?
    } else if output.can_ask() {
        guide(context, host, asked);
        output
            .ask_text(prompt, true)?
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| Error::new(Kind::Interrupted, "stopped signing in"))?
    } else {
        return Err(Error::new(
            Kind::Usage,
            format!("signing in to {host} needs a terminal to paste the key into"),
        )
        .help(format!(
            "in your own terminal, run `{command}`. a script can pipe the key in, as in `{command} --with-token`"
        )));
    };
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(
            Error::new(Kind::Usage, "--with-token read nothing from stdin").help(format!(
                "pipe the key in, as in `echo $KEY | {command} --with-token`"
            )),
        );
    }
    if reqwest::header::HeaderValue::from_str(&format!("Bearer {text}")).is_err() {
        return Err(Error::new(
            Kind::Usage,
            "the key holds a character no request header can carry, such as a line break",
        )
        .help("copy the key alone, with nothing else around it"));
    }
    Ok(text)
}

/// what stdin brings: a line typed at a terminal after `prompt`, hidden, or what a pipe sends.
fn read_stdin(output: &Output, prompt: &str) -> Result<String, Error> {
    if std::io::stdin().is_terminal() {
        return output.read_hidden(prompt);
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|err| Error::io("could not read the key from stdin", &err))?;
    Ok(text)
}

/// the steps to a website's key, which each site words its own way.
fn guide(context: &Context, host: &str, asked: &Asked) {
    let output = &context.output;
    output.say(&format!("{:>11} {host}", "Sign in"));
    let sent = match asked {
        Asked::Token => "Authorization: Bearer".to_owned(),
        Asked::Header(name) => format!("the {name} header"),
    };
    let steps = [
        format!("sign in at {host} in your browser"),
        "make a token or an api key where its settings or profile offer one".to_owned(),
        format!("paste it below; it is sent in {sent}"),
    ];
    for (number, step) in steps.iter().enumerate() {
        output.say(&format!("{:>11} {step}", format!("{}.", number + 1)));
    }
}

/// whether the person agrees to `question` about saving `saved` for `host`: a yes, or `--yes`
/// where nobody can be asked. a no is a cancel; no answer at all needs `--yes`.
fn agrees(
    context: &Context,
    host: &str,
    saved: &Saved,
    question: &str,
    default: bool,
) -> Result<(), Error> {
    match agree(context, question, default)? {
        Some(true) => Ok(()),
        None if context.yes => Ok(()),
        Some(false) => Err(answered_no()),
        None => Err(Error::new(
            Kind::Unconfirmed,
            format!(
                "saving {} for {host} needs a yes, and fetchloom cannot ask you here",
                saved.what()
            ),
        )
        .help("run it again with --yes")),
    }
}

/// saves `saved` for `host` once the person agrees, or without asking when `told`: in the
/// system's credential store, or in fetchloom's own file where the machine has none.
fn keep(context: &Context, host: &str, saved: &Saved, told: bool) -> Result<Origin, Error> {
    let var = &auth::env;
    let failed = |err: std::io::Error| {
        Error::io(format!("could not save {} for {host}", saved.what()), &err)
    };
    if store::KEYRING.usable(var) {
        if !told {
            let question = format!("save it in {}?", auth::keyring_name());
            agrees(context, host, saved, &question, true)?;
        }
        return site::save(var, host, saved, store::Place::Keyring).map_err(failed);
    }
    let file = store::folder(var)
        .unwrap_or_default()
        .join("credentials.json");
    if told {
        context.output.warn(
            Warning::AuthNoKeyring,
            &format!(
                "this machine has no credential store, so it is saved in {}, readable only by you",
                file.display()
            ),
        );
    } else {
        let question = format!(
            "no credential store here, so it would go in a file only you can read\nsave it to {}?",
            crate::units::path(&file)
        );
        agrees(context, host, saved, &question, false)?;
    }
    site::save(var, host, saved, store::Place::File).map_err(failed)
}

/// what is saved, as a line names it: "a token", "the X-API-Key header" or "3 cookies from
/// firefox".
fn described(saved: &Saved) -> String {
    match saved {
        Saved::Token { .. } => "a token".to_owned(),
        Saved::Header { name, .. } => format!("the {name} header"),
        Saved::Cookies { cookies, from } => format!(
            "{} from {}",
            crate::units::count(cookies.len() as u64, "cookie", "cookies"),
            match from {
                Taken::Browser(browser) => browser.clone(),
                Taken::File(_) => "a cookies file".to_owned(),
            }
        ),
    }
}

/// the moment `seconds` after 1970 is.
fn moment(seconds: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

/// the date `seconds` after 1970 is, as `2026-12-01`.
fn date(seconds: i64) -> String {
    let mut stamp = crate::project::lock::timestamp(moment(seconds));
    stamp.truncate(10);
    stamp
}

/// a line for each website with a saved credential: its host, the kind, where it is kept and,
/// for cookies, when the soonest expires; cookies past it are marked expired with how to save
/// new ones.
pub(super) fn listed(output: &Output) -> Vec<State> {
    let var = &auth::env;
    let now = SystemTime::now();
    let mut rows = Vec::new();
    for host in site::hosts(var) {
        let Some((saved, origin)) = site::find(var, &host) else {
            continue;
        };
        let soonest = saved.soonest();
        let when = soonest.map_or_else(String::new, |soonest| {
            format!(", the soonest expiring {}", date(soonest))
        });
        if soonest.is_some_and(|soonest| moment(soonest) <= now) {
            output.mark(
                "Expired",
                &format!(
                    "{} for {host}{when}; {}",
                    described(&saved),
                    saved.again(&host)
                ),
            );
        } else {
            output.step(
                "Signed in",
                &format!(
                    "to {host} with {}{when} \u{b7} {}",
                    described(&saved),
                    origin.shown()
                ),
            );
        }
        rows.push(State {
            account: format!("site:{host}"),
            signed_in: true,
            from: Some(origin.to_string()),
            user: None,
            valid: None,
            kind: Some(saved.kind()),
            expires: soonest.map(|soonest| crate::project::lock::timestamp(moment(soonest))),
        });
    }
    rows
}

/// removes a website's saved credential for the host `logout` names. none when it names an
/// account, or a login's host with nothing saved for it here, which `auth logout` goes on to.
pub(super) fn sign_out(context: &Context, logout: &Logout) -> Result<Option<Report>, Error> {
    if logout.host.is_some() {
        return Ok(None);
    }
    let (host, named) = match Account::parse(&logout.account, None) {
        Ok(Account::Login(host)) => (host, true),
        Ok(_) => return Ok(None),
        Err(error) => (site::host(&logout.account).ok_or(error)?, false),
    };
    let var = &auth::env;
    let output = &context.output;
    if !site::hosts(var).contains(&host) {
        if named {
            return Ok(None);
        }
        output.done("Ok", &format!("nothing was saved for {host}"));
        return Ok(Some(Report::Logout {
            account: format!("site:{host}"),
            removed_from: None,
        }));
    }
    // a sign-out has nothing to stop, so a stop asked once it starts lets it finish
    context.stop.finish();
    let removed = site::forget(var, &host)
        .map_err(|err| Error::io(format!("could not remove what was saved for {host}"), &err))?;
    let removed_from = removed.map(|origin| {
        output.done(
            "Signed out",
            &format!("of {host} \u{b7} removed from {}", origin.shown()),
        );
        origin.to_string()
    });
    if let Some(left) = Account::Login(host.clone()).find(var) {
        output.warn_with(
            Warning::AuthKeyRemains,
            &format!(
                "a login for {host} is still read from {}",
                left.origin.shown()
            ),
            &format!(
                "run `fetchloom auth logout {}` again to remove it too",
                crate::suggested::shell_word(&logout.account)
            ),
        );
    }
    Ok(Some(Report::Logout {
        account: format!("site:{host}"),
        removed_from,
    }))
}
