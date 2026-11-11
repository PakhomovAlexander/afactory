//! Native Scenario + installed production lifecycle; only HTTPS and GitHub transport are inert.
use super::*;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("/usr/bin/git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
fn hash(path: &Path) -> String {
    format!(
        "sha256:{}",
        af_contracts::hex::encode(&Sha256::digest(std::fs::read(path).unwrap()))
    )
}
fn preparation(run: &Value, name: &str) -> Value {
    serde_json::from_str(
        receipt(run, name)["evidence"]
            .as_str()
            .unwrap_or_else(|| panic!("missing preparation {name}: {run}")),
    )
    .unwrap()
}
fn receipt<'a>(run: &'a Value, name: &str) -> &'a Value {
    &run["frames"]["root"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == name)
        .unwrap()["outputs"]["result"]["value"]
}
fn remote_head(f: &Fixture, remote: &Path, p: &Value) -> String {
    git(
        &f.repo,
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/heads/{}", p["branch"].as_str().unwrap()),
        ],
    )
}
fn assert_task(state: &Path, run: &Value, name: &str, p: &Value) {
    let t = receipt(run, name);
    assert_eq!(t["result"]["acceptance"], "satisfied");
    assert_eq!(t["task_id"], p["task_id"]);
    assert_eq!(t["result_id"], p["result_id"]);
    assert_eq!(
        t["result"]["outputs"]["snapshot"]["snapshot_id"],
        p["snapshot_id"]
    );
    let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
    let revision = cas
        .get_artifact(t["revision_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        revision.payload["inputs"]["source"]["snapshot_id"],
        p["source_snapshot"]
    );
    assert_eq!(revision.payload["kind"], "implement");
    assert!(
        af_source_git::task::descends_from(
            &cas,
            p["snapshot_id"].as_str().unwrap(),
            p["source_snapshot"].as_str().unwrap()
        )
        .unwrap()
    );
    if !p["source_predecessor"].is_null() {
        let (snapshot, _) =
            af_source_git::task::read_snapshot(&cas, p["source_snapshot"].as_str().unwrap())
                .unwrap();
        let origin = af_source_git::task::read_origin(&cas, &snapshot.origin_id).unwrap();
        assert_eq!(
            origin.bound_from().unwrap().snapshot_id,
            p["source_predecessor"].as_str().unwrap()
        );
        let requirements = cas
            .get_artifact(
                revision.payload["inputs"]["requirements"]["artifact_ids"][0]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            requirements.payload["specification"]["scenario_evidence"],
            *receipt(run, "checks")
        );
    }
}

#[test]
fn production_workspace_scenario_repairs_same_pr_and_cleans_only_after_merge() {
    for mode in ["ready", "repairable", "pending", "stale"] {
        let f = Fixture::bare();
        std::fs::remove_dir(&f.repo).unwrap();
        let task_root = f.repo.parent().unwrap().join("task-fixture");
        std::fs::create_dir(&task_root).unwrap();
        let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "pagination");
        std::fs::rename(repo, &f.repo).unwrap();
        let commit = std::process::Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(commit.status.success());
        let commit = String::from_utf8(commit.stdout).unwrap();
        let file = f.machine.join("template.json");
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("ticket.json")).unwrap()).unwrap();
        document["verification"] = json!("evaluation");
        write(&file, &document);
        let pinned = cli(
            &f.repo,
            &f.state,
            &[
                "scenario",
                "template-pin",
                "--file",
                file.to_str().unwrap(),
                "--repo",
                f.repo.to_str().unwrap(),
                "--task-state",
                task_state.to_str().unwrap(),
                "--authority-commit",
                commit.trim(),
            ],
        );
        let remote = f.machine.join("remote.git");
        git(&f.repo, &["init", "--bare", remote.to_str().unwrap()]);
        git(
            &f.repo,
            &["push", remote.to_str().unwrap(), "HEAD:refs/heads/main"],
        );
        install(&f, &task_state, &pinned, commit.trim(), &remote, mode);
        let binding = |id: &str| json!({"source":"step","step":id,"port":"result"});
        let project = |id: &str, kind: &str| json!({"source":"project","value":binding(id),"projection":{"kind":kind}});
        let grant = |op: &str| json!({"adapter":"workspace","operation":op,"destination":"github:octo/project"});
        let action = |op: &str, signed: bool| json!({"kind":"action","grant":grant(op),"requires_decision":signed,"required_condition":null});
        let prepare = |id: &str, candidate: &str| {
            step(
                id,
                action("prepare_candidate_v1", false),
                json!({"candidate":binding(candidate),"subject":project(candidate,"task_result_id"),
            "revision":{"source":"project","value":binding(candidate),"projection":{"kind":"task_output_snapshot","port":"snapshot"}},
            "body":literal("text",json!("{\"schema\":\"af/git-workspace-content@1\",\"title\":\"Documentation\",\"body\":\"Exact accepted candidate\"}"))}),
                "action",
            )
        };
        let decision = |id: &str, prepared: &str| {
            step(
                id,
                json!({"kind":"decision","owner":"owner"}),
                json!({"subject":project(prepared,"workspace_subject"),"revision":project(prepared,"workspace_revision")}),
                "decision",
            )
        };
        let publish = |id: &str, prepared: &str, approval: &str| {
            step(
                id,
                action("publish_pr", true),
                json!({"subject":project(prepared,"workspace_subject"),"revision":project(prepared,"workspace_revision"),
            "body":project(prepared,"workspace_publication"),"decision":binding(approval)}),
                "action",
            )
        };
        let repair_if = json!({"source":"equals","left":project("checks","checks_state"),"right":literal("text",json!("repairable"))});
        let mut repair = step(
            "repair",
            json!({"kind":"fresh_task","template":pinned["template"],
            "bindings":{"source":{"port":"snapshot","artifact_type":"af/SourceTree@1"}}}),
            json!({"source":binding("edit"),"scenario_evidence":binding("checks"),"publication":binding("prepare")}),
            "task",
        );
        repair["when"] = repair_if.clone();
        let mut repaired = prepare("repaired", "repair");
        repaired["inputs"]["previous"] = binding("prepare");
        repaired["when"] = repair_if.clone();
        let mut approve_repair = decision("approve-repair", "repaired");
        approve_repair["when"] = repair_if.clone();
        let mut publish_repair = publish("publish-repair", "repaired", "approve-repair");
        publish_repair["when"] = repair_if.clone();
        let selected = json!({"source":"select","condition":repair_if,"when_true":binding("repaired"),"when_false":binding("prepare")});
        let merge = step(
            "merge",
            json!({"kind":"wait","grant":grant("observe"),"condition":"merged"}),
            json!({"subject":{"source":"project","value":selected,"projection":{"kind":"workspace_subject"}},
                "revision":{"source":"project","value":selected,"projection":{"kind":"workspace_revision"}}}),
            "observation",
        );
        let cleanup = step(
            "cleanup",
            json!({"kind":"action","grant":grant("cleanup_workspace_v1"),"requires_decision":false,"required_condition":"merged"}),
            json!({"prepared":selected,"body":{"source":"project","value":selected,"projection":{"kind":"workspace_receipt"}},
                "subject":{"source":"project","value":selected,"projection":{"kind":"workspace_subject"}},
                "revision":{"source":"project","value":selected,"projection":{"kind":"workspace_revision"}},
                "evidence":binding("merge")}),
            "action",
        );
        let bundle = json!({"schema":"af/scenario-bundle@5","root":"documentation-maintenance","definitions":{"documentation-maintenance":{
            "schema":"af/scenario@5","admission":null,"inputs":{},"outputs":{"selected":"action"},
            "steps":[step("edit",json!({"kind":"fresh_task","template":pinned["template"],"bindings":{}}),json!({}),"task"),
                prepare("prepare","edit"),decision("approve","prepare"),publish("publish","prepare","approve"),
                step("checks",json!({"kind":"wait","grant":grant("observe"),"condition":"checks_completed"}),
                    json!({"subject":project("prepare","workspace_subject"),"revision":project("prepare","workspace_revision")}),"observation"),
                repair,repaired,approve_repair,publish_repair,merge,cleanup],
            "result":{"selected":selected},"completion":[literal("boolean",json!(true))],
            "authority":{"task_plans":[],"task_templates":[pinned["template_id"]],
                "grants":[grant("prepare_candidate_v1"),grant("publish_pr"),grant("observe"),grant("cleanup_workspace_v1")],"decision_owners":["owner"]},
            "limits":{"max_tokens":2000,"max_wall_ms":3600000,"max_dispatches":24}}}});
        valid("scenario-bundle-v5.json", &bundle);
        write(&f.bundle, &bundle);
        let initial = f.start("documentation");
        assert_eq!(initial["status"], "waiting", "{initial}");
        let first = preparation(&initial, "prepare");
        assert_eq!(first["source_commit"], commit.trim());
        assert!(first["source_predecessor"].is_null());
        assert_task(&task_state, &initial, "edit", &first);
        assert_eq!(
            git(
                &f.repo,
                &[
                    "rev-parse",
                    &format!("{}^", first["commit"].as_str().unwrap())
                ]
            ),
            commit.trim()
        );
        assert_eq!(remote_head(&f, &remote, &first), ""); // preparation does not push
        f.approve("documentation");
        let mut run = cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]);
        assert_eq!(remote_head(&f, &remote, &first), first["commit"]);
        let identity = f.github_state()["prs"]["1"]["id"].clone();
        assert_eq!(f.github_state()["effects"], json!(["publish_pr:1"]));
        if mode == "stale" || mode == "pending" {
            assert_eq!(
                run["status"],
                if mode == "stale" { "failed" } else { "waiting" },
                "{run}"
            );
            assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 1);
            if mode == "stale" {
                assert!(
                    run["failure"].as_str().unwrap().contains("Adapter failed"),
                    "{run}"
                );
            }
            assert!(receipt(&run, "checks").is_null());
            if mode == "pending" {
                // Pending itself admitted no repair. A foreign remote head on the next
                // poll must refuse, rather than supplying current-head failure evidence.
                git(
                    &f.repo,
                    &[
                        "--git-dir",
                        remote.to_str().unwrap(),
                        "update-ref",
                        &format!("refs/heads/{}", first["branch"].as_str().unwrap()),
                        commit.trim(),
                        first["commit"].as_str().unwrap(),
                    ],
                );
                let stale = cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]);
                assert_eq!(stale["status"], "failed", "{stale}");
                assert_eq!(stale["fresh_tasks"].as_object().unwrap().len(), 1);
                assert!(receipt(&stale, "checks").is_null());
                assert_eq!(remote_head(&f, &remote, &first), commit.trim());
            }
            assert!(Path::new(first["worktree"].as_str().unwrap()).exists());
            continue;
        }
        let selected = if mode == "repairable" {
            assert_eq!(run["status"], "waiting", "{run}");
            assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 2);
            let repaired = preparation(&run, "repaired");
            assert_ne!(repaired["commit"], first["commit"]);
            assert_eq!(repaired["source_commit"], first["commit"]);
            assert_eq!(repaired["source_predecessor"], first["snapshot_id"]);
            assert_eq!(repaired["previous_owner"], first["owner"]);
            assert_eq!(repaired["branch"], first["branch"]);
            assert_task(&task_state, &run, "repair", &repaired);
            assert_eq!(
                git(
                    &f.repo,
                    &[
                        "rev-parse",
                        &format!("{}^", repaired["commit"].as_str().unwrap())
                    ]
                ),
                first["commit"]
            );
            // Process restart alone cannot reuse the first head's signed authority.
            let waiting = cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]);
            assert_eq!(waiting["status"], "waiting");
            assert_eq!(remote_head(&f, &remote, &first), first["commit"]);
            f.approve("documentation");
            run = cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]);
            repaired
        } else {
            assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 1);
            first.clone()
        };
        assert_eq!(run["status"], "waiting", "{run}");
        assert_eq!(remote_head(&f, &remote, &selected), selected["commit"]);
        assert_eq!(
            git(
                &f.repo,
                &[
                    "--git-dir",
                    remote.to_str().unwrap(),
                    "rev-parse",
                    &format!("{}^", selected["commit"].as_str().unwrap())
                ]
            ),
            selected["source_commit"]
        );
        assert_eq!(
            receipt(&run, "approve")["request"]["revision"],
            first["commit"]
        );
        if mode == "repairable" {
            assert_eq!(
                receipt(&run, "approve-repair")["request"]["revision"],
                selected["commit"]
            );
            assert_ne!(
                receipt(&run, "approve")["signature"],
                receipt(&run, "approve-repair")["signature"]
            );
        }
        let service = f.github_state();
        assert_eq!(service["effects"], json!(["publish_pr:1"]));
        assert_eq!(service["prs"].as_object().unwrap().len(), 1);
        assert_eq!(service["prs"]["1"]["id"], identity);
        assert_eq!(service["prs"]["1"]["head"]["sha"], selected["commit"]);
        assert_eq!(service["prs"]["1"]["head"]["ref"], selected["branch"]);
        assert_eq!(
            service["requests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r[0] == "POST")
                .count(),
            1
        );
        for p in [&first, &selected] {
            assert!(
                Path::new(p["worktree"].as_str().unwrap())
                    .join("target")
                    .is_dir()
            );
        }
        // Perform a real no-ff merge and publish its actual commit to the inert bare remote.
        git(&f.repo, &["checkout", "-q", "-B", "main", commit.trim()]);
        git(
            &f.repo,
            &[
                "merge",
                "--no-ff",
                "-m",
                "human merge",
                selected["commit"].as_str().unwrap(),
            ],
        );
        let merged = git(&f.repo, &["rev-parse", "HEAD"]);
        assert_eq!(git(&f.repo, &["rev-parse", "HEAD^2"]), selected["commit"]);
        git(
            &f.repo,
            &["push", remote.to_str().unwrap(), "HEAD:refs/heads/main"],
        );
        let mut service = f.github_state();
        service["prs"]["1"]["state"] = json!("closed");
        service["prs"]["1"]["merged"] = json!(true);
        service["prs"]["1"]["merged_at"] = json!("2026-11-06T12:00:00Z");
        service["prs"]["1"]["merge_commit_sha"] = json!(merged);
        write(&f.machine.join("mock-service.json"), &service);
        let done = cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]);
        assert_eq!(done["status"], "succeeded", "{done}");
        for p in [&first, &selected] {
            assert!(!Path::new(p["worktree"].as_str().unwrap()).exists());
        }
        assert_eq!(
            std::fs::read_dir(f.machine.join("worktrees"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(remote_head(&f, &remote, &selected), "");
        assert_eq!(
            git(&f.repo, &["worktree", "list", "--porcelain"])
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1
        );
        assert_eq!(
            git(
                &f.repo,
                &[
                    "for-each-ref",
                    "--format=%(objectname)",
                    &format!("refs/heads/{}", selected["branch"].as_str().unwrap())
                ]
            ),
            ""
        );
        assert_eq!(
            git(
                &f.repo,
                &[
                    "--git-dir",
                    remote.to_str().unwrap(),
                    "rev-parse",
                    "refs/heads/main"
                ]
            ),
            merged
        );
        assert_eq!(
            cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]),
            done
        );
        assert_eq!(f.github_state()["effects"], json!(["publish_pr:1"]));
    }
}

fn install(f: &Fixture, task_state: &Path, pin: &Value, commit: &str, remote: &Path, mode: &str) {
    let binary = crate::common::privately_installed_af(&f.machine);
    let git_wrapper = f.machine.join("git");
    let wrapper = format!(
        r#"#!/usr/bin/python3 -IS
import os, sys
args = sys.argv[1:]
url = "ssh://git@github.com/octo/project.git"
if url in args:
    assert "push" in args or "ls-remote" in args
    args = [ {remote} if a == url else a for a in args ]
    # Override the production file denial only for this one fixed transport mapping.
    args = [a if a != "protocol.file.allow=never" else "protocol.file.allow=always" for a in args]
os.execve("/usr/bin/git", ["/usr/bin/git"] + args, os.environ)
"#,
        remote = serde_json::to_string(remote).unwrap()
    );
    std::fs::write(&git_wrapper, wrapper).unwrap();
    std::fs::set_permissions(&git_wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ssh = f.machine.join("ssh-config");
    let token = f.machine.join("token");
    std::fs::write(&ssh, "Host github.com\n  BatchMode yes\n").unwrap();
    std::fs::write(&token, "github_pat_TEST_CREDENTIAL_ONLY\n").unwrap();
    for p in [&ssh, &token] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let worktrees = f.machine.join("worktrees");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&worktrees)
        .unwrap();
    let config = json!({"schema":"af/git-workspace-config@2", "repository":"octo/project","repository_id":17,
        "actor":{"id":7,"login":"machine-bot"},"token_file":token,"state_dir":f.machine.join("receipts"),
        "repo":f.repo,"task_store":task_state,"worktree_root":worktrees,"base":"main","branch_prefix":"accepted",
        "source_commit":commit,"source_content_digest":pin["template"]["source_content_digest"],
        "af":binary,"af_digest":hash(&binary),"git":git_wrapper,"git_digest":hash(&git_wrapper),
        "ssh_config":ssh,"ssh_config_digest":hash(&ssh),"author_name":"Fixture","author_email":"fixture@example.invalid",
        "required_checks":[{"kind":"check","name":"CI","app_id":4}],"reviewers":[],
        "publication_mode":"exclusive_owned_writer","operations":["prepare_candidate_v1","publish_pr","observe","cleanup_workspace_v1"]});
    valid("git-workspace-config-v2.json", &config);
    let config_path = f.machine.join("config.json");
    write(&config_path, &config);
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let executable = f.machine.join("workspace-adapter");
    let installed = std::process::Command::new("/usr/bin/python3")
        .arg("-IS")
        .arg(workspace().join("scripts/install-scenario-git-workspace.py"))
        .arg("--config")
        .arg(&config_path)
        .arg("--output")
        .arg(&executable)
        .env_clear()
        .env("HOME", &f.machine)
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert_eq!(
        String::from_utf8(installed.stdout).unwrap().trim(),
        hash(&executable)
    );
    let production = std::fs::read(&executable).unwrap();
    let mut bytes = b"#!/usr/bin/python3 -IS\n".to_vec();
    bytes
        .extend(std::fs::read(workspace().join("fixtures/scenarios/github-transport.py")).unwrap());
    bytes.extend(
        std::fs::read(workspace().join("fixtures/scenarios/workspace/github-transport.py"))
            .unwrap(),
    );
    bytes.extend(production);
    std::fs::write(&executable, bytes).unwrap();
    let mut service: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/scenarios/github/mock-service.json")).unwrap(),
    )
    .unwrap();
    service["bare_remote"] = json!(remote);
    service["mode"] = json!(mode);
    service["requests"] = json!([]);
    write(&f.machine.join("mock-service.json"), &service);
    std::fs::set_permissions(
        f.machine.join("mock-service.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
    policy["task_templates"] = json!([pin["template_id"]]);
    policy["adapters"] = json!({"workspace":{"executable":executable,"digest":hash(&executable),
        "operations":config["operations"],"destinations":["github:octo/project"],"max_wall_ms":30000,"max_output_bytes":65536}});
    write(&f.policy, &policy);
}
