//! Fake Provider CLIs that serve the two recorded model catalogs token-free (ADR-0148).
//!
//! The fake `claude` answers the `list_models` and `get_binary_version` control requests of a
//! `-p --input-format stream-json --output-format stream-json --verbose` session with
//! `claude-list-models.json` and `claude-binary-version.json`, and records any user message it
//! receives in `turns`, which no catalog read may ever write. The fake `codex` serves a bare
//! `app-server`: `initialize`, `model/list` from `codex-model-list.json` (writing
//! `models_cache.json` first, as the CLI caches its catalog) and `account/read`. Both answer their
//! capability check, their version and their login status, and record every process af starts
//! as one line of `started` in the auth directory. A file `catalog-broken` in the auth directory
//! makes the catalog session exit 1 before answering.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The recorded catalogs, below the workspace root a test run names (`AF_WORKSPACE_ROOT`), or the
/// one this crate was compiled in.
fn fixtures() -> std::path::PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(|root| std::path::PathBuf::from(root).join("crates/af"))
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .join("tests/fixtures/provider-models")
}

const CLAUDE: &str = r#"#!/usr/bin/python3
import json,os,sys
home=os.environ.get('CLAUDE_CONFIG_DIR') or os.environ['HOME']+'/.claude'
with open(home+'/started','a') as f: f.write(' '.join(sys.argv[1:])+'\n')
if sys.argv[1:]==['--version']:
 print('2.1.300 (Claude Code)')
 sys.exit(0)
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':'developer@example.test'}))
 sys.exit(0)
if sys.argv[1:]==['-p','--input-format','stream-json','--output-format','stream-json','--verbose']:
 if os.path.exists(home+'/catalog-broken'):
  sys.stderr.write('Error: the catalog session cannot start\n')
  sys.exit(1)
 answers={'list_models':json.load(open(home+'/claude-list-models.json')),'get_binary_version':json.load(open(home+'/claude-binary-version.json'))}
 for line in sys.stdin:
  message=json.loads(line)
  if message.get('type')=='control_request':
   subtype=message['request']['subtype']
   if subtype in answers:
    response={'subtype':'success','request_id':message['request_id'],'response':answers[subtype]}
   else:
    response={'subtype':'error','request_id':message['request_id'],'error':'Unsupported control request subtype'}
   sys.stdout.write(json.dumps({'type':'control_response','response':response})+'\n')
   sys.stdout.flush()
  else:
   with open(home+'/turns','a') as f: f.write(line)
 sys.exit(0)
sys.exit(64)
"#;

const CODEX: &str = r#"#!/usr/bin/python3
import json,os,shutil,sys
home=os.environ.get('CODEX_HOME') or os.environ['HOME']+'/.codex'
with open(home+'/started','a') as f: f.write(' '.join(sys.argv[1:])+'\n')
if sys.argv[1:]==['--version']:
 print('codex-cli 0.162.1')
 sys.exit(0)
if sys.argv[1:3]==['login','status']:
 sys.stderr.write('Logged in using ChatGPT\n')
 sys.exit(0)
if sys.argv[1:]==['app-server']:
 for line in sys.stdin:
  message=json.loads(line)
  method=message.get('method')
  if method=='initialize':
   answer={'id':1,'result':{'userAgent':'codex_cli_rs/0.162.1'}}
  elif method=='model/list':
   if os.path.exists(home+'/catalog-broken'):
    sys.exit(1)
   shutil.copyfile(home+'/codex-models-cache.json',home+'/models_cache.json')
   answer={'id':message['id'],'result':json.load(open(home+'/codex-model-list.json'))}
  elif method=='account/read':
   answer={'id':message['id'],'result':{'account':{'type':'chatgpt','email':'developer@example.test'}}}
  elif method in ('thread/start','thread/resume','thread/fork','turn/start'):
   with open(home+'/turns','a') as f: f.write(line)
   sys.exit(64)
  else:
   continue
  sys.stdout.write(json.dumps(answer)+'\n')
  sys.stdout.flush()
 sys.exit(0)
sys.exit(64)
"#;

/// Install the fake `kind` CLI into `bin`, and the recorded catalog it serves into `auth`.
pub fn install(kind: &str, bin: &Path, auth: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    std::fs::create_dir_all(auth).unwrap();
    std::fs::set_permissions(auth, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (script, files): (&str, &[&str]) = match kind {
        "claude" => (
            CLAUDE,
            &["claude-list-models.json", "claude-binary-version.json"],
        ),
        "codex" => (CODEX, &["codex-model-list.json", "codex-models-cache.json"]),
        _ => panic!("unknown Provider kind {kind}"),
    };
    for file in files {
        std::fs::copy(fixtures().join(file), auth.join(file)).unwrap();
    }
    let program = bin.join(kind);
    std::fs::write(&program, crate::provider_cli::with_help(kind, script)).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The processes the fake started in `auth`, one line each; empty when none started.
pub fn started(auth: &Path) -> String {
    std::fs::read_to_string(auth.join("started")).unwrap_or_default()
}

/// Business messages the fake received: Claude non-control messages or Codex thread/turn requests.
pub fn turns(auth: &Path) -> String {
    std::fs::read_to_string(auth.join("turns")).unwrap_or_default()
}

/// Install an official-SDK protocol fixture outside the repository. It drives the real embedded
/// AF host on real Node, with synthetic credentials and inference events; no installed Pi needed.
pub(crate) fn install_pi(
    root: &Path,
    auth: &Path,
    mode: &str,
) -> (PathBuf, std::ffi::OsString, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let sdk = root.join("installed/node_modules/@earendil-works/pi-coding-agent");
    std::fs::create_dir_all(sdk.join("dist/core")).unwrap();
    let fixture = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("fixtures/pi-sdk");
    let state = root.join("pi-fixture.json");
    std::fs::write(
        &state,
        serde_json::to_vec(&serde_json::json!({"mode":mode.strip_prefix("kimi-").unwrap_or(mode),"backend":if mode.starts_with("kimi-") { "kimi-coding" } else { "openai-codex" },"auth":auth.join("auth.json")}))
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        sdk.join("package.json"),
        r#"{"name":"@earendil-works/pi-coding-agent","version":"1.4.0","type":"module"}"#,
    )
    .unwrap();
    let script = std::fs::read_to_string(fixture.join("index.mjs"))
        .unwrap()
        .replace("AF_FIXTURE_STATE", &serde_json::to_string(&state).unwrap());
    std::fs::write(sdk.join("dist/index.js"), script).unwrap();
    std::fs::copy(
        fixture.join("auth-storage.mjs"),
        sdk.join("dist/core/auth-storage.js"),
    )
    .unwrap();
    let ai = sdk.parent().unwrap().join("pi-ai");
    std::fs::create_dir_all(ai.join("dist")).unwrap();
    std::fs::write(
        ai.join("package.json"),
        r#"{"type":"module","name":"@earendil-works/pi-ai","version":"1.4.0"}"#,
    )
    .unwrap();
    let core = sdk.parent().unwrap().join("pi-agent-core");
    std::fs::create_dir_all(&core).unwrap();
    std::fs::write(
        core.join("package.json"),
        r#"{"name":"@earendil-works/pi-agent-core","version":"1.4.0"}"#,
    )
    .unwrap();
    std::fs::write(
        ai.join("dist/models.js"),
        "export const getSupportedThinkingLevels = () => ['high'];\n",
    )
    .unwrap();
    std::fs::create_dir_all(auth).unwrap();
    std::fs::set_permissions(auth, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(auth.join("auth.json"),r#"{"openai-codex":{"type":"oauth","accountId":"synthetic-account","access":"e30.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOiB7ImNoYXRncHRfYWNjb3VudF9pZCI6ICJzeW50aGV0aWMtYWNjb3VudCJ9fQ.synthetic","refresh":"SYNTHETIC_REFRESH","expires":9999999999999}}"#).unwrap();
    if mode.starts_with("kimi-") {
        std::fs::write(auth.join("auth.json"), r#"{"kimi-coding":{"type":"oauth","access":"SYNTHETIC_KIMI_ACCESS","refresh":"SYNTHETIC_KIMI_REFRESH","expires":9999999999999}}"#).unwrap();
    }
    let bin = root.join("pi-bin");
    std::fs::create_dir(&bin).unwrap();
    for name in ["node", "git", "python3"] {
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|p| p.join(name))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("test requires installed {name}"));
        let executable = std::fs::canonicalize(executable).unwrap();
        if name == "node" {
            // An explicit fixture launcher keeps the test installation independent of the
            // host's package-manager layout (Homebrew itself has a .git ancestor). Production
            // still rejects every repository-owned executable; the marker-trap regression
            // separately exercises that boundary. This only runs AF's host with synthetic SDK/auth.
            let quoted = executable.to_str().unwrap().replace('\'', "'\\''");
            let launcher = bin.join(name);
            std::fs::write(&launcher, format!("#!/bin/sh\nexec '{quoted}' \"$@\"\n")).unwrap();
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        } else {
            std::os::unix::fs::symlink(executable, bin.join(name)).unwrap();
        }
    }
    (sdk.join("dist/index.js"), bin.into_os_string(), state)
}
