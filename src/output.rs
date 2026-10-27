//! what fetchloom prints. stdout carries the answer and nothing else, stderr carries the story.

mod style;

use std::io::Write;

use anstyle::{AnsiColor, Style as Paint};

pub(crate) use style::{ColorChoice, Style};

use crate::error::Error;

pub(crate) struct Output {
    style: Style,
}

impl Output {
    pub(crate) fn new(style: Style) -> Output {
        Output { style }
    }

    /// prints a failure: the sentence, its supporting lines, and what to do next.
    pub(crate) fn error(&self, error: &Error) {
        let mut err = self.stderr();
        let red = Paint::new().fg_color(Some(AnsiColor::Red.into())).bold();
        let bold = Paint::new().bold();
        let _ = writeln!(err, "{red}{}{red:#} {}", self.style.failed(), error.message);
        for line in &error.details {
            let _ = writeln!(err, "  {line}");
        }
        if let Some(help) = &error.help {
            let _ = writeln!(err, "  {bold}help:{bold:#} {help}");
        }
    }

    fn stderr(&self) -> anstream::AutoStream<std::io::StderrLock<'static>> {
        let choice = if self.style.color {
            anstream::ColorChoice::Always
        } else {
            anstream::ColorChoice::Never
        };
        anstream::AutoStream::new(std::io::stderr().lock(), choice)
    }
}
