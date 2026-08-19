//! Typed argument slots.
//!
//! A check command is not a string. The shell harness took `<name><TAB><shell command>` and ran
//! it through `bash -c`, with `{tests}` placeholders filled from the bundle — which means paths
//! **taken from the diff under review** were spliced into a command line. A file added in a PR
//! and named `--config=/tmp/evil` is not exotic; it is one `git add` away.
//!
//! So the vector is typed instead. The program and every option are trusted literals from
//! project configuration. Values derived from the change are [`Provenance::Untrusted`], and an
//! untrusted value may never occupy an option position: no leading `-`, no `@response-file`, no
//! embedded NUL. The check declares where untrusted values go; nothing else can put them there.
//!
//! Refusing is deliberate, rather than escaping or quoting. Quoting depends on the program's own
//! option syntax, which the kernel does not know and must not guess.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// From trusted project configuration. May be an option.
    Literal,
    /// Derived from the change under review. Never an option.
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arg {
    pub value: String,
    pub provenance: Provenance,
}

impl Arg {
    pub fn literal(value: impl Into<String>) -> Arg {
        Arg {
            value: value.into(),
            provenance: Provenance::Literal,
        }
    }

    pub fn untrusted(value: impl Into<String>) -> Arg {
        Arg {
            value: value.into(),
            provenance: Provenance::Untrusted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// An untrusted value that would be read as an option.
    OptionInjection {
        value: String,
    },
    /// An untrusted value that would be read as a response file (`@path`), which several
    /// toolchains expand into *more arguments* read from disk.
    ResponseFile {
        value: String,
    },
    /// A NUL cannot survive an argv boundary; silently truncating one would change the command.
    EmbeddedNul {
        value: String,
    },
    EmptyProgram,
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::OptionInjection { value } => write!(
                f,
                "untrusted value {value:?} would be read as an option; a check must pass such values in a value position"
            ),
            ArgError::ResponseFile { value } => write!(
                f,
                "untrusted value {value:?} would be read as a response file"
            ),
            ArgError::EmbeddedNul { value } => {
                write!(f, "untrusted value {value:?} contains a NUL")
            }
            ArgError::EmptyProgram => write!(f, "a check must name a program"),
        }
    }
}

impl std::error::Error for ArgError {}

/// A check's command: a trusted program and a typed argument vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub program: String,
    pub args: Vec<Arg>,
}

impl Command {
    pub fn new(program: impl Into<String>, args: Vec<Arg>) -> Command {
        Command {
            program: program.into(),
            args,
        }
    }

    /// Validate every slot, returning the argv to execute.
    ///
    /// Called before anything is spawned, so a rejected command never runs at all rather than
    /// running with a sanitized approximation of what was asked for.
    pub fn resolve(&self) -> Result<Vec<String>, ArgError> {
        if self.program.trim().is_empty() {
            return Err(ArgError::EmptyProgram);
        }
        let mut argv = Vec::with_capacity(self.args.len());
        for arg in &self.args {
            if arg.value.contains('\0') {
                return Err(ArgError::EmbeddedNul {
                    value: arg.value.clone(),
                });
            }
            if arg.provenance == Provenance::Untrusted {
                if arg.value.starts_with('-') {
                    return Err(ArgError::OptionInjection {
                        value: arg.value.clone(),
                    });
                }
                if arg.value.starts_with('@') {
                    return Err(ArgError::ResponseFile {
                        value: arg.value.clone(),
                    });
                }
            }
            argv.push(arg.value.clone());
        }
        Ok(argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_option_is_fine() {
        let command = Command::new(
            "cargo",
            vec![Arg::literal("test"), Arg::literal("--locked")],
        );
        assert_eq!(command.resolve().unwrap(), vec!["test", "--locked"]);
    }

    #[test]
    fn the_same_bytes_are_refused_when_they_come_from_the_change() {
        for hostile in ["--locked", "-rf", "--config=/tmp/evil"] {
            let command = Command::new("cargo", vec![Arg::untrusted(hostile)]);
            assert!(
                matches!(command.resolve(), Err(ArgError::OptionInjection { .. })),
                "{hostile} was accepted"
            );
            // ...and the identical string is fine when the project itself wrote it.
            assert!(
                Command::new("cargo", vec![Arg::literal(hostile)])
                    .resolve()
                    .is_ok()
            );
        }
    }

    #[test]
    fn a_response_file_is_refused() {
        let command = Command::new("clang", vec![Arg::untrusted("@/tmp/args.rsp")]);
        assert!(matches!(
            command.resolve(),
            Err(ArgError::ResponseFile { .. })
        ));
    }

    #[test]
    fn ordinary_untrusted_paths_pass_through_untouched() {
        // The point is not to reject values — it is to reject *option positions*. A test
        // selector from the diff must survive exactly as written.
        let command = Command::new(
            "./tests/hub-test",
            vec![
                Arg::literal("--"),
                Arg::untrusted("tests/queries/03974_negative_boundary.sql"),
                Arg::untrusted("a file with spaces.sql"),
                Arg::untrusted("weird;but$fine.sql"),
            ],
        );
        assert_eq!(
            command.resolve().unwrap(),
            vec![
                "--",
                "tests/queries/03974_negative_boundary.sql",
                "a file with spaces.sql",
                "weird;but$fine.sql"
            ]
        );
    }

    #[test]
    fn a_nul_is_refused_rather_than_truncated() {
        let command = Command::new("echo", vec![Arg::untrusted("ok\0hidden")]);
        assert!(matches!(
            command.resolve(),
            Err(ArgError::EmbeddedNul { .. })
        ));
    }

    #[test]
    fn a_check_must_name_a_program() {
        assert_eq!(
            Command::new("  ", vec![]).resolve(),
            Err(ArgError::EmptyProgram)
        );
    }
}
