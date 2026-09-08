//! What a test says when the machine it is running on cannot give it what it
//! needs.

use std::io::Write;

const RECORD: &str = "FETCHLOOM_TEST_DECLINED";

#[must_use]
pub fn name_of<T>(_: T) -> &'static str {
    let path = std::any::type_name::<T>();
    let without_marker = path.strip_suffix("::declining").unwrap_or(path);
    without_marker.rsplit("::").next().unwrap_or(without_marker)
}

pub fn declined(test: &str, needs: &str) {
    let line = format!("NOT VERIFIED {test}: needs {needs}");
    eprintln!("{line}");
    let Some(path) = std::env::var_os(RECORD) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

#[macro_export]
macro_rules! decline {
    ($needs:expr) => {{
        fn declining() {}
        $crate::declined($crate::name_of(declining), $needs);
    }};
}

pub trait Presence {
    fn is_absent(&self) -> bool;
}

impl<T> Presence for Vec<T> {
    fn is_absent(&self) -> bool {
        self.is_empty()
    }
}

impl<T> Presence for Option<T> {
    fn is_absent(&self) -> bool {
        self.is_none()
    }
}

impl Presence for bool {
    fn is_absent(&self) -> bool {
        !*self
    }
}

#[macro_export]
macro_rules! require {
    ($subject:expr, $needs:expr) => {{
        let found = $subject;
        if $crate::Presence::is_absent(&found) {
            fn declining() {}
            $crate::declined($crate::name_of(declining), $needs);
            return;
        }
        found
    }};
}
