//! The words and the document `fl` prints, kept as snapshots with the machine left out.

use serde_json::Value;

use crate::support::{Failure, World, json};

type TestResult = Result<(), Failure>;

fn redact(world: &World, text: &str) -> String {
    world.scrub(text)
}

fn pretty(world: &World, document: &Value) -> Result<String, Failure> {
    let mut document = document.clone();
    if let Some(phases) = document["work"]["phase_ms"].as_object_mut() {
        for value in phases.values_mut() {
            *value = Value::from(0);
        }
    }
    Ok(redact(world, &serde_json::to_string_pretty(&document)?))
}

macro_rules! snapshot {
    ($name:expr, $text:expr) => {
        insta::with_settings!({
            filters => vec![
                (r"127\.0\.0\.1:\d+", "[SERVER]"),
                (r"\d+(\.\d+)?(ms|s)\b", "[TIME]"),
            ],
        }, {
            insta::assert_snapshot!($name, $text);
        });
    };
}

#[tokio::test(flavor = "multi_thread")]
async fn the_story_of_a_first_add() -> TestResult {
    let world = World::start().await?;
    let ran = world.add()?;
    snapshot!("add_stderr", redact(&world, &ran.stderr));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_story_of_a_second_verbose_sync() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    let ran = world.here(&["sync", "-v"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    snapshot!("sync_verbose_stderr", redact(&world, &ran.stderr));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_document_of_a_second_sync() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    world.here(&["sync", "-v"])?;
    let ran = world.here(&["sync", "--json"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let text = pretty(&world, &json(&ran)?)?;
    snapshot!("sync_json_stdout", text);
    Ok(())
}
