use af_contracts::{Arg, Command};
use af_runner_codex::task::CodexTaskAdapter;

#[test]
fn package_cannot_override_codex_sandbox_or_working_directory() {
    for arguments in [
        vec![Arg::literal("-C"), Arg::literal("/")],
        vec![Arg::literal("-s"), Arg::literal("danger-full-access")],
        vec![
            Arg::literal("-c"),
            Arg::literal("sandbox_workspace_write.network_access=true"),
        ],
    ] {
        let Err(error) = CodexTaskAdapter::new(&Command::new("codex", arguments)) else {
            panic!("a package argument that widens authority was accepted");
        };
        assert!(error.contains("packages may set only"), "{error}");
    }
}
