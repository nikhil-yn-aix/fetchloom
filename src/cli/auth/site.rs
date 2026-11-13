//! `auth login <host>` with a website's own credential: a token, a header or cookies, kept for
//! that host alone, as `auth` lists it and `auth logout` removes it.

use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Read as _};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use super::{Outcome, Report, State, agree, answered_no};
use crate::auth::cookies::{self, Picked, Why};
use crate::auth::site::{self, Saved, Taken};
use crate::auth::{self, Account, Origin, store};
use crate::cli::Context;
use crate::cli::args::{Login, Logout};
use crate::error::{Error, Kind};
use crate::output::{Output, Warning};

/// whether `login` saves a website's own credential rather than an account's key.
pub(super) fn wanted(login: &Login) -> bool {
    login.token || login.header.is_some() || login.cookies.is_some() || login.browser.is_some()
}

/// the most of a cookies.txt file read: a browser's whole jar takes a few MiB.
const LARGEST_FILE: u64 = 64 << 20;

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
    fn saved(&self, key: String) -> Saved {
        match self {
            Asked::Token => Saved::Token { token: key },
            Asked::Header(name) => Saved::Header {
                name: name.clone(),
                value: key,
            },
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
            "run `{}`. --token, --header, --cookies and --browser are for websites fetchloom has no account for",
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
        let saved = if let Some(path) = &login.cookies {
            let saved = from_file(context, &host, path)?;
            replacing(context, &host)?;
            saved
        } else if let Some(typed) = &login.browser {
            let saved = from_browser(context, &host, typed)?;
            replacing(context, &host)?;
            saved
        } else {
            if !told {
                replacing(context, &host)?;
            }
            let key = secret(context, login, &host, &asked)?;
            asked.saved(key)
        };
        // the key is in hand, so a ctrl-c at the question below cancels it and any other stop
        // lets it finish
        context.stop.finish();
        let origin = keep(context, &host, &saved, told)?;
        let when = saved.soonest().map_or_else(String::new, |soonest| {
            format!(", the soonest expiring {}", date(soonest))
        });
        context.output.done(
            "Signed in",
            &format!(
                "to {host} with {}{when} \u{b7} saved to {}",
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

/// the ways a website may let a person in, as a refusal's sign-in offers them: the word a
/// picker shows first, then what it means.
const WAYS: [(&str, &str); 5] = [
    ("browser", "the cookies of a browser signed in there"),
    ("cookies", "a cookies.txt file a browser extension saved"),
    ("token", "a token the site gave you"),
    ("header", "a key the site sends in a header of its own"),
    ("login", "a username and password"),
];

/// asks a person at the terminal how they sign in to the website `account` names, after it
/// refused a request without naming a way in, then signs them in that way. false when they
/// would rather not, or nobody can be asked.
pub(super) async fn offered(context: &Arc<Context>, account: &Account) -> Result<bool, Error> {
    let Account::Login(host) = account else {
        return Ok(false);
    };
    let output = &context.output;
    let ways = WAYS.map(|(word, meaning)| format!("{word:<7}  {meaning}"));
    let chosen = match output.choose(&format!("sign in to {host} with"), &ways, "") {
        Err(error) if error.kind == Kind::Interrupted => return Err(error),
        Err(_) => None,
        Ok(chosen) => chosen,
    };
    let mut login = Login {
        account: host.clone(),
        ..Login::default()
    };
    let typed = |prompt: &str, hint: &str| -> Result<Option<String>, Error> {
        output.item(hint);
        Ok(output
            .ask_text(prompt, false)?
            .filter(|text| !text.is_empty()))
    };
    match chosen {
        None => return Ok(false),
        Some(0) => {
            login.browser = typed(
                "browser",
                &format!(
                    "{}, with :<profile> for another profile than the newest",
                    cookies::BROWSERS
                ),
            )?;
        }
        Some(1) => login.cookies = typed("file", "the cookies.txt file's path")?.map(Into::into),
        Some(2) => login.token = true,
        Some(3) => {
            login.header = typed(
                "header",
                "the header's name, such as X-API-Key; its value is asked next",
            )?;
        }
        Some(_) => {
            super::sign_in(context, account, &login, true).await?;
            return Ok(true);
        }
    }
    if !wanted(&login) {
        return Ok(false);
    }
    sign_in(context, &login)?;
    Ok(true)
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

/// the cookies for `host` in the cookies.txt file at `path`, once they are shown, by name,
/// domain and expiry, with those left out.
fn from_file(context: &Context, host: &str, path: &Path) -> Result<Saved, Error> {
    let shown = crate::units::path(path);
    let failed = |err: &std::io::Error| Error::io(format!("could not read {shown}"), err);
    let file = std::fs::File::open(path).map_err(|err| failed(&err))?;
    let mut text = String::new();
    file.take(LARGEST_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|err| failed(&err))?;
    if text.len() as u64 > LARGEST_FILE {
        return Err(Error::new(
            Kind::Usage,
            format!("{shown} is larger than a cookies file of one browser"),
        )
        .help("export the cookies of the site alone"));
    }
    let (found, _) = cookies::netscape(&text);
    if found.is_empty() {
        return Err(Error::new(
            Kind::Usage,
            format!("{shown} holds no cookies in the cookies.txt format curl and yt-dlp read"),
        )
        .help(
            "export them again as cookies.txt, the netscape format: a cookie a line, seven fields apart by tabs",
        ));
    }
    let picked = cookies::pick(found, host, SystemTime::now());
    tell(&context.output, host, &picked, &format!("in {shown}"));
    if picked.kept.is_empty() {
        let bare = site::without_port(host);
        let again = format!(
            "sign in at {bare} in your browser again and export its cookies, then run the same command again"
        );
        let error = none_kept(host, &picked, &shown, &again);
        return Err(if error.kind == Kind::Usage {
            error.help(format!(
                "export the cookies while you are signed in at {bare}, or name the host they were set for"
            ))
        } else {
            error
        });
    }
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(Saved::Cookies {
        from: Taken::File(path),
        cookies: picked.kept,
    })
}

/// the error for cookies of which `host` takes none, from `place`: expired ones, with `again`
/// for help, or none at all, whose domains it names.
fn none_kept(host: &str, picked: &Picked, place: &str, again: &str) -> Error {
    let expired = picked
        .left
        .iter()
        .any(|left| left.why == Why::Expired && cookies::covers(&left.domain, host));
    if expired {
        return Error::new(
            Kind::AuthRequired,
            format!("the cookies for {host} in {place} have expired"),
        )
        .help(again);
    }
    if picked.left.iter().any(|left| left.why == Why::Locked) {
        let help = if cfg!(target_os = "macos") {
            "when macos asks whether fetchloom may use the browser's safe storage key in your keychain, allow it, then run the same command again"
        } else {
            "unlock your keyring, which the browser keeps its key in, then run the same command again; fetchloom asks the secret service, which gnome keyring and KDE wallet 5.97 and later answer"
        };
        return Error::new(
            Kind::AuthRequired,
            format!("the cookies for {host} in {place} are encrypted, and fetchloom could not read the key that opens them"),
        )
        .help(format!("{help}. or export the site's cookies as cookies.txt with a browser extension and use --cookies <file>"));
    }
    let mut domains: Vec<&str> = Vec::new();
    for left in &picked.left {
        if !domains.contains(&left.domain.as_str()) {
            domains.push(&left.domain);
        }
    }
    let error = Error::new(Kind::Usage, format!("{place} holds no cookies for {host}"));
    if domains.is_empty() {
        return error;
    }
    let named: Vec<&str> = domains.into_iter().take(10).collect();
    error.detail(format!("it holds cookies for {}", named.join(", ")))
}

/// the cookies for `host` in the profile of the browser `typed` names, once they are shown, by
/// name, domain and expiry, with those left out that a person might have expected.
fn from_browser(context: &Context, host: &str, typed: &str) -> Result<Saved, Error> {
    let browser = cookies::Browser::parse(typed)?;
    let (mut picked, profile) = browser.cookies(&auth::env, host, SystemTime::now())?;
    // a profile holds every site's cookies; only those of the host's own site are worth a word
    picked
        .left
        .retain(|left| left.why != Why::Elsewhere || same_site(&left.domain, host));
    let place = format!(
        "{}'s profile {}",
        browser.name(),
        crate::units::path(&profile)
    );
    tell(&context.output, host, &picked, &format!("in {place}"));
    let bare = site::without_port(host);
    if picked.kept.is_empty() {
        let name = browser.name();
        let again = format!("sign in to {bare} in {name} again, then run the same command again");
        let error = none_kept(host, &picked, &place, &again);
        return Err(if error.kind == Kind::Usage {
            error.help(format!(
                "sign in to {bare} in {name}, then run the same command again. a profile other than the one it used last is named as `--browser {name}:<profile>`"
            ))
        } else {
            error
        });
    }
    Ok(Saved::Cookies {
        from: Taken::Browser(typed.trim().to_owned()),
        cookies: picked.kept,
    })
}

/// whether a cookie for `domain` belongs to the same site as `host`: both end in its last two
/// labels, or three where the second to last is short, as in `example.co.uk`.
fn same_site(domain: &str, host: &str) -> bool {
    let host = site::without_port(host).to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return domain.eq_ignore_ascii_case(&host);
    }
    let labels: Vec<&str> = host.split('.').collect();
    let keep = if labels.len() > 2 && labels[labels.len() - 2].len() <= 3 {
        3
    } else {
        2
    };
    let base = labels[labels.len().saturating_sub(keep)..].join(".");
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    domain == base || domain.ends_with(&format!(".{base}"))
}

/// tells which cookies `from` holds for `host`, by name, domain and expiry, never by value, and
/// which were left out and why.
fn tell(output: &Output, host: &str, picked: &Picked, from: &str) {
    let count = |cookies: usize| crate::units::count(cookies as u64, "cookie", "cookies");
    if !picked.kept.is_empty() {
        output.step(
            "Found",
            &format!("{} for {host} {from}", count(picked.kept.len())),
        );
    }
    for cookie in &picked.kept {
        let ends = cookie.expires.map_or_else(
            || "ends with the browser's session".to_owned(),
            |expires| format!("expires {}", date(expires)),
        );
        output.item(&format!(
            "{} \u{b7} {} \u{b7} {ends}",
            cookie.name, cookie.domain
        ));
    }
    for (why, said) in [
        (Why::Elsewhere, "for other hosts"),
        (Why::Expired, "that expired"),
        (Why::Unsendable, "no request can carry"),
        (
            Why::Apart,
            "the browser keeps apart, for a container, a private window or another site's frames",
        ),
        (Why::Locked, "no key fetchloom could read opens"),
    ] {
        let mut domains: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut order: Vec<&str> = Vec::new();
        let mut left = 0;
        for cookie in picked.left.iter().filter(|left| left.why == why) {
            left += 1;
            if !domains.contains_key(cookie.domain.as_str()) {
                order.push(&cookie.domain);
            }
            domains
                .entry(&cookie.domain)
                .or_default()
                .push(&cookie.name);
        }
        if left == 0 {
            continue;
        }
        let mut named: Vec<String> = order
            .iter()
            .take(6)
            .map(|domain| format!("{} of {domain}", domains[domain].join(", ")))
            .collect();
        if order.len() > 6 {
            named.push(format!(
                "and {} more",
                crate::units::count((order.len() - 6) as u64, "host", "hosts")
            ));
        }
        output.step(
            "Left out",
            &format!("{} {said}: {}", count(left), named.join("; ")),
        );
    }
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
    let (it, they) = if matches!(saved, Saved::Cookies { .. }) {
        ("them", "they")
    } else {
        ("it", "it")
    };
    if store::KEYRING.usable(var) {
        if !told {
            let question = format!("save {it} in {}?", auth::keyring_name());
            agrees(context, host, saved, &question, true)?;
        }
        return site::save(var, host, saved, store::Place::Keyring).map_err(|err| {
            // windows' store keeps 2,560 bytes an entry, which a site's cookies often pass
            let error = failed(err);
            if matches!(saved, Saved::Cookies { .. }) {
                error.help("set FETCHLOOM_KEYRING=false to keep them in fetchloom's own file, readable only by you, then run it again")
            } else {
                error
            }
        });
    }
    let file = store::folder(var)
        .unwrap_or_default()
        .join("credentials.json");
    if told || context.yes {
        context.output.warn(
            Warning::AuthNoKeyring,
            &format!(
                "this machine has no credential store, so {they} went in {}, readable only by you",
                file.display()
            ),
        );
    } else {
        let question = format!(
            "no credential store here, so {they} would go in a file only you can read\nsave {it} to {}?",
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

use crate::auth::site::moment;

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
