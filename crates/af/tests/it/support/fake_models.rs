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
use std::path::Path;

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
