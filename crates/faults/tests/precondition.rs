//! What a test that cannot run here says, and where it is recorded.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use fetchloom_engine as _;

const RECORD: &str = "FETCHLOOM_TEST_DECLINED";

#[test]
fn a_declination_names_the_test_and_what_it_needed() {
    let directory = std::env::temp_dir().join(format!(
        "fetchloom-declined-{}-{}",
        std::process::id(),
        line!()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let record = directory.join("declined.txt");

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "declining_here", "--ignored", "--nocapture"])
        .env(RECORD, &record)
        .output()
        .unwrap();
    assert!(output.status.success(), "the declining child failed");
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        said.contains("NOT VERIFIED declining_here: needs a machine this one is not"),
        "the declination was recorded and never said: {said}"
    );

    let written = std::fs::read_to_string(&record).unwrap();
    assert_eq!(
        written.trim(),
        "NOT VERIFIED declining_here: needs a machine this one is not",
        "a declination was recorded as something other than the test and what it needed"
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
#[ignore = "run only as the declining child of the precondition test"]
fn declining_here() {
    fetchloom_faults::decline!("a machine this one is not");
}
