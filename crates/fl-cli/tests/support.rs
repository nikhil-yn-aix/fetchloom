//! A fake zenodo, a scratch machine for `fl` to run on, and a way to run it.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub type Failure = Box<dyn Error>;

pub const RECORD: &str = include_str!("../../../fixtures/responses/zenodo/3608990/record.json");
pub const ADAPTER: &str = include_str!("../../../sources/zenodo.toml");
pub const IRIS: &[u8] = include_bytes!("../../../fixtures/responses/zenodo/3608990/iris.csv");
pub const IRIS_MD5: &str = "395d6abd001abf3760a1a794ef4c453c";
pub const NOT_FOUND: &str =
    r#"{"status":404,"message":"The persistent identifier does not exist."}"#;

pub struct World {
    pub temp: TempDir,
    pub server: MockServer,
    pub project: PathBuf,
    pub store: PathBuf,
    pub config: PathBuf,
}

pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl World {
    pub async fn start() -> Result<Self, Failure> {
        let temp = tempfile::tempdir()?;
        let server = MockServer::start().await;
        let project = temp.path().join("project");
        let store = temp.path().join("store");
        let config = temp.path().join("config");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(config.join("sources"))?;
        std::fs::write(
            config.join("sources").join("zenodo.toml"),
            ADAPTER.replace("https://zenodo.org", &server.uri()),
        )?;
        let record = RECORD.replace("https://zenodo.org", &server.uri());
        Mock::given(method("GET"))
            .and(path("/api/records/3608990"))
            .respond_with(ResponseTemplate::new(200).set_body_string(record))
            .mount(&server)
            .await;
        for route in [
            "/api/records/3608990/files/iris.csv/content",
            "/files/iris.csv",
        ] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(IRIS))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/api/records/404"))
            .respond_with(ResponseTemplate::new(404).set_body_string(NOT_FOUND))
            .mount(&server)
            .await;
        Ok(Self {
            temp,
            server,
            project,
            store,
            config,
        })
    }

    pub fn fl(&self, dir: &Path, args: &[&str]) -> Result<Ran, Failure> {
        self.fl_with_store(dir, &self.store, args)
    }

    pub fn fl_with_store(&self, dir: &Path, store: &Path, args: &[&str]) -> Result<Ran, Failure> {
        self.fl_with_env(dir, store, args, &[])
    }

    pub fn fl_with_env(
        &self,
        dir: &Path,
        store: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Ran, Failure> {
        let out = Command::new(env!("CARGO_BIN_EXE_fl"))
            .args(args)
            .current_dir(dir)
            .env_remove("FETCHLOOM_LOG")
            .env_remove("HF_ENDPOINT")
            .env_remove("HF_TOKEN")
            .env("FETCHLOOM_STORE", store)
            .env("FETCHLOOM_CONFIG", self.config.join("config.toml"))
            .envs(env.iter().copied())
            .output()?;
        Ok(Ran {
            code: out.status.code(),
            stdout: String::from_utf8(out.stdout)?,
            stderr: String::from_utf8(out.stderr)?,
        })
    }

    pub fn here(&self, args: &[&str]) -> Result<Ran, Failure> {
        self.fl(&self.project, args)
    }

    pub fn add(&self) -> Result<Ran, Failure> {
        let ran = self.here(&["add", "zenodo:3608990"])?;
        if ran.code != Some(0) {
            return Err(format!("fl add failed: {}", ran.stderr).into());
        }
        Ok(ran)
    }

    pub async fn requests(&self) -> Result<usize, Failure> {
        Ok(self
            .server
            .received_requests()
            .await
            .ok_or("the server keeps no request log")?
            .len())
    }

    pub fn iris(&self) -> PathBuf {
        self.project.join("data").join("3608990").join("iris.csv")
    }

    pub fn scrub(&self, text: &str) -> String {
        let temp = self.temp.path().to_path_buf();
        let roots = [
            (&self.project, "[PROJECT]"),
            (&self.store, "[STORE]"),
            (&self.config, "[CONFIG]"),
            (&temp, "[TEMP]"),
        ];
        let mut text = text.to_owned();
        for (root, name) in roots {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            for path in [canonical, root.clone()] {
                let raw = path.display().to_string();
                for form in [raw.replace('\\', "\\\\"), raw.replace('\\', "/"), raw] {
                    text = text.replace(&form, name);
                }
            }
        }
        text.replace("\\\\", "/").replace('\\', "/")
    }
}

pub fn json(ran: &Ran) -> Result<Value, Failure> {
    Ok(serde_json::from_str(&ran.stdout)?)
}

pub fn make_writable(file: &Path) -> Result<(), Failure> {
    let parent = file.parent().ok_or("the file has no directory")?;
    let plain = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::set_permissions(file, plain.as_file().metadata()?.permissions())?;
    Ok(())
}
