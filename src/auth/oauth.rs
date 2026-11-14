//! a token endpoint's answer, as google's and azure's metadata service give it: a bearer token and
//! how long it lives, or why there is none.

use serde::Deserialize;

use crate::error::{Error, Kind, report_answer};

/// what a token endpoint answers: a token, or why it gave none.
#[derive(Deserialize)]
struct Answer {
    access_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    token_type: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// what `endpoint` said in an answer of `status` and `body` that gave no token: `who`'s words,
/// made plain, or the status alone when it gave none.
pub(crate) fn said(endpoint: &str, who: &str, status: u16, body: &[u8]) -> String {
    serde_json::from_slice::<Answer>(body)
        .ok()
        .and_then(|answer| {
            let code = answer.error?;
            Some(match answer.error_description {
                Some(description) => format!("{code}: {description}"),
                None => code,
            })
        })
        .map_or_else(
            || format!("{endpoint} answered {status}"),
            |said| format!("{who} said {}", super::google::plain(&said)),
        )
}

/// the bearer token in `endpoint`'s answer `body`, and the seconds it lives when the answer says.
///
/// # Errors
///
/// `source.invalid` for an answer without a bearer token a request header can carry.
pub(crate) fn bearer(endpoint: &str, body: &[u8]) -> Result<(String, Option<u64>), Error> {
    let unusable = |what: &str| {
        Error::new(Kind::SourceInvalid, format!("{endpoint} sent {what}")).help(report_answer())
    };
    let answer: Answer =
        serde_json::from_slice(body).map_err(|_| unusable("an answer fetchloom could not read"))?;
    if answer
        .token_type
        .as_deref()
        .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
    {
        return Err(unusable("a token of another kind than bearer"));
    }
    // a token goes in a header, so one a header cannot carry is no token
    let value = answer
        .access_token
        .filter(|token| !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic()))
        .ok_or_else(|| unusable("no token a request can carry"))?;
    let seconds = match answer.expires_in {
        Some(serde_json::Value::Number(number)) => number.as_u64(),
        Some(serde_json::Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    };
    Ok((value, seconds))
}
