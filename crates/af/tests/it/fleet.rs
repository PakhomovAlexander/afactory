//! Exercise the public CLI against the broker wire format and an authorized native Task Store.
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

use crate::{common, schemas};

struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in ["config", "state", "home", "bin"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn af(&self, args: &[&str]) -> Command {
        let mut command = common::af();
        let path = std::env::join_paths(
            std::iter::once(self.path("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        command
            .current_dir(self.root.path())
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("PATH", path)
            .args(args);
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        as_json(self.af(args).output().unwrap())
    }

    fn write(&self, name: &str, value: &Value) {
        fs::write(self.path(name), serde_json::to_vec(value).unwrap()).unwrap();
    }
}

fn success(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn as_json(output: Output) -> Value {
    serde_json::from_str(&success(output)).unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn require_failure(output: Output, message: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}

#[test]
fn fresh_install_and_offline_standalone() {
    let machine = Machine::new();
    let validator = schemas::validator("fleet-briefing-v1.json");
    let infra = machine.json(&["infra", "--json"]);
    schemas::valid(&validator, &infra);
    assert!(serde_json::from_value::<af_contracts::fleet::BriefingV1>(infra.clone()).is_ok());
    let mut invalid = infra.clone();
    invalid.as_object_mut().unwrap().remove("inventory");
    assert!(!validator.is_valid(&invalid));
    assert!(serde_json::from_value::<af_contracts::fleet::BriefingV1>(invalid).is_err());
    assert_eq!(infra["configured_contexts"], json!([]));
    assert_eq!(infra["discovery"], "unconfigured");
    assert_eq!(infra["next_action"]["action"], "ask_owner");
    assert!(
        infra["next_action"]["reason"]
            .as_str()
            .unwrap()
            .contains("credential reference")
    );
    let text = success(machine.af(&["infra"]).output().unwrap());
    assert!(text.contains("Next ask_owner"), "{text}");
    let cluster = machine.json(&["cluster", "--json"]);
    assert_eq!(cluster["cluster"], "unknown");
    assert_eq!(cluster["distributed_execution_ready"], false);
    let missing = machine.json(&["infra", "status", "--context", "unknown", "--json"]);
    assert_eq!(missing["context"], "unknown");
    assert!(missing["inventory"].is_null());
    assert!(machine.path("config").read_dir().unwrap().next().is_none());
    assert!(machine.path("state").read_dir().unwrap().next().is_none());

    let (repo, _) = crate::task_cli::fixture_named(machine.root.path(), "pagination");
    let planned = as_json(
        machine
            .af(&["task", "plan", "--file", "ticket.json", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    let shown = as_json(
        machine
            .af(&["task", "show", "pagination-cli", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    assert_eq!(shown["plan_id"], planned["plan_id"]);
    assert!(shown["plan_id"].is_string());
}

fn executable(file: &Path, text: &str) {
    fs::write(file, text).unwrap();
    fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
}

fn key(machine: &Machine, name: &str) -> String {
    success(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(machine.path(name))
            .output()
            .unwrap(),
    );
    fs::read_to_string(machine.path(&format!("{name}.pub")))
        .unwrap()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn link(machine: &Machine, key: &str, destination: &str) {
    let destination = machine.path(destination);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    symlink(machine.path(key), destination).unwrap();
}

fn quoted(path: &Path) -> String {
    shell_words::quote(path.to_str().unwrap()).into_owned()
}

struct Endpoint {
    address: (&'static str, u16),
    #[cfg(target_os = "macos")]
    _server: loopback::Server,
}

fn endpoint(machine: &Machine, sshd: &Path, name: &str, allowed: &[(&str, &str)]) -> Endpoint {
    let authorized = allowed
        .iter()
        .map(|(key, command)| format!("restrict,command=\"{command}\" {key}\n"))
        .collect::<String>();
    fs::write(machine.path(&format!("{name}.keys")), authorized).unwrap();
    let config = machine.path(&format!("{name}.sshd"));
    let log = machine.path(&format!("{name}.sshd.log"));
    fs::write(&config, format!(
        "HostKey {}\nAuthorizedKeysFile {}\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n",
        quoted(&machine.path("host-key")),
        quoted(&machine.path(&format!("{name}.keys")))
    )).unwrap();
    #[cfg(target_os = "macos")]
    {
        let server = loopback::Server::start(sshd, config, log);
        Endpoint {
            address: ("127.0.0.1", server.port),
            _server: server,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        executable(
            &machine.path(&format!("{name}.proxy")),
            &format!(
                "#!/bin/sh\nexec {} -i -e -f {} 2>>{}\n",
                quoted(sshd),
                quoted(&config),
                quoted(&log),
            ),
        );
        let host = match name {
            "broker" => "broker.invalid",
            "project" => "project.invalid",
            _ => unreachable!("unknown SSH fixture endpoint {name}"),
        };
        Endpoint {
            address: (host, 22),
        }
    }
}

// Keep stdio on Linux so the authenticated fixture also runs in gates that forbid TCP listeners.
#[cfg(not(target_os = "macos"))]
fn ssh_fixture(machine: &Machine) {
    executable(
        &machine.path("bin/ssh"),
        &format!(
            r#"#!/bin/sh
for arg do
  case "$arg" in
    broker.invalid) reader=broker ;;
    project.invalid) reader=project ;;
  esac
done
[ -n "$reader" ] || exit 255
exec /usr/bin/ssh -o "ProxyCommand={}/$reader.proxy" "$@"
"#,
            quoted(machine.root.path())
        ),
    );
}

#[cfg(target_os = "macos")]
mod loopback {
    use std::fs::File;
    use std::io::ErrorKind;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use crate::task_interrupt::ChildGuard;

    const POLL: Duration = Duration::from_millis(10);

    pub(super) struct Server {
        pub(super) port: u16,
        stop: Sender<()>,
        worker: Option<JoinHandle<()>>,
        log: PathBuf,
    }

    impl Server {
        pub(super) fn start(sshd: &Path, config: PathBuf, log: PathBuf) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let (stop, stopped) = mpsc::channel();
            let mut command = Command::new(sshd);
            command.args(["-i", "-e", "-f"]).arg(config);
            let stderr = File::create(&log).unwrap();
            let worker = thread::spawn(move || {
                let mut child = ChildGuard(None);
                while stopped.recv_timeout(POLL) == Err(RecvTimeoutError::Timeout) {
                    if let Some(process) = &mut child.0 {
                        if process.try_wait().unwrap().is_none() {
                            continue;
                        }
                        child.0 = None;
                    }
                    let (stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => continue,
                        Err(error) => panic!("accepting fixture SSH connection: {error}"),
                    };
                    // Apple's inetd-mode sshd needs a connected TCP socket on both descriptors
                    // to create the audit session for its forced command.
                    child.0 = Some(
                        command
                            .stdin(Stdio::from(OwnedFd::from(stream.try_clone().unwrap())))
                            .stdout(Stdio::from(OwnedFd::from(stream)))
                            .stderr(Stdio::from(stderr.try_clone().unwrap()))
                            .spawn()
                            .unwrap(),
                    );
                    // Command retains the descriptors used for spawn. Close the parent's copies
                    // so ssh gets EOF as soon as the server closes its side of the connection.
                    command.stdin(Stdio::null()).stdout(Stdio::null());
                }
            });
            Self {
                port,
                stop,
                worker: Some(worker),
                log,
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Err(error) = self.worker.take().unwrap().join()
                && !thread::panicking()
            {
                std::panic::resume_unwind(error);
            }
            if thread::panicking()
                && let Ok(log) = std::fs::read_to_string(&self.log)
            {
                eprintln!("{}:\n{log}", self.log.display());
            }
        }
    }

    pub(super) fn disconnected() -> (OwnedFd, (&'static str, u16)) {
        use rustix::io::{FdFlags, fcntl_setfd};
        use rustix::net::{AddressFamily, SocketType, bind, getsockname, socket};

        // Reserve the ephemeral port without listening, so no SSH handshake is possible.
        let socket = socket(AddressFamily::INET, SocketType::STREAM, None).unwrap();
        fcntl_setfd(&socket, FdFlags::CLOEXEC).unwrap();
        bind(&socket, &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = SocketAddrV4::try_from(getsockname(&socket).unwrap()).unwrap();
        (socket, ("127.0.0.1", address.port()))
    }
}

#[test]
fn pinned_ssh_broker_and_project_origin_keep_independent_read_authority() {
    let sshd = Path::new("/usr/sbin/sshd");
    if !sshd.is_file() || !Path::new("/usr/bin/ssh").is_file() {
        eprintln!("OpenSSH integration fixture requires /usr/sbin/sshd and /usr/bin/ssh");
        return;
    }
    let machine = Machine::new();
    let (repo, _, _) = crate::task_planning::setup(machine.root.path(), false, false);
    let plan = machine
        .af(&["task", "plan", "--file", "ticket.json", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    as_json(plan);
    let waiting = as_json(
        machine
            .af(&["task", "run", "--execute", "pagination-cli", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    assert_eq!(waiting["phase"]["reason"], "needs_plan_review");
    let project = as_json(
        machine
            .af(&["project", "show", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    let project_id = project["project_id"].as_str().unwrap();
    let workspace = &project["workspaces"][0];
    let state = Path::new(workspace["state"].as_str().unwrap());
    let store_id = format!("sha256:{}", af_telemetry::trace::store_id(state).unwrap());
    let database = state.join("events.sqlite");
    let native_history = fs::read(&database).unwrap();

    let host_key = key(&machine, "host-key");
    let wrong_host_key = key(&machine, "wrong-host-key");
    let management_key = key(&machine, "management");
    let owner_key = key(&machine, "owner");
    link(
        &machine,
        "management",
        "config/af/fleet/credentials/operator",
    );
    link(
        &machine,
        "management",
        "config/af/fleet/project-credentials/lab/project",
    );
    machine.write(
        "pools.json",
        &json!({
            "static": ["static-builder", "static-runner"], "dynamic": ["dynamic-worker"], "release": "explicit",
            "available": [{"node":"host-a", "cpu_ids":[0,1,2,3,3], "ram_gib":8, "ssd_gib":120,
                            "static_available":{"cpu_ids":[0,1], "ram_gib":4, "ssd_gib":90}}]
        }),
    );
    machine.write("allocations.json", &json!([
        {"vmid":101,"name":"dynamic-worker","node":"host-a","power":"stopped","state":"reserved","task":"private-native-task"},
        {"vmid":102,"name":null,"node":null,"power":null,"state":null,"task":null}
    ]));
    executable(
        &machine.path("broker"),
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$SSH_ORIGINAL_COMMAND" >> {requests}
case "$SSH_ORIGINAL_COMMAND" in
  'control-api http://localhost/pools')
    [ ! -e {temporary} ] || exit 75
    exec /bin/cat {pools} ;;
  'control-api http://localhost/dynamic/allocations') exec /bin/cat {allocations} ;;
  *) exit 77 ;;
esac
"#,
            requests = quoted(&machine.path("requests")),
            temporary = quoted(&machine.path("temporary")),
            pools = quoted(&machine.path("pools.json")),
            allocations = quoted(&machine.path("allocations.json"))
        ),
    );
    let reader = format!(
        "exec env HOME={} XDG_CONFIG_HOME={} {} cluster origin-read --scope fleet-fixture --origin project --project {} --workspace {} --principal owner --allow-plan-decisions --state-home {}",
        quoted(&machine.path("home")),
        quoted(&machine.path("config")),
        quoted(common::af_binary()),
        project_id,
        shell_words::quote(workspace["label"].as_str().unwrap()),
        quoted(&machine.path("state")),
    );
    // The remote fixture chooses its result after auth, independently of all client arguments.
    executable(
        &machine.path("project"),
        &format!(
            r#"#!/bin/sh
[ "$SSH_ORIGINAL_COMMAND" = 'af cluster origin-read' ] || exit 77
[ ! -e {} ] || exit 75
if [ -e {} ]; then exec /bin/cat {}; fi
{reader}
"#,
            quoted(&machine.path("project-offline")),
            quoted(&machine.path("project-stale")),
            quoted(&machine.path("project-observation.json"))
        ),
    );
    let broker = endpoint(
        &machine,
        sshd,
        "broker",
        &[(&management_key, &quoted(&machine.path("broker")))],
    );
    let project = endpoint(
        &machine,
        sshd,
        "project",
        &[(&owner_key, &quoted(&machine.path("project")))],
    );
    #[cfg(target_os = "macos")]
    let (_reserved_port, disconnected) = {
        symlink("/usr/bin/ssh", machine.path("bin/ssh")).unwrap();
        loopback::disconnected()
    };
    #[cfg(not(target_os = "macos"))]
    let disconnected = {
        ssh_fixture(&machine);
        ("offline.invalid", 22)
    };
    let user = success(Command::new("id").arg("-un").output().unwrap());
    let target = |(host, port): (&str, u16), pin: &str| json!({"host":host,"port":port,"user":user.trim(),"host_key":pin});
    let origin = |address, id: &str| {
        json!({
            "target":target(address, &host_key), "project_id":project_id, "store_id":id
        })
    };
    let descriptor = json!({
        "schema":"af/fleet-context@1","scope":"fleet-fixture","cluster":"absent",
        "management":target(broker.address,&host_key),
        "origins":{"project":origin(project.address,&store_id)}
    });
    let validator = schemas::validator("fleet-briefing-v1.json");
    schemas::valid(&schemas::validator("fleet-context-v1.json"), &descriptor);
    machine.write("lab.afcontext", &descriptor);
    let connect = |alias, file| {
        machine
            .af(&[
                "infra",
                "connect",
                "--context",
                alias,
                "--from",
                file,
                "--credential-ref",
                "operator",
            ])
            .output()
            .unwrap()
    };
    let mut wrong = descriptor.clone();
    wrong["management"]["host_key"] = json!(wrong_host_key);
    machine.write("wrong.afcontext", &wrong);
    require_failure(
        connect("bad", "wrong.afcontext"),
        "server identity mismatch",
    );
    assert!(!machine.path("config/af/fleet/contexts/bad.json").exists());
    assert!(!machine.path("requests").exists());
    let first = success(connect("lab", "lab.afcontext"));
    assert!(
        first.contains("pinned management reader is authenticated"),
        "{first}"
    );
    let binding_path = machine.path("config/af/fleet/contexts/lab.json");
    let bytes = fs::read(&binding_path).unwrap();
    schemas::valid(
        &schemas::validator("fleet-binding-v1.json"),
        &serde_json::from_slice(&bytes).unwrap(),
    );
    let bound_at = fs::metadata(&binding_path).unwrap().modified().unwrap();
    success(connect("lab", "lab.afcontext"));
    assert_eq!(
        fs::metadata(&binding_path).unwrap().modified().unwrap(),
        bound_at
    );
    let mut different = descriptor.clone();
    different["scope"] = json!("different-scope");
    machine.write("collision.afcontext", &different);
    require_failure(
        connect("lab", "collision.afcontext"),
        "already has a different binding",
    );
    assert_eq!(fs::read(binding_path).unwrap(), bytes);

    let brief = machine.json(&["infra", "--json"]);
    schemas::valid(&validator, &brief);
    assert_eq!(brief["context"], "lab");
    assert_eq!(brief["discovery"], "ready");
    assert!(brief["desired_footprint"].is_null());
    let status = machine.json(&["infra", "status", "--context", "lab", "--json"]);
    assert_eq!(status["next_action"]["action"], "run");
    let inventory = &status["inventory"]["data"];
    assert_eq!(inventory["hosts"].as_array().unwrap().len(), 1);
    let host = &inventory["hosts"][0];
    assert_eq!(host["available"]["cpu_ids"], json!([0, 1, 2, 3]));
    assert_eq!(host["available"]["ram_gib"].as_f64(), Some(8.0));
    assert_eq!(host["available"]["ssd_gib"].as_f64(), Some(120.0));
    assert_eq!(host["static_available"]["cpu_ids"], json!([0, 1]));
    assert_eq!(host["static_available"]["ram_gib"].as_f64(), Some(4.0));
    assert_eq!(host["static_available"]["ssd_gib"].as_f64(), Some(90.0));
    assert!(host["physical_totals"].is_null() && host["reservations"].is_null());
    assert!(status["inventory"]["observation"]["revision"].is_null());
    assert_eq!(inventory["pools"], json!([]));
    let text = success(
        machine
            .af(&["infra", "status", "--context", "lab"])
            .output()
            .unwrap(),
    );
    assert!(
        text.contains("4 allocatable vCPUs")
            && text.contains("Storage pool inventory unknown")
            && text.contains("Next run"),
        "{text}"
    );
    for vm in ["static-builder", "static-runner", "dynamic-worker"] {
        assert!(!text.contains(vm), "{text}");
    }
    let allocations = machine.json(&["infra", "allocations", "--context", "lab", "--json"]);
    let vms = &allocations["allocations"]["data"];
    assert_eq!(vms[0]["name"], "dynamic-worker");
    assert_eq!(vms[0]["power"], "stopped");
    assert!(
        vms[0]["charged"].is_null() && vms[0]["executor"].is_null() && vms[1]["node"].is_null()
    );
    assert!(!allocations.to_string().contains("private-native-task"));

    let cache = machine.path("state/af/fleet-cache/lab/pools.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
    schemas::valid(&schemas::validator("fleet-cache-v1.json"), &record);
    fs::write(machine.path("temporary"), "").unwrap();
    let stale = machine.json(&["infra", "status", "--context", "lab", "--json"]);
    assert_eq!(stale["inventory"]["observation"]["freshness"], "stale");
    assert_eq!(stale["inventory"]["data"], *inventory);
    assert_eq!(stale["next_action"]["action"], "wait");
    record["observed_unix_ms"] = json!(1);
    fs::write(cache, record.to_string()).unwrap();
    let expired = machine.json(&["infra", "status", "--context", "lab", "--json"]);
    assert!(expired["inventory"]["data"].is_null());
    fs::remove_file(machine.path("temporary")).unwrap();

    let denied = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    assert_eq!(denied["origins"][0]["observation"]["freshness"], "denied");
    assert!(denied["origins"][0]["data"].is_null());
    assert!(!denied.to_string().contains("pagination-cli"));
    fs::remove_file(machine.path("config/af/fleet/project-credentials/lab/project")).unwrap();
    link(
        &machine,
        "owner",
        "config/af/fleet/project-credentials/lab/project",
    );
    let work = machine.json(&["cluster", "--infra", "lab", "--json"]);
    assert_eq!(work["discovery"], "ready");
    assert_eq!(work["cluster"], "absent");
    assert_eq!(work["distributed_execution_ready"], false);
    let inbox = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    schemas::valid(&validator, &inbox);
    assert_eq!(inbox["next_action"]["action"], "run");
    let task = &inbox["origins"][0]["data"]["tasks"][0];
    assert_eq!(task["task_id"], "pagination-cli");
    assert_eq!(task["phase"], "waiting");
    assert_eq!(task["reason"], "needs_plan_review");
    assert_eq!(task["decision"]["plan_id"], waiting["plan_id"]);
    assert_eq!(task["decision"]["task_revision_id"], waiting["revision_id"]);
    assert!(task["decision"]["prepared_id"].is_null());
    assert_eq!(task["decision"]["developer"], "owner");
    assert!(task["placement"].is_null());
    assert!(task["observation"]["revision"].is_u64());
    let inbox_text = success(
        machine
            .af(&["cluster", "inbox", "--context", "lab"])
            .output()
            .unwrap(),
    );
    assert!(inbox_text.contains("may approve or reject plan") && inbox_text.contains("Next run"));

    // A read-only grant can list Tasks while the server withholds plan Decisions.
    let origin_reply = machine
        .af(&[
            "cluster",
            "origin-read",
            "--scope",
            "fleet-fixture",
            "--origin",
            "project",
            "--project",
            project_id,
            "--workspace",
            workspace["label"].as_str().unwrap(),
            "--principal",
            "reader",
            "--state-home",
            path(&machine.path("state")),
        ])
        .output()
        .unwrap();
    let mut wire = as_json(origin_reply);
    schemas::valid(&schemas::validator("fleet-origin-v1.json"), &wire);
    assert!(wire["origin"]["tasks"][0]["decision"].is_null());
    let current = wire["observed_unix_ms"].as_u64().unwrap();
    wire["observed_unix_ms"] = json!(current - 600_000);
    wire["origin"]["tasks"][0]["observation"]["observed_unix_ms"] =
        wire["observed_unix_ms"].clone();
    wire["origin"]["principal"] = json!("owner");
    wire["origin"]["tasks"][0]["decision"] = task["decision"].clone();
    machine.write("project-observation.json", &wire);
    fs::write(machine.path("project-stale"), "").unwrap();
    let stale = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    assert_eq!(stale["origins"][0]["observation"]["freshness"], "stale");
    assert!(stale["origins"][0]["data"]["tasks"][0]["decision"].is_null());
    fs::remove_file(machine.path("project-stale")).unwrap();
    fs::write(machine.path("project-offline"), "").unwrap();
    let offline = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    assert_eq!(
        offline["origins"][0]["observation"]["freshness"],
        "unavailable"
    );
    assert!(offline["origins"][0]["data"].is_null());
    fs::remove_file(machine.path("project-offline")).unwrap();

    let mut mixed = descriptor;
    mixed["origins"]["disconnected"] = origin(disconnected, &store_id);
    mixed["origins"]["mismatch"] = origin(project.address, &store_id);
    machine.write("mixed.afcontext", &mixed);
    success(connect("mixed", "mixed.afcontext"));
    let choices = machine.json(&["infra", "--json"]);
    assert_eq!(choices["configured_contexts"], json!(["lab", "mixed"]));
    assert!(choices["context"].is_null());
    let missing_keys = machine.json(&["cluster", "inbox", "--context", "mixed", "--json"]);
    assert!(
        missing_keys["origins"]
            .as_array()
            .unwrap()
            .iter()
            .all(|origin| {
                origin["observation"]["freshness"] == "missing_credential"
                    && origin["data"].is_null()
            })
    );

    for name in ["project", "disconnected", "mismatch"] {
        link(
            &machine,
            "owner",
            &format!("config/af/fleet/project-credentials/mixed/{name}"),
        );
    }
    let mixed = machine.json(&["cluster", "inbox", "--context", "mixed", "--json"]);
    assert_eq!(mixed["discovery"], "partial");
    assert_eq!(
        mixed["origins"][0]["observation"]["freshness"],
        "unavailable"
    );
    assert_eq!(
        mixed["origins"][1]["observation"]["freshness"],
        "identity_mismatch"
    );
    assert_eq!(mixed["origins"][2]["observation"]["freshness"], "fresh");
    assert_eq!(
        mixed["origins"][2]["data"]["tasks"][0]["task_id"],
        "pagination-cli"
    );
    assert_eq!(native_history, fs::read(database).unwrap());
    assert!(
        !machine
            .path("state/af/fleet-cache/lab/waiting_tasks.json")
            .exists()
    );
    let requests = fs::read_to_string(machine.path("requests")).unwrap();
    assert!(requests.lines().any(|line| line.ends_with("/pools")));
    assert!(
        requests
            .lines()
            .any(|line| line.ends_with("/dynamic/allocations"))
    );
    assert!(requests.lines().all(|line| matches!(
        line,
        "control-api http://localhost/pools" | "control-api http://localhost/dynamic/allocations"
    )));
}
