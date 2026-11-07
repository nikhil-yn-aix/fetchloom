//! switches that let one build measure a candidate change against the current behavior. each is
//! off unless `FETCHLOOM_EXP_<NAME>` is set. a switch is read once, where the work it changes is
//! set up, into a field that tests set directly. a change that wins becomes the behavior and its
//! switch goes; one that loses goes with its code.

use std::env;

/// whether `FETCHLOOM_EXP_<name>` is set to anything but empty or `0`.
pub(crate) fn on(name: &str) -> bool {
    env::var_os(format!("FETCHLOOM_EXP_{name}"))
        .is_some_and(|value| !value.is_empty() && value != "0")
}

/// the number in `FETCHLOOM_EXP_<name>`, when it holds one.
pub(crate) fn number(name: &str) -> Option<u64> {
    env::var(format!("FETCHLOOM_EXP_{name}"))
        .ok()?
        .trim()
        .parse()
        .ok()
}
