//! Help pages for fake Provider CLIs (ADR-0146).
//!
//! Before `af provider status`, `af provider setup` or Task Provider admission asks a CLI about
//! its login, af runs the token-free capability check: `claude --help` or `codex exec --help`
//! must declare every option af's invocation passes. A fixture CLI whose subject is something
//! else answers that check through [`with_help`]; a fixture whose subject is the check writes its
//! own page, usually [`CLAUDE_HELP`] or [`CODEX_EXEC_HELP`] with lines removed.

/// The options section of `claude --help` in Claude Code's commander layout, abridged to what af
/// passes plus the usual neighbours; the layout and option names are those of 2.1.299, checked by
/// hand on 2026-10-14.
pub const CLAUDE_HELP: &str = "\
Usage: claude [options] [command] [prompt]

Claude Code - starts an interactive session by default, use -p/--print for
non-interactive output

Options:
  --allowedTools, --allowed-tools <tools...>
                                        Comma or space-separated list of tool
                                        names to allow
  --effort <level>                      Effort level for the current session
  -h, --help                            Display help for command
  --json-schema <schema>                JSON Schema for structured output
  --model <model>                       Model for the current session
  --output-format <format>              Output format (only works with
                                        --print)
  --permission-mode <mode>              Permission mode to use for the session
  -p, --print                           Print response and exit
  --restricted                          Restricted mode: removes the built-in
                                        tools --tools does not name
  --safe-mode                           Start with all customizations disabled
  --strict-mcp-config                   Only use MCP servers from --mcp-config
  --tools <tools...>                    Specify the list of available tools
  -v, --version                         Output the version number
";

/// `codex exec --help` in codex-cli's clap layout, abridged to what af passes plus the usual
/// neighbours; the layout and option names are those of 0.121.0 and 0.162.1, checked by hand on
/// 2026-10-14.
pub const CODEX_EXEC_HELP: &str = "\
Run Codex non-interactively

Usage: codex exec [OPTIONS] [PROMPT]

Options:
  -c, --config <key=value>
          Override a configuration value that would otherwise be loaded from config.toml
  -m, --model <MODEL>
          Model the agent should use
  -s, --sandbox <SANDBOX_MODE>
          Select the sandbox policy to use when executing model-generated shell commands
  -C, --cd <DIR>
          Tell the agent to use the specified directory as its working root
      --skip-git-repo-check
          Allow running Codex outside a Git repository
      --ephemeral
          Run without persisting session files to disk
      --json
          Print events to stdout as JSONL
  -o, --output-last-message <FILE>
          Specifies file where the last message from the agent should be written
  -h, --help
          Print help
";

/// `script` with a first branch, right after its interpreter line, that answers af's capability
/// check with a help page declaring every option af passes. `kind` is `claude` or `codex`; a
/// `python` interpreter line gets a Python branch, anything else a POSIX shell one.
pub fn with_help(kind: &str, script: &str) -> String {
    with_page(kind, script, page(kind))
}

/// `script` answering the capability check with `help` as its page.
pub fn with_page(kind: &str, script: &str, help: &str) -> String {
    let (interpreter, body) = script.split_once('\n').expect("a script with a body");
    assert!(interpreter.starts_with("#!"), "{interpreter}");
    let python = interpreter.contains("python");
    // `printf` is a shell builtin: a fixture `PATH` often holds nothing but the fake itself.
    let quoted = help.replace('\'', "'\\''");
    let branch = match (kind, python) {
        ("claude", false) => {
            format!("if [ \"$1\" = --help ]; then\nprintf '%s' '{quoted}'\nexit 0\nfi\n")
        }
        ("codex", false) => format!(
            "if [ \"$1\" = exec ] && [ \"$2\" = --help ]; then\nprintf '%s' '{quoted}'\nexit 0\nfi\n"
        ),
        (_, true) => {
            let argv = if kind == "claude" {
                "['--help']"
            } else {
                "['exec','--help']"
            };
            format!(
                "import sys as _af_sys\nif _af_sys.argv[1:]=={argv}:\n _af_sys.stdout.write({help:?})\n _af_sys.exit(0)\n"
            )
        }
        _ => panic!("unknown Provider kind {kind}"),
    };
    format!("{interpreter}\n{branch}{body}")
}

/// The help page of a CLI accepting af's whole invocation.
pub fn page(kind: &str) -> &'static str {
    match kind {
        "claude" => CLAUDE_HELP,
        "codex" => CODEX_EXEC_HELP,
        _ => panic!("unknown Provider kind {kind}"),
    }
}
