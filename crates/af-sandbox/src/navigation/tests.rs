use super::*;
use serde_json::json;

fn fixture() -> Broker {
    Broker::new(BTreeMap::from([
        (
            "src/lib.rs".into(),
            b"pub fn synthetic() {}\n// needle\n".to_vec(),
        ),
        ("README.md".into(), b"synthetic source\n".to_vec()),
    ]))
    .unwrap()
}
#[test]
fn expired_subject_deadline_refuses_before_creating_a_container() {
    let mut broker = fixture();
    let identity = broker.source_identity().to_owned();
    assert!(
        broker
            .execute_until(&Request::ListPaths { offset: 0 }, Instant::now())
            .is_err()
    );
    // A preflight refusal has no ambiguous create to preserve or disable the broker for.
    assert!(broker.staging.is_some());
    assert_eq!(broker.source_identity(), identity);
    assert!(remaining(Instant::now()).is_err());
    assert!(
        remaining(Instant::now() + Duration::from_secs(600)).unwrap() <= Duration::from_secs(30)
    );
}
#[test]
fn admission_denies_escapes_unknown_tools_owner_input_and_malformed_requests() {
    for tool in [
        "exec",
        "exec_command",
        "build",
        "test",
        "write",
        "network",
        "request_user_input",
        "apply_patch",
    ] {
        assert!(Request::parse(&serde_json::to_vec(&json!({"tool":tool})).unwrap()).is_err());
    }
    for path in [
        "../oracle",
        "/etc/passwd",
        "src/../../checkout",
        "../sibling",
        ".git/config",
        "/root/.ssh/id_rsa",
        "a//b",
        "a/./b",
        "a\\b",
        "",
        "é",
        "a/../b",
    ] {
        assert!(
            Request::parse(
                &serde_json::to_vec(
                    &json!({"tool":"af_read_range","path":path,"offset":0,"length":1})
                )
                .unwrap()
            )
            .is_err()
        );
        assert!(Broker::new(BTreeMap::from([(path.into(), vec![])])).is_err());
    }
    for raw in [
        r#"{"tool":"af_list_paths","tool":"af_list_paths"}"#,
        r#"{"tool":"af_list_paths","command":"cargo test"}"#,
        r#"{"tool":"af_search","query":""}"#,
        r#"{"tool":"af_read_range","path":"a","offset":-1,"length":1}"#,
        r#"{"tool":"af_read_range","path":"a","offset":0,"length":65537}"#,
        "null",
        "{",
    ] {
        assert!(Request::parse(raw.as_bytes()).is_err(), "{raw}");
    }
    assert!(Request::parse(&vec![b' '; 4097]).is_err());
    assert!(
        Broker::new(BTreeMap::from([(
            "a".into(),
            vec![b'x'; 8 * 1024 * 1024 + 1]
        )]))
        .is_err()
    );
}
#[test]
fn wire_schema_parity() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../schemas/source-navigation-v3.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for request in [
        Request::ListPaths { offset: 0 },
        Request::ReadRange {
            path: "src/lib.rs".into(),
            offset: 0,
            length: 5,
        },
        Request::Search {
            offset: 0,
            query: "needle".into(),
        },
    ] {
        let value = serde_json::to_value(&request).unwrap();
        assert!(validator.is_valid(&value));
        Request::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    }
    for response in [
        Response::ListPaths {
            paths: vec!["a".into()],
            next_offset: None,
        },
        Response::ReadRange {
            text: Some("hello".into()),
        },
        Response::Search {
            next_offset: None,
            non_text_paths: vec![],
            hits: vec![Hit {
                path: "a".into(),
                line: 1,
                text: "hello".into(),
            }],
        },
    ] {
        let value = serde_json::to_value(&response).unwrap();
        let responses = jsonschema::validator_for(&schema["$defs"]["response"]).unwrap();
        assert!(responses.is_valid(&value));
        assert_eq!(serde_json::from_value::<Response>(value).unwrap(), response);
    }
    for value in [
        json!({"tool":"request_user_input"}),
        json!({"tool":"af_list_paths","extra":1}),
        json!({"tool":"af_read_range","path":"../oracle","offset":0,"length":1}),
    ] {
        assert!(!validator.is_valid(&value));
        assert!(Request::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
/// Explicit operator command: never substitutes a fake daemon or silently skips a missing one.
#[test]
#[ignore = "requires actual Docker and the pinned local image; run the documented conformance command"]
fn docker_conformance() {
    let mut broker = fixture();
    let staging = broker.staging.as_ref().unwrap().path().to_owned();
    assert_eq!(
        broker.execute(&Request::ListPaths { offset: 0 }).unwrap(),
        Response::ListPaths {
            paths: vec!["README.md".into(), "src/lib.rs".into()],
            next_offset: None
        }
    );
    assert_eq!(
        broker
            .execute(&Request::ReadRange {
                path: "src/lib.rs".into(),
                offset: 0,
                length: 6
            })
            .unwrap(),
        Response::ReadRange {
            text: Some("pub fn".into())
        }
    );
    assert_eq!(
        broker
            .execute(&Request::Search {
                offset: 0,
                query: "needle".into()
            })
            .unwrap(),
        Response::Search {
            next_offset: None,
            non_text_paths: vec![],
            hits: vec![Hit {
                path: "src/lib.rs".into(),
                line: 2,
                text: "// needle".into()
            }]
        }
    );
    for path in [
        "oracle.json",
        "checkout/file",
        "sibling/file",
        "history",
        "/root/.docker/config.json",
        "/var/run/docker.sock",
        "/etc/passwd",
    ] {
        assert!(
            broker
                .execute(&Request::ReadRange {
                    path: path.into(),
                    offset: 0,
                    length: 10
                })
                .is_err()
        );
    }
    // Adversarial staging corruption is test-only: host API cannot introduce a link/FIFO.
    let staging = staging.join("source");
    mode(&staging, 0o755).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", staging.join("escape")).unwrap();
    assert!(
        broker
            .execute(&Request::ReadRange {
                path: "escape".into(),
                offset: 0,
                length: 10
            })
            .is_err()
    );
    std::fs::remove_file(staging.join("escape")).unwrap();
    nix::unistd::mkfifo(&staging.join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(
        broker
            .execute(&Request::ReadRange {
                path: "fifo".into(),
                offset: 0,
                length: 10
            })
            .is_err()
    );
    std::fs::remove_file(staging.join("fifo")).unwrap();
    // Paginated search keeps every match without silently truncating the result.
    std::fs::write(staging.join("many"), "needle\n".repeat(129)).unwrap();
    let Response::Search {
        hits, next_offset, ..
    } = broker
        .execute(&Request::Search {
            query: "needle".into(),
            offset: 0,
        })
        .unwrap()
    else {
        panic!("wrong response")
    };
    assert_eq!(hits.len(), 128);
    assert_eq!(next_offset, Some(128));
    let Response::Search {
        hits, next_offset, ..
    } = broker
        .execute(&Request::Search {
            query: "needle".into(),
            offset: 128,
        })
        .unwrap()
    else {
        panic!("wrong response")
    };
    assert_eq!(hits.len(), 2); // final 'many' line and the original src/lib.rs match
    assert_eq!(next_offset, None);
    std::fs::remove_file(staging.join("many")).unwrap();
    drop(broker);
    assert!(!staging.exists());
}

#[test]
fn identities_are_deterministic_and_bound_to_source_and_wire() {
    let a = fixture();
    let b = fixture();
    assert_eq!(a.source_identity(), b.source_identity());
    let changed = Broker::new(BTreeMap::from([(
        "src/lib.rs".into(),
        b"different".to_vec(),
    )]))
    .unwrap();
    assert_ne!(a.source_identity(), changed.source_identity());
    assert!(Broker::conformance_identity().starts_with("sha256:"));
    assert_ne!(
        Broker::request_identity(&Request::ListPaths { offset: 0 }),
        Broker::request_identity(&Request::Search {
            offset: 0,
            query: "needle".into()
        })
    );
    assert_ne!(
        Broker::response_identity(&Response::ReadRange {
            text: Some("a".into())
        }),
        Broker::response_identity(&Response::ReadRange {
            text: Some("b".into())
        })
    );
}

/// Actual containment probes reuse production create flags. No model or credentials involved.
#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_substrate_conformance() {
    let mut broker = fixture();
    let stage = broker.staging.as_ref().unwrap().path().to_owned();
    let name = stage.file_name().unwrap().to_str().unwrap();
    let mount = format!(
        "type=bind,src={},dst=/source,readonly",
        stage.join("source").display()
    );
    let marker = tempfile::NamedTempFile::new().unwrap();
    let program = format!(
        r#"
import os, socket
assert os.getuid() == 65534
for path in ['/oracle', '/checkout', '/sibling', '/history', '/root/.ssh', '/var/run/docker.sock', {marker:?}]:
    assert not os.path.exists(path), path
for path in ['/source/src/lib.rs', '/source/new', '/tmp/new', '/new']:
    try:
        f = open(path, 'w')
    except OSError:
        pass
    else:
        f.close()
        raise AssertionError('write allowed: ' + path)
s = socket.socket()
s.settimeout(1)
try:
    s.connect(('1.1.1.1', 53))
except OSError:
    pass
else:
    raise AssertionError('network allowed')
assert 'NoNewPrivs:\t1' in open('/proc/self/status').read()
assert 'CapEff:\t0000000000000000' in open('/proc/self/status').read()
print('contained')
"#,
        marker = marker.path().to_str().unwrap()
    );
    create(name, &mount, &program).unwrap();
    let result = docker(&["start", "--attach", "-i", name], Some(vec![]));
    let cleanup = docker(&["rm", "--force", name], None);
    if cleanup.is_err() {
        let _ = broker.staging.take().unwrap().keep();
    }
    cleanup.unwrap();
    assert_eq!(result.unwrap(), b"contained\n");
}

#[test]
fn malformed_and_oversized_responses_fail_closed() {
    let list = Request::ListPaths { offset: 0 };
    for raw in [
        r#"{"tool":"af_list_paths","paths":["a"]}"#,
        r#"{"tool":"af_list_paths","next_offset":null,"paths":["../oracle"]}"#,
        r#"{"tool":"af_list_paths","next_offset":null,"paths":["b","a"]}"#,
        r#"{"tool":"af_list_paths","next_offset":null,"paths":["a","a"]}"#,
        r#"{"tool":"af_list_paths","next_offset":null,"paths":[],"extra":true}"#,
        r#"{"tool":"af_read_range","text":"hello"}"#,
    ] {
        assert!(decode(&list, raw.as_bytes()).is_err());
    }
    assert!(decode(&list, &vec![b' '; MAX_RESPONSE + 1]).is_err());
    let read = Request::ReadRange {
        path: "a".into(),
        offset: 0,
        length: 1,
    };
    assert!(decode(&read, br#"{"tool":"af_read_range","text":"too long"}"#).is_err());
}

#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_deadline_removes_container_and_descendants() {
    let mut broker = fixture();
    let stage = broker.staging.as_ref().unwrap().path().to_owned();
    let name = stage.file_name().unwrap().to_str().unwrap();
    // Fixed test-only program: parent and child both outlive the client's deadline.
    assert!(
        broker
            .execute_program(
                &Request::ListPaths { offset: 0 },
                "import os,time; os.fork(); time.sleep(300)",
                Duration::from_secs(3)
            )
            .is_err()
    );
    assert_eq!(
        docker(
            &[
                "container",
                "ls",
                "--all",
                "--filter",
                &format!("name=^/{name}$"),
                "--format",
                "{{.ID}}"
            ],
            None
        )
        .unwrap(),
        b""
    );
    assert!(broker.execute(&Request::ListPaths { offset: 0 }).is_ok());
}

#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_utf8_byte_boundaries_and_eof() {
    let mut broker = Broker::new(BTreeMap::from([(
        "unicode.txt".into(),
        "é".as_bytes().to_vec(),
    )]))
    .unwrap();
    assert_eq!(
        broker
            .execute(&Request::ReadRange {
                path: "unicode.txt".into(),
                offset: 0,
                length: 2
            })
            .unwrap(),
        Response::ReadRange {
            text: Some("é".into())
        }
    );
    for (offset, length) in [(0, 1), (1, 1), (3, 1)] {
        assert!(
            broker
                .execute(&Request::ReadRange {
                    path: "unicode.txt".into(),
                    offset,
                    length
                })
                .is_err()
        );
    }
    assert_eq!(
        broker
            .execute(&Request::ReadRange {
                path: "unicode.txt".into(),
                offset: 2,
                length: 1
            })
            .unwrap(),
        Response::ReadRange {
            text: Some(String::new())
        }
    );
}

#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_create_collision_does_not_remove_another_container() {
    let mut broker = fixture();
    let stage = broker.staging.as_ref().unwrap().path().to_owned();
    let name = stage.file_name().unwrap().to_str().unwrap();
    let mount = format!(
        "type=bind,src={},dst=/source,readonly",
        stage.join("source").display()
    );
    let id = String::from_utf8(create(name, &mount, "pass").unwrap()).unwrap();
    let result = broker.execute(&Request::ListPaths { offset: 0 });
    let still_exists = docker(
        &["container", "inspect", "--format", "{{.Id}}", id.trim()],
        None,
    );
    docker(&["rm", "--force", id.trim()], None).unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("staging preserved")
    );
    assert!(still_exists.is_ok());
    assert!(broker.execute(&Request::ListPaths { offset: 0 }).is_err());
    // The test owns and has now confirmed cleanup of the deliberately preserved staging.
    fn unlock(path: &Path) {
        mode(path, 0o755).unwrap();
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                unlock(&entry.path());
            }
        }
    }
    unlock(&stage);
    std::fs::remove_dir_all(stage).unwrap();
}

#[test]
fn staging_refuses_case_aliases_and_case_insensitive_history() {
    for paths in [["A", "a"], ["Src/a", "src/b"]] {
        assert!(Broker::new(paths.into_iter().map(|p| (p.into(), vec![])).collect()).is_err());
    }
    for path in [".GIT/config", "a/.Hg/store", ".SvN/history"] {
        assert!(Broker::new(BTreeMap::from([(path.into(), vec![])])).is_err());
        assert!(
            Request::ReadRange {
                path: path.into(),
                offset: 0,
                length: 1
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn shared_temporary_root_keeps_live_and_preserved_staging_private() {
    use std::os::unix::fs::PermissionsExt;
    // Exercise the same constructor as TMPDIR=/tmp without changing process-global environment.
    let mut broker = Broker::stage_in(
        BTreeMap::from([("src/private".into(), b"private source".to_vec())]),
        Path::new("/tmp"),
    )
    .unwrap();
    let envelope = broker.staging.as_ref().unwrap().path().to_owned();
    let permissions = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(permissions(&envelope), 0o700);
    assert_eq!(permissions(&envelope.join("source")), 0o555);
    assert_eq!(permissions(&envelope.join("source/src/private")), 0o444);
    assert!(broker.preserve().to_string().contains("staging preserved"));
    drop(broker);
    assert_eq!(permissions(&envelope), 0o700);
    // Test owns this preserved fixture; no container has been created.
    mode(&envelope.join("source"), 0o700).unwrap();
    mode(&envelope.join("source/src"), 0o700).unwrap();
    std::fs::remove_dir_all(envelope).unwrap();
}

#[test]
fn admission_and_schema_bound_pagination_and_reject_old_list_shape() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../schemas/source-navigation-v3.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for value in [
        json!({"tool":"af_list_paths"}),
        json!({"tool":"af_list_paths","offset":16385}),
        json!({"tool":"af_list_paths","offset":-1}),
    ] {
        assert!(!validator.is_valid(&value));
        assert!(Request::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for query in ["\u{b}", "\u{c}", "\u{1c}", "\u{1d}", "\u{1e}"] {
        let value = json!({"tool":"af_search","query":query,"offset":0});
        assert!(validator.is_valid(&value));
        Request::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    }
    assert!(
        Broker::new(
            (0..=MAX_FILES)
                .map(|n| (format!("file{n}"), vec![]))
                .collect()
        )
        .is_err()
    );
    for raw in [
        json!({"tool":"af_list_paths","paths":["a"],"next_offset":256}),
        json!({"tool":"af_list_paths","paths":["a"],"next_offset":0}),
    ] {
        assert!(
            decode(
                &Request::ListPaths { offset: 0 },
                &serde_json::to_vec(&raw).unwrap()
            )
            .is_err()
        );
    }
}

#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_large_source_lists_every_page_and_reads_and_searches_final_file() {
    let files: BTreeMap<_, _> = (0..4097)
        .map(|n| {
            (
                format!("src/file{n:05}.rs"),
                if n == 4096 {
                    b"final needle\n".to_vec()
                } else {
                    vec![]
                },
            )
        })
        .collect();
    let expected: Vec<_> = files.keys().cloned().collect();
    let mut broker = Broker::new(files).unwrap();
    let mut listed = Vec::new();
    let mut offset = 0;
    loop {
        let Response::ListPaths { paths, next_offset } =
            broker.execute(&Request::ListPaths { offset }).unwrap()
        else {
            panic!("wrong response")
        };
        listed.extend(paths);
        match next_offset {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(listed, expected);
    assert_eq!(
        broker
            .execute(&Request::ReadRange {
                path: expected[4096].clone(),
                offset: 0,
                length: 64
            })
            .unwrap(),
        Response::ReadRange {
            text: Some("final needle\n".into())
        }
    );
    assert_eq!(
        broker
            .execute(&Request::Search {
                offset: 0,
                query: "needle".into()
            })
            .unwrap(),
        Response::Search {
            next_offset: None,
            non_text_paths: vec![],
            hits: vec![Hit {
                path: expected[4096].clone(),
                line: 1,
                text: "final needle".into()
            }]
        }
    );
    assert!(
        broker
            .execute(&Request::ListPaths { offset: 4098 })
            .is_err()
    );
}

#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_search_preserves_controls_and_lf_line_numbers() {
    let mut broker = Broker::stage_in(
        BTreeMap::from([(
            "controls".into(),
            "a\u{b}b\u{c}c\u{1c}d\u{1d}e\u{1e}f\r\nsecond needle\n"
                .as_bytes()
                .to_vec(),
        )]),
        Path::new("/tmp"),
    )
    .unwrap();
    for query in ["\u{b}", "\u{c}", "\u{1c}", "\u{1d}", "\u{1e}", "b\u{c}c"] {
        let request = Request::parse(
            &serde_json::to_vec(&json!({"tool":"af_search","query":query,"offset":0})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            broker.execute(&request).unwrap(),
            Response::Search {
                next_offset: None,
                non_text_paths: vec![],
                hits: vec![Hit {
                    path: "controls".into(),
                    line: 1,
                    text: "a\u{b}b\u{c}c\u{1c}d\u{1d}e\u{1e}f\r".into()
                }]
            }
        );
    }
    assert_eq!(
        broker
            .execute(&Request::Search {
                offset: 0,
                query: "needle".into()
            })
            .unwrap(),
        Response::Search {
            next_offset: None,
            non_text_paths: vec![],
            hits: vec![Hit {
                path: "controls".into(),
                line: 2,
                text: "second needle".into()
            }]
        }
    );
}

#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_search_near_limit_short_lines_uses_bounded_memory() {
    let mut bytes = b"ab\n".repeat(MAX_FILE_BYTES / 3);
    bytes.extend_from_slice(b"last needle");
    bytes.truncate(MAX_FILE_BYTES);
    let mut broker = Broker::new(BTreeMap::from([("many-lines".into(), bytes)])).unwrap();
    assert_eq!(
        broker
            .execute(&Request::Search {
                offset: 0,
                query: "absent".into()
            })
            .unwrap(),
        Response::Search {
            next_offset: None,
            non_text_paths: vec![],
            hits: vec![]
        }
    );
    // A subsequent request proves the same broker remains usable after scanning millions
    // of lines in the production container's unchanged 128 MiB memory allowance.
    assert!(broker.execute(&Request::ListPaths { offset: 0 }).is_ok());
}

#[test]
fn native_manifest_identity_retains_binary_entries_and_executable_modes() {
    use af_source_git::{Entry, EntryKind, Manifest};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(root.path().join("cas")).unwrap();
    let bytes = [0xff, 0, 0x80];
    let mut manifest = Manifest::new(vec![Entry {
        path: "binary".into(),
        kind: EntryKind::File,
        content: cas.put(&bytes).unwrap(),
        size: bytes.len() as u64,
    }])
    .unwrap();
    let plain = Broker::from_manifest(&manifest, &cas).unwrap();
    assert_eq!(plain.source_identity(), manifest.content_digest());
    assert_eq!(
        std::fs::read(plain.staging.as_ref().unwrap().path().join("source/binary")).unwrap(),
        bytes
    );
    manifest.entries[0].kind = EntryKind::Executable;
    let executable = Broker::from_manifest(&manifest, &cas).unwrap();
    assert_ne!(plain.source_identity(), executable.source_identity());
    let staged = executable
        .staging
        .as_ref()
        .unwrap()
        .path()
        .join("source/binary");
    assert_eq!(
        std::fs::metadata(staged).unwrap().permissions().mode() & 0o777,
        0o444
    );
    manifest.entries[0].size += 1;
    assert!(Broker::from_manifest(&manifest, &cas).is_err());
    manifest.entries[0].size -= 1;
    manifest.entries[0].kind = EntryKind::Symlink;
    assert!(Broker::from_manifest(&manifest, &cas).is_err());
    // CAS validates domain-separated artifact identities too; a source file must still
    // name its raw bytes, never an envelope identity that happens to be readable there.
    let (artifact, _) = cas
        .put_artifact(
            "af/Test@1",
            af_contracts::Producer::KernelOperation {
                run_id: "synthetic".into(),
                node_id: None,
                operation_id: "fixture".into(),
            },
            vec![],
            None,
            json!({"synthetic":true}),
        )
        .unwrap();
    let bytes = cas.get(&artifact).unwrap();
    manifest.entries[0] = Entry {
        path: "artifact".into(),
        kind: EntryKind::File,
        content: artifact,
        size: bytes.len() as u64,
    };
    assert!(Broker::from_manifest(&manifest, &cas).is_err());
}

#[test]
#[ignore = "requires actual Docker and pinned local image; documented conformance command"]
fn docker_binary_entries_are_listed_and_explicitly_non_text() {
    let mut broker = Broker::new(BTreeMap::from([
        ("a.bin".into(), vec![0xff, 0]),
        ("b.txt".into(), b"needle".to_vec()),
    ]))
    .unwrap();
    assert_eq!(
        broker.execute(&Request::ListPaths { offset: 0 }).unwrap(),
        Response::ListPaths {
            paths: vec!["a.bin".into(), "b.txt".into()],
            next_offset: None
        }
    );
    assert_eq!(
        broker
            .execute(&Request::ReadRange {
                path: "a.bin".into(),
                offset: 0,
                length: 2
            })
            .unwrap(),
        Response::ReadRange { text: None }
    );
    assert_eq!(
        broker
            .execute(&Request::Search {
                query: "needle".into(),
                offset: 0
            })
            .unwrap(),
        Response::Search {
            hits: vec![Hit {
                path: "b.txt".into(),
                line: 1,
                text: "needle".into()
            }],
            next_offset: None,
            non_text_paths: vec!["a.bin".into()]
        }
    );
}
