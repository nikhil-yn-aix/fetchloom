//! switches that let one experiment build measure a candidate speed-up against the code it would
//! replace. each is off unless its `FETCHLOOM_EXP_<NAME>` variable is true or 1, read once as a
//! command starts into a field that tests set directly; a switch that is off runs the code as it
//! was. a switch that wins becomes the plain behavior and leaves this module, as one that loses
//! leaves with its code.

/// hunt-feel's switches for the commands a person runs every day, every one off by default.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Feel {
    /// a sync that places skips its first look at what the cache lacks, which only weighs the
    /// room, where the sizes it already knows show room for every object it needs; the strong look
    /// after the fill stays (`FETCHLOOM_EXP_LOOKUP_ONCE_PLACING`).
    pub(crate) lookup_once_placing: bool,
}

impl Feel {
    /// the switches the variables `var` gives turn on.
    pub(crate) fn read(var: crate::machine::Var<'_>) -> Feel {
        let on = |name: &str| {
            matches!(
                crate::machine::switched(name, var(name).as_deref()),
                Ok(Some(true))
            )
        };
        Feel {
            lookup_once_placing: on("FETCHLOOM_EXP_LOOKUP_ONCE_PLACING"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::Feel;

    /// each switch is read from its own variable, on only when set true.
    #[test]
    fn each_switch_reads_its_own_variable() {
        let read = |name: &'static str, value: &'static str| {
            Feel::read(&move |asked: &str| (asked == name).then(|| OsString::from(value)))
        };
        assert!(read("FETCHLOOM_EXP_LOOKUP_ONCE_PLACING", "1").lookup_once_placing);
        assert!(read("FETCHLOOM_EXP_LOOKUP_ONCE_PLACING", "true").lookup_once_placing);
        assert!(!read("FETCHLOOM_EXP_LOOKUP_ONCE_PLACING", "0").lookup_once_placing);
        assert!(!read("FETCHLOOM_EXP_NONE", "1").lookup_once_placing);
    }
}
