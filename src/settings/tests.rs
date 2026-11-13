use super::*;

fn machine(memory: Option<u64>) -> Machine {
    Machine {
        processors: 4,
        memory,
        open_files: Some(1024),
        ..Machine::default()
    }
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs: Vec<(String, OsString)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    }
}

fn read(pairs: &[(&str, &str)], memory: Option<u64>) -> Result<Settings, Error> {
    Settings::read(&env(pairs), File::default(), machine(memory))
}

fn file(text: &str) -> Result<File, Error> {
    let path = PathBuf::from("/home/ada/.config/fetchloom/settings.toml");
    parse(&path, text).map(|values| File {
        path: Some(path),
        values,
        found: true,
        bypassed: None,
    })
}

#[test]
fn the_build_floor_is_a_size_or_left_to_the_cache_disk() {
    assert_eq!(read(&[], None).unwrap().build_floor, Set::default(None));
    assert_eq!(
        read(&[("FETCHLOOM_BUILD_FLOOR", "20G")], None)
            .unwrap()
            .build_floor,
        Set {
            value: Some(20 << 30),
            origin: Origin::Environment
        }
    );
    let set = Settings::read(
        &env(&[]),
        file("build_floor = \"500M\"\n").unwrap(),
        machine(None),
    )
    .unwrap();
    assert_eq!(
        set.build_floor,
        Set {
            value: Some(500 << 20),
            origin: Origin::File
        }
    );
    let error = read(&[("FETCHLOOM_BUILD_FLOOR", "lots")], None).unwrap_err();
    assert_eq!(
        error.message,
        "FETCHLOOM_BUILD_FLOOR is \"lots\", which is not a size such as 512M or 4G"
    );
}

#[test]
fn unset_variables_keep_the_defaults() {
    let settings = read(&[], None).unwrap();
    assert_eq!(settings.jobs, Set::default(JOBS));
    assert_eq!(settings.host_jobs, Set::default(HOST_JOBS));
    assert_eq!(settings.timeout, Set::default(TIMEOUT));
    assert_eq!(settings.ask_above, Set::default(ASK_ABOVE));
    assert_eq!(settings.memory, Set::default(LEAST_MEMORY));
    let empty = read(&[("FETCHLOOM_JOBS", ""), ("FETCHLOOM_MEMORY", "")], None).unwrap();
    assert_eq!(empty.jobs, Set::default(JOBS));
}

#[test]
fn the_memory_budget_is_a_quarter_of_what_the_machine_gives_within_bounds() {
    for (room, budget) in [
        (Some(100 << 20), 50 << 20),
        (Some(600 << 20), LEAST_MEMORY),
        (Some(2 << 30), 512 << 20),
        (Some(64 << 30), MOST_MEMORY),
    ] {
        let settings = read(&[], room).unwrap();
        assert_eq!(settings.memory.value, budget, "{room:?}");
        assert_eq!(settings.memory.origin, Origin::Machine);
    }
    let set = read(&[("FETCHLOOM_MEMORY", "64G")], Some(1 << 30)).unwrap();
    assert_eq!(
        set.memory,
        Set {
            value: 64 << 30,
            origin: Origin::Environment
        }
    );
}

#[test]
fn variables_override_and_name_their_origin() {
    let settings = read(
        &[
            ("FETCHLOOM_JOBS", " 64 "),
            ("FETCHLOOM_HOST_JOBS", "16"),
            ("FETCHLOOM_TIMEOUT", "300"),
            ("FETCHLOOM_ASK_ABOVE", "50G"),
        ],
        None,
    )
    .unwrap();
    let set = |value| Set {
        value,
        origin: Origin::Environment,
    };
    assert_eq!(settings.jobs, set(64));
    assert_eq!(settings.host_jobs, set(16));
    assert_eq!(settings.ask_above.value, 50 << 30);
    assert_eq!(
        settings.timeout,
        Set {
            value: Duration::from_secs(300),
            origin: Origin::Environment
        }
    );
}

#[test]
fn the_settings_file_sets_what_the_environment_leaves_and_names_its_errors() {
    let file = file("jobs = 32\nmemory = \"8G\"\ntimeout = 120\ncache = \"cache\"\n").unwrap();
    assert_eq!(
        file.cache(),
        Some(PathBuf::from("/home/ada/.config/fetchloom/cache"))
    );
    let settings = Settings::read(&env(&[("FETCHLOOM_JOBS", "4")]), file, machine(None)).unwrap();
    assert_eq!(
        settings.jobs,
        Set {
            value: 4,
            origin: Origin::Environment
        }
    );
    assert_eq!(
        settings.memory,
        Set {
            value: 8 << 30,
            origin: Origin::File
        }
    );
    assert_eq!(settings.timeout.value, Duration::from_secs(120));
    let error =
        Settings::read(&env(&[]), self::file("jobs = 0\n").unwrap(), machine(None)).unwrap_err();
    assert_eq!(
        error.message,
        "jobs in the settings file /home/ada/.config/fetchloom/settings.toml is \"0\", which is not a number from 1 to 256"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "remove jobs from the settings file to use the default of 8, or set it to a number from 1 to 256"
        )
    );
    let unknown = self::file("job = 3\n").unwrap_err();
    assert_eq!(unknown.kind, Kind::Usage);
    assert_eq!(
        unknown.message,
        "the settings file /home/ada/.config/fetchloom/settings.toml has \"job\", which is not a setting"
    );
    let wrong = self::file("jobs = true\n").unwrap_err();
    assert_eq!(
        wrong.message,
        "jobs in the settings file /home/ada/.config/fetchloom/settings.toml is a boolean, not a whole number or text in quotes"
    );
    assert!(self::file("jobs = [").is_err());
}

/// an unreadable file never counts as no file, which would leave its settings out. here
/// `FETCHLOOM_SETTINGS` names a folder.
#[test]
fn a_settings_file_that_cannot_be_read_is_an_error_not_a_missing_file() {
    let folder = tempfile::tempdir().unwrap();
    let named = folder.path().to_str().unwrap();
    let Err(error) = File::read(&env(&[("FETCHLOOM_SETTINGS", named)])) else {
        panic!("a folder was read as the settings file");
    };
    assert_eq!(error.kind, Kind::LocalIo);
    let reading = format!("could not read the settings file {named}");
    // windows refuses to open a folder as a file, and says so as a denied permission
    if cfg!(windows) {
        assert!(error.message.starts_with(&reading), "{}", error.message);
    } else {
        assert_eq!(
            error.message,
            format!("{reading}: it is a folder, not a file")
        );
        assert_eq!(
            error.help.as_deref(),
            Some("unset FETCHLOOM_SETTINGS, or set it to a settings file fetchloom may read")
        );
    }
}

/// the error never shows the text where reading stopped, which may hold a login.
#[test]
fn a_settings_file_that_is_not_toml_names_where_reading_stopped() {
    let error = file("jobs = 16\n[extra\n").unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert_eq!(
        error.message,
        "the settings file /home/ada/.config/fetchloom/settings.toml is not valid TOML at line 2, column 7: unclosed table, expected `]`"
    );
    let error = file("jobs = 16\nmirrors = [\"https://ada:hunter2@lab.example/m\"\n").unwrap_err();
    assert!(error.message.contains(" at line "), "{}", error.message);
    assert!(!error.message.contains("hunter2"), "{}", error.message);
}

/// an unknown key far from every setting gets the list of settings.
#[test]
fn a_mistyped_key_is_told_the_setting_it_is_near() {
    let typo = file("jbos = 16\n").unwrap_err();
    assert_eq!(
        typo.message,
        "the settings file /home/ada/.config/fetchloom/settings.toml has \"jbos\", which is not a setting"
    );
    assert_eq!(typo.help.as_deref(), Some("did you mean `jobs`?"));
    let unknown = file("colour = \"red\"\n").unwrap_err();
    assert_eq!(unknown.kind, Kind::Usage);
    assert_eq!(
        unknown.help.as_deref(),
        Some(
            "the settings file takes cache, resources, jobs, host_jobs, retries, timeout, ask_above, memory, most_files, archive_files, archive_size, archive_ratio, allowed_origins, s3_buckets, gcs_buckets, azure_containers, drive_folders, mirrors, mirrors_only, push_to, build_floor, each a number or text"
        )
    );
}

#[test]
fn the_most_files_a_dataset_lists_follow_the_memory_budget() {
    let gib = 1 << 30;
    let most = |memory: Option<u64>, pairs: &[(&str, &str)]| {
        Settings::read(&env(pairs), File::default(), machine(memory))
            .unwrap()
            .most_files
    };
    // 32 GiB available: a 4 GiB budget, at 2 KiB a file
    assert_eq!(
        most(Some(32 * gib), &[]),
        Set {
            value: 2_097_152,
            origin: Origin::Machine
        }
    );
    // 12 GiB available: a 3 GiB budget below the cap, which lean's own 128 MiB never shrinks
    let sized = Set {
        value: 1_572_864,
        origin: Origin::Machine,
    };
    assert_eq!(most(Some(12 * gib), &[]), sized);
    assert_eq!(
        most(Some(12 * gib), &[("FETCHLOOM_RESOURCES", "lean")]),
        sized
    );
    // a small pod never lists fewer than fetchloom always allowed
    assert_eq!(
        most(Some(100 << 20), &[]),
        Set::default(crate::source::MOST_FILES)
    );
    assert_eq!(most(None, &[]).value, 131_072);
    assert_eq!(most(None, &[("FETCHLOOM_MEMORY", "32G")]).value, 16_777_216);
    assert_eq!(
        most(Some(32 * gib), &[("FETCHLOOM_MOST_FILES", "5000000")]),
        Set {
            value: 5_000_000,
            origin: Origin::Environment
        }
    );
    let error = Settings::read(
        &env(&[("FETCHLOOM_MOST_FILES", "0")]),
        File::default(),
        machine(None),
    )
    .unwrap_err();
    assert_eq!(
        error.message,
        "FETCHLOOM_MOST_FILES is \"0\", which is not a number from 1 to 1000000000"
    );
}

#[test]
fn archives_unpack_within_their_settings_and_a_share_of_the_memory() {
    let defaults = crate::archive::Limits::default();
    let limits = |pairs: &[(&str, &str)]| {
        Settings::read(&env(pairs), File::default(), machine(None))
            .unwrap()
            .archive_limits()
    };
    assert_eq!(limits(&[]), defaults);
    let raised = limits(&[
        ("FETCHLOOM_ARCHIVE_FILES", "50000000"),
        ("FETCHLOOM_ARCHIVE_SIZE", "64T"),
        ("FETCHLOOM_ARCHIVE_RATIO", "5000"),
        ("FETCHLOOM_MEMORY", "16G"),
    ]);
    assert_eq!(
        raised,
        crate::archive::Limits {
            members: 50_000_000,
            bytes: 64 << 40,
            ratio: 5_000,
            memory: 4 << 30,
        }
    );
    // a small budget never takes decoders below what fetchloom always allowed
    assert_eq!(
        limits(&[("FETCHLOOM_MEMORY", "64M")]).memory,
        defaults.memory
    );
}

#[test]
fn a_bad_value_is_a_usage_error_naming_the_variable_and_what_it_takes() {
    for (name, value, said, help) in [
        (
            "FETCHLOOM_HOST_JOBS",
            "0",
            "FETCHLOOM_HOST_JOBS is \"0\", which is not a number from 1 to 256",
            "unset FETCHLOOM_HOST_JOBS to use the default of 4, or set it to a number from 1 to 256",
        ),
        (
            "FETCHLOOM_TIMEOUT",
            "soon",
            "FETCHLOOM_TIMEOUT is \"soon\", which is not a number from 1 to 3600",
            "unset FETCHLOOM_TIMEOUT to use the default of 30, or set it to a number from 1 to 3600",
        ),
        (
            "FETCHLOOM_MEMORY",
            "4gb",
            "FETCHLOOM_MEMORY is \"4gb\", which is not a size such as 512M or 4G",
            "unset FETCHLOOM_MEMORY to use the default of 256.0 MiB, or set it to a size such as 512M or 4G",
        ),
    ] {
        let error = read(&[(name, value)], None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{name}");
        assert_eq!(error.message, said, "{name}");
        assert_eq!(error.help.as_deref(), Some(help), "{name}");
    }
}

#[test]
fn lean_keeps_required_checksums_on_existing_workers() {
    for (level, processors, ceiling, expected) in [
        ("default", 12, 8, 8),
        ("default", 12, 2, 2),
        ("lean", 12, 2, 1),
        ("max", 12, 8, 8),
        ("max", 1, 8, 1),
    ] {
        let mut settings = read(&[("FETCHLOOM_RESOURCES", level)], None).unwrap();
        settings.machine.processors = processors;
        let cap = crate::threads::Threads::default().at_most(ceiling);
        assert_eq!(
            cap.limit(settings.hash_workers()),
            expected,
            "{level} on {processors} CPUs"
        );
    }
    let mut explicit = read(
        &[("FETCHLOOM_RESOURCES", "lean"), ("FETCHLOOM_THREADS", "8")],
        None,
    )
    .unwrap();
    explicit.machine.processors = 12;
    let cap = crate::threads::Threads::default().at_most(8);
    assert_eq!(
        cap.limit(explicit.hash_workers()),
        8,
        "a named ceiling overrides the level"
    );
}

#[test]
fn each_download_queues_its_share_of_the_memory_budget() {
    for (jobs, memory, batches) in [
        ("8", "256M", 16),
        ("8", "64M", 8),
        ("64", "256M", 4),
        ("256", "256M", 1),
        ("1", "4G", 16),
    ] {
        let settings = read(
            &[("FETCHLOOM_JOBS", jobs), ("FETCHLOOM_MEMORY", memory)],
            None,
        )
        .unwrap();
        assert_eq!(settings.hash_batches(), batches, "{jobs} jobs, {memory}");
    }
}

#[test]
fn retries_come_from_the_setting_within_its_range() {
    let retries = |value: &str| read(&[("FETCHLOOM_RETRIES", value)], None);
    assert_eq!(read(&[], None).unwrap().retries.value, crate::net::RETRIES);
    assert_eq!(retries("").unwrap().retries.value, crate::net::RETRIES);
    assert_eq!(retries("0").unwrap().retries.value, 0);
    assert_eq!(retries(" 12 ").unwrap().retries.value, 12);
    assert_eq!(
        retries("20").unwrap().retries.value,
        crate::net::MOST_RETRIES
    );
    for bad in ["21", "-1", "many", "1.5"] {
        let error = retries(bad).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{bad}");
        assert_eq!(
            error.message,
            format!("FETCHLOOM_RETRIES is \"{bad}\", which is not a number from 0 to 20")
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "unset FETCHLOOM_RETRIES to use the default of 4, or set it to a number from 0 to 20"
            )
        );
    }
}

#[test]
fn allowed_origins_are_exact_origins_from_the_environment_or_the_file() {
    let shown = |settings: &Settings| -> Vec<String> {
        settings
            .allowed_origins
            .value
            .iter()
            .map(url::Origin::ascii_serialization)
            .collect()
    };
    assert_eq!(
        read(&[], None).unwrap().allowed_origins,
        Set::default(vec![])
    );
    let set = read(
        &[(
            "FETCHLOOM_ALLOWED_ORIGINS",
            " http://LAB.local:8080, https://10.0.0.5:443/ ,",
        )],
        None,
    )
    .unwrap();
    assert_eq!(shown(&set), ["http://lab.local:8080", "https://10.0.0.5"]);
    assert_eq!(set.allowed_origins.origin, Origin::Environment);
    let file = file("allowed_origins = [\"http://lab.local:8080\", \"http://[::1]:9000\"]\n");
    let set = Settings::read(&env(&[]), file.unwrap(), machine(None)).unwrap();
    assert_eq!(shown(&set), ["http://lab.local:8080", "http://[::1]:9000"]);
    assert_eq!(set.allowed_origins.origin, Origin::File);
    for entry in [
        "lab.local:8080",
        "http://lab.local:8080/data",
        "ftp://lab.local",
        "http://ada:secret@lab.local",
        "http://lab.local/?all=1",
    ] {
        let error = read(&[("FETCHLOOM_ALLOWED_ORIGINS", entry)], None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{entry}");
        assert_eq!(
            error.message,
            format!(
                "FETCHLOOM_ALLOWED_ORIGINS holds \"{entry}\", which is not an origin such as http://lab.local:8080"
            )
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "unset FETCHLOOM_ALLOWED_ORIGINS to allow none, or list origins as a scheme, a host and a port when it is not the usual one, such as http://lab.local:8080, separated by commas"
            )
        );
    }
    let wrong = self::file("allowed_origins = [8080]\n").unwrap_err();
    assert_eq!(
        wrong.message,
        "allowed_origins in the settings file /home/ada/.config/fetchloom/settings.toml holds a integer, not text in quotes"
    );
    let elsewhere = self::file("jobs = [32]\n").unwrap_err();
    assert!(
        elsewhere.message.contains("is a array"),
        "{}",
        elsewhere.message
    );
}

#[test]
fn s3_buckets_name_aws_buckets_or_buckets_at_another_service() {
    let shown = |settings: &Settings| -> Vec<String> {
        settings
            .s3_buckets
            .value
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    assert_eq!(read(&[], None).unwrap().s3_buckets, Set::default(vec![]));
    let set = read(
        &[(
            "FETCHLOOM_S3_BUCKETS",
            " lab-raw, s3://lab.data/ ,s3:https://MINIO.lab:9000/scans/,s3:http://10.0.0.5:80/old-scans,",
        )],
        None,
    )
    .unwrap();
    assert_eq!(
        shown(&set),
        [
            "s3://lab-raw",
            "s3://lab.data",
            "s3:https://minio.lab:9000/scans",
            "s3:http://10.0.0.5/old-scans"
        ]
    );
    assert_eq!(set.s3_buckets.origin, Origin::Environment);
    let file = file("s3_buckets = [\"s3://lab-raw\"]\n").unwrap();
    let set = Settings::read(&env(&[]), file, machine(None)).unwrap();
    assert_eq!(shown(&set), ["s3://lab-raw"]);
    assert_eq!(set.s3_buckets.origin, Origin::File);
    for (entry, said) in [
        ("s3://Lab-Raw", "s3://Lab-Raw"),
        ("gs://lab-raw", "gs://lab-raw"),
        ("s3://lab-raw/prefix/", "s3://lab-raw/prefix/"),
        (
            "s3:https://minio.lab/scans/raw",
            "s3:https://minio.lab/scans/raw",
        ),
        (
            "s3:https://minio.lab/scans?x=1",
            "s3:https://minio.lab/scans?x=***",
        ),
        ("s3:ftp://minio.lab/scans", "s3:ftp://minio.lab/scans"),
        (
            "s3:https://ada:hunter2@minio.lab/scans",
            "s3:https://***@minio.lab/scans",
        ),
    ] {
        let error = read(&[("FETCHLOOM_S3_BUCKETS", entry)], None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{entry}");
        assert_eq!(
            error.message,
            format!(
                "FETCHLOOM_S3_BUCKETS holds \"{said}\", which is not a bucket such as s3://lab-data or s3:https://minio.lab:9000/scans"
            )
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "unset FETCHLOOM_S3_BUCKETS to sign for none, or list buckets as s3://name, or as s3:https://host:port/name at another S3 service, separated by commas"
            )
        );
    }
}

#[test]
fn mirrors_come_from_the_variable_or_else_the_file_in_their_order() {
    let folder = |path: &str| Location::Folder(PathBuf::from(path));
    let set = read(
        &[(
            "FETCHLOOM_MIRRORS",
            " /mnt/lab/fetchloom , https://data.corp.internal/fetchloom,",
        )],
        None,
    )
    .unwrap();
    assert_eq!(set.mirrors.origin, Origin::Environment);
    assert_eq!(
        set.mirrors.value,
        [
            folder("/mnt/lab/fetchloom"),
            Location::parse("https://data.corp.internal/fetchloom", None).unwrap(),
        ]
    );
    // a folder whose name holds a comma keeps it in the file's list
    let listed = file("mirrors = [\"mirror\", \"/mnt/a,b\"]\nmirrors_only = true\n").unwrap();
    let set = Settings::read(&env(&[]), listed.clone(), machine(None)).unwrap();
    assert_eq!(set.mirrors.origin, Origin::File);
    assert_eq!(
        set.mirrors.value,
        [
            folder("/home/ada/.config/fetchloom/mirror"),
            folder("/mnt/a,b")
        ]
    );
    assert_eq!(
        set.mirrors_only,
        Set {
            value: true,
            origin: Origin::File
        }
    );
    let set = Settings::read(
        &env(&[
            ("FETCHLOOM_MIRRORS", "/usb"),
            ("FETCHLOOM_MIRRORS_ONLY", "0"),
        ]),
        listed,
        machine(None),
    )
    .unwrap();
    assert_eq!(set.mirrors.value, [folder("/usb")]);
    assert!(!set.mirrors_only.value);
    assert_eq!(read(&[], None).unwrap().mirrors, Set::default(Vec::new()));
    assert_eq!(read(&[], None).unwrap().mirrors_only, Set::default(false));
}

#[test]
fn a_mirror_that_is_no_folder_or_web_address_is_a_usage_error() {
    let error = read(&[("FETCHLOOM_MIRRORS", "file:///mnt/lab")], None).unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert_eq!(
        error.message,
        "FETCHLOOM_MIRRORS holds \"file:///mnt/lab\", which is no mirror: a file:// address names a folder"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("write the folder path itself, or unset FETCHLOOM_MIRRORS to ask no mirror")
    );
    let error = Settings::read(
        &env(&[]),
        file("mirrors = [\"https://ada:hunter2@lab.example/m\"]\n").unwrap(),
        machine(None),
    )
    .unwrap_err();
    assert!(!error.message.contains("hunter2"), "{}", error.message);
    assert!(
        error
            .help
            .as_deref()
            .is_some_and(|help| help.contains("`fetchloom auth login lab.example`")
                && help.ends_with("remove mirrors from the settings file to ask no mirror")),
        "{:?}",
        error.help
    );
    let error = read(&[("FETCHLOOM_MIRRORS_ONLY", "yes")], None).unwrap_err();
    assert_eq!(
        error.message,
        "FETCHLOOM_MIRRORS_ONLY is \"yes\", which is not true or false"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "unset FETCHLOOM_MIRRORS_ONLY to use the default of false, or set it to true or false"
        )
    );
    let wrong = file("mirrors_only = [true]\n").unwrap_err();
    assert_eq!(wrong.kind, Kind::Usage);
}

/// an empty variable keeps a run meant to reach the sources from taking the file's mirror.
/// other settings still fall back to the file.
#[test]
fn an_empty_variable_clears_the_files_mirrors_and_push_folder() {
    let filed =
        file("mirrors = [\"/mnt/lab\"]\nmirrors_only = true\npush_to = \"/mnt/lab\"\njobs = 32\n")
            .unwrap();
    for empty in ["", " "] {
        let set = Settings::read(
            &env(&[
                ("FETCHLOOM_MIRRORS", empty),
                ("FETCHLOOM_MIRRORS_ONLY", empty),
                ("FETCHLOOM_PUSH_TO", empty),
                ("FETCHLOOM_JOBS", ""),
            ]),
            filed.clone(),
            machine(None),
        )
        .unwrap();
        assert_eq!(
            set.mirrors,
            Set {
                value: Vec::new(),
                origin: Origin::Environment
            },
            "{empty:?}"
        );
        assert_eq!(
            set.mirrors_only,
            Set {
                value: false,
                origin: Origin::Environment
            },
            "{empty:?}"
        );
        assert_eq!(
            set.push_to,
            Set {
                value: None,
                origin: Origin::Environment
            },
            "{empty:?}"
        );
        assert_eq!(
            set.jobs,
            Set {
                value: 32,
                origin: Origin::File
            }
        );
    }
}

#[test]
fn gcs_buckets_name_cloud_storage_buckets() {
    let shown = |settings: &Settings| -> Vec<String> {
        settings
            .gcs_buckets
            .value
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    assert_eq!(read(&[], None).unwrap().gcs_buckets, Set::default(vec![]));
    let set = read(
        &[(
            "FETCHLOOM_GCS_BUCKETS",
            " lab-raw, gs://lab.data/ ,GS://lab_scans,lab-raw,",
        )],
        None,
    )
    .unwrap();
    assert_eq!(
        shown(&set),
        ["gs://lab-raw", "gs://lab.data", "gs://lab_scans"]
    );
    assert_eq!(set.gcs_buckets.origin, Origin::Environment);
    let filed = file("gcs_buckets = [\"gs://lab-raw\"]\n").unwrap();
    let set = Settings::read(&env(&[]), filed.clone(), machine(None)).unwrap();
    assert_eq!(shown(&set), ["gs://lab-raw"]);
    assert_eq!(set.gcs_buckets.origin, Origin::File);
    // a blank variable clears what the file lists, so no request carries a token for it
    let cleared = Settings::read(
        &env(&[("FETCHLOOM_GCS_BUCKETS", " ")]),
        filed,
        machine(None),
    )
    .unwrap();
    assert_eq!(
        cleared.gcs_buckets,
        Set {
            value: Vec::new(),
            origin: Origin::Environment
        }
    );
    for entry in [
        "gs://Lab-Raw",
        "s3://lab-raw",
        "gs://lab-raw/prefix/",
        "https://storage.googleapis.com/lab-raw",
        "gs://ab",
    ] {
        let error = read(&[("FETCHLOOM_GCS_BUCKETS", entry)], None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{entry}");
        assert_eq!(
            error.message,
            format!(
                "FETCHLOOM_GCS_BUCKETS holds \"{entry}\", which is not a bucket such as gs://lab-data"
            )
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "unset FETCHLOOM_GCS_BUCKETS to send google credentials to none, or list buckets as gs://name, separated by commas"
            )
        );
    }
}

/// a container is named by account and container, or by a link that may carry its own SAS,
/// which never shows, not even in a refused entry.
#[test]
fn azure_containers_name_containers_and_keep_their_sas_unshown() {
    let shown = |settings: &Settings| -> Vec<String> {
        settings
            .azure_containers
            .value
            .iter()
            .map(|container| format!("{container:?}"))
            .collect()
    };
    assert_eq!(
        read(&[], None).unwrap().azure_containers,
        Set::default(vec![])
    );
    let set = read(
        &[(
            "FETCHLOOM_AZURE_CONTAINERS",
            " azure:Lab/raw, https://lab.blob.core.windows.net/scans/ ,azure:lab/raw/,",
        )],
        None,
    )
    .unwrap();
    assert_eq!(
        shown(&set),
        [
            "AzureContainer(azure:lab/raw)",
            "AzureContainer(azure:lab/scans)"
        ]
    );
    assert_eq!(set.azure_containers.origin, Origin::Environment);
    let filed = file(
        "azure_containers = [\"https://lab.blob.core.windows.net/raw?sv=2022-11-02&sr=c&sp=rl&sig=abc%3D\"]\n",
    )
    .unwrap();
    let set = Settings::read(&env(&[]), filed.clone(), machine(None)).unwrap();
    assert_eq!(shown(&set), ["AzureContainer(azure:lab/raw with a SAS)"]);
    assert_eq!(
        set.azure_containers.value[0]
            .sas()
            .map(crate::source::Sas::query),
        Some("sv=2022-11-02&sr=c&sp=rl&sig=abc%3D")
    );
    assert_eq!(set.azure_containers.origin, Origin::File);
    let cleared = Settings::read(
        &env(&[("FETCHLOOM_AZURE_CONTAINERS", " ")]),
        filed,
        machine(None),
    )
    .unwrap();
    assert_eq!(
        cleared.azure_containers,
        Set {
            value: Vec::new(),
            origin: Origin::Environment
        }
    );
    for (entry, said) in [
        ("azure:lab", "azure:lab"),
        ("azure:lab/Raw", "azure:lab/Raw"),
        ("gs://lab-raw", "gs://lab-raw"),
        (
            "http://lab.blob.core.windows.net/raw",
            "http://lab.blob.core.windows.net/raw",
        ),
        (
            "https://lab.blob.core.windows.net/raw/scans",
            "https://lab.blob.core.windows.net/raw/scans",
        ),
        (
            "https://lab.blob.core.windows.net/raw?sp=rl&sig=abc",
            "https://lab.blob.core.windows.net/raw?sp=***&sig=***",
        ),
    ] {
        let error = read(&[("FETCHLOOM_AZURE_CONTAINERS", entry)], None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{entry}");
        assert_eq!(
            error.message,
            format!(
                "FETCHLOOM_AZURE_CONTAINERS holds \"{said}\", which is not a container such as azure:lab/raw or its link"
            )
        );
    }
}

#[test]
fn drive_folders_name_drive_ids() {
    const LAB: &str = "1AbCdEfGhIjKlMnOpQrStUvWxYz012345";
    const SCANS: &str = "0B-ScansFolder_2019";
    const NOTES: &str = "1notesFileIdOfTheLab";
    let shown = |settings: &Settings| -> Vec<String> {
        settings
            .drive_folders
            .value
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    assert_eq!(read(&[], None).unwrap().drive_folders, Set::default(vec![]));
    let listed = format!(
        " {LAB}, https://drive.google.com/drive/folders/{SCANS}?usp=sharing ,drive:{NOTES},https://drive.google.com/file/d/{LAB}/view,"
    );
    let set = read(&[("FETCHLOOM_DRIVE_FOLDERS", listed.as_str())], None).unwrap();
    assert_eq!(shown(&set), [LAB, SCANS, NOTES]);
    assert_eq!(set.drive_folders.origin, Origin::Environment);
    let filed = file(&format!("drive_folders = [\"drive:folders/{SCANS}\"]\n")).unwrap();
    let set = Settings::read(&env(&[]), filed.clone(), machine(None)).unwrap();
    assert_eq!(shown(&set), [SCANS]);
    assert_eq!(set.drive_folders.origin, Origin::File);
    // a blank variable clears what the file lists, so nothing on drive gets a token
    let cleared = Settings::read(
        &env(&[("FETCHLOOM_DRIVE_FOLDERS", " ")]),
        filed,
        machine(None),
    )
    .unwrap();
    assert_eq!(
        cleared.drive_folders,
        Set {
            value: Vec::new(),
            origin: Origin::Environment
        }
    );
    for entry in [
        "lab-data",
        "gs://lab-private",
        "drive:folders/short",
        "https://example.org/drive/folders/1AbCdEfGhIjKlMnOpQrStUvWxYz012345",
        "1AbCdEfGhIjKlMnOp/QrStUvWxYz012345",
    ] {
        let error = read(&[("FETCHLOOM_DRIVE_FOLDERS", entry)], None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{entry}");
        assert_eq!(
            error.message,
            format!(
                "FETCHLOOM_DRIVE_FOLDERS holds \"{entry}\", which is not a google drive folder or file: its id or the link drive gives when you share it"
            )
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "unset FETCHLOOM_DRIVE_FOLDERS to send google credentials to drive for nothing, or list ids or sharing links, separated by commas"
            )
        );
    }
}

/// an empty variable leaves no request signed for the file's buckets.
#[test]
fn an_empty_variable_clears_the_files_s3_buckets() {
    let filed = file("s3_buckets = [\"s3://lab-raw\"]\n").unwrap();
    for empty in ["", " "] {
        let set = Settings::read(
            &env(&[("FETCHLOOM_S3_BUCKETS", empty)]),
            filed.clone(),
            machine(None),
        )
        .unwrap();
        assert_eq!(
            set.s3_buckets,
            Set {
                value: Vec::new(),
                origin: Origin::Environment
            },
            "{empty:?}"
        );
    }
}

/// one typo means a character added, dropped, changed or swapped; other names are left alone.
#[test]
fn a_name_one_typo_from_a_variable_is_named_with_it() {
    // put together, since the docs check takes each whole FETCHLOOM_ name in src for a setting
    let prefixed = |rest: &str| format!("FETCHLOOM_{rest}");
    let names = [
        prefixed("OFLINE"),
        prefixed("SPILT"),
        prefixed("MIRROR"),
        prefixed("JOBZ"),
        "FETCHLOM_CACHE".to_owned(),
        prefixed("OFFLINEE"),
        "FETCHLOOM_JOBS".to_owned(),
        "FETCHLOOM_MIRRORS".to_owned(),
        "FETCHLOOM_TEST_TICKER".to_owned(),
        prefixed("SOMETHING_ELSE"),
        prefixed("JBSO"),
        "PATH".to_owned(),
    ];
    assert_eq!(
        misnamed(names.map(OsString::from)),
        [
            (prefixed("OFLINE"), "FETCHLOOM_OFFLINE"),
            (prefixed("SPILT"), "FETCHLOOM_SPLIT"),
            (prefixed("MIRROR"), "FETCHLOOM_MIRRORS"),
            (prefixed("JOBZ"), "FETCHLOOM_JOBS"),
            ("FETCHLOM_CACHE".to_owned(), "FETCHLOOM_CACHE"),
            (prefixed("OFFLINEE"), "FETCHLOOM_OFFLINE"),
        ]
    );
}

/// setting a documented variable is never taken for a typo.
#[test]
fn every_documented_variable_is_known() {
    let page = include_str!("../../docs/user/reference/environment.md");
    let documented: Vec<&str> = page
        .lines()
        .filter_map(|line| line.strip_prefix("| `FETCHLOOM_"))
        .filter_map(|rest| rest.split('`').next())
        .collect();
    assert!(documented.len() > 20, "{documented:?}");
    for name in documented {
        let name = format!("FETCHLOOM_{name}");
        assert!(known().any(|known| known == name), "{name}");
    }
}

#[test]
fn push_to_is_a_folder_from_the_variable_or_else_the_file() {
    assert_eq!(read(&[], None).unwrap().push_to, Set::default(None));
    let set = read(&[("FETCHLOOM_PUSH_TO", " /mnt/lab/fetchloom ")], None).unwrap();
    assert_eq!(
        set.push_to,
        Set {
            value: Some(PathBuf::from("/mnt/lab/fetchloom")),
            origin: Origin::Environment
        }
    );
    // a relative folder is taken from the settings file's folder, as mirrors are
    let filed = file("push_to = \"mirror\"\n").unwrap();
    let set = Settings::read(&env(&[]), filed, machine(None)).unwrap();
    assert_eq!(
        set.push_to,
        Set {
            value: Some(PathBuf::from("/home/ada/.config/fetchloom/mirror")),
            origin: Origin::File
        }
    );
}

/// push writes folders only; a bucket or a server gets the folder through rclone.
#[test]
fn push_to_a_web_address_is_a_usage_error() {
    let error = read(
        &[("FETCHLOOM_PUSH_TO", "https://data.lab.example/fetchloom")],
        None,
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert_eq!(
        error.message,
        "FETCHLOOM_PUSH_TO is \"https://data.lab.example/fetchloom\", which is a web address, and push writes to folders"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "set it to a folder, such as /mnt/lab/fetchloom, and copy that folder to a server or a bucket with a tool such as rclone; or unset FETCHLOOM_PUSH_TO"
        )
    );
    let error = Settings::read(
        &env(&[]),
        file("push_to = \"file:///mnt/lab\"\n").unwrap(),
        machine(None),
    )
    .unwrap_err();
    assert_eq!(
        error.help.as_deref(),
        Some("write the folder path itself, or remove push_to from the settings file")
    );
}
