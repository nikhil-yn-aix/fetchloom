//! Questions for the person at the terminal: a yes or no, a choice from a list, a line of text.
//! Without a terminal nothing is asked, and the question becomes an error naming the flag or
//! the command that answers it.

use std::sync::Arc;

use console::Term;
use dialoguer::{Confirm, Input};
use fl_model::error::ErrorKind;

use crate::failure::Failure;
use crate::report::Reporter;

/// Something that can put a question to the user.
pub(crate) trait Ask: Send + Sync {
    /// Whether the user agrees to `question`. Without a terminal, `refusal` is the error.
    fn confirm(&self, question: &str, refusal: Failure) -> Result<bool, Failure>;

    /// A line the user types, trimmed; empty when they type nothing. Without a terminal,
    /// `refusal` is the error.
    fn text(&self, prompt: &str, refusal: Failure) -> Result<String, Failure>;
}

/// Asks on the terminal, through stderr, pausing whatever the reporter draws.
pub(crate) struct Prompt {
    events: Arc<dyn Reporter>,
}

impl Prompt {
    /// Questions that pause the drawing of `events` while they wait.
    pub(crate) fn new(events: Arc<dyn Reporter>) -> Self {
        Self { events }
    }

    fn paused<T>(&self, ask: impl FnOnce(&Term) -> dialoguer::Result<T>) -> Result<T, Failure> {
        let mut answer = None;
        let mut ask = Some(ask);
        self.events.suspend(&mut || {
            if let Some(ask) = ask.take() {
                answer = Some(ask(&Term::stderr()));
            }
        });
        answer
            .ok_or_else(|| Failure::new(ErrorKind::LocalIo, "the question was not asked"))?
            .map_err(|dialoguer::Error::IO(error)| {
                Failure::new(ErrorKind::LocalIo, format!("the terminal failed: {error}"))
            })
    }
}

impl Ask for Prompt {
    fn confirm(&self, question: &str, _refusal: Failure) -> Result<bool, Failure> {
        self.paused(|term| {
            Confirm::new()
                .with_prompt(question)
                .default(false)
                .interact_on(term)
        })
    }

    fn text(&self, prompt: &str, _refusal: Failure) -> Result<String, Failure> {
        self.paused(|term| {
            Input::<String>::new()
                .with_prompt(prompt)
                .allow_empty(true)
                .interact_text_on(term)
        })
        .map(|line| line.trim().to_owned())
    }
}

/// Asks nothing: every question fails with its refusal.
pub(crate) struct Refuse;

impl Ask for Refuse {
    fn confirm(&self, _question: &str, refusal: Failure) -> Result<bool, Failure> {
        Err(refusal)
    }

    fn text(&self, _prompt: &str, refusal: Failure) -> Result<String, Failure> {
        Err(refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_terminal_every_question_is_its_refusal() {
        let refusal = || Failure::new(ErrorKind::Usage, "pass --yes");
        let confirm = Refuse.confirm("go?", refusal()).unwrap_err();
        assert_eq!(confirm.to_string(), "pass --yes");
        assert!(Refuse.text("what?", refusal()).is_err());
    }
}
