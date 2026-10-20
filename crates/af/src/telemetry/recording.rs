//! What a command that runs a kernel run records (design §6, §7.1): its `af.command` span, the
//! subscriber the Task runtime's spans reach, and the segment's seal at the end.
//!
//! Only `af task run`, `af task start --execute`, `af review run`, `af provider doctor` and
//! `af self optimize --execute` record; every other command opens no telemetry directory. At
//! `off` nothing is started. A telemetry-only failure is one line on stderr and the command's
//! outcome is its own (ADR-0153 clause 7).

use review_pipeline::telemetry::RunKind;
use review_telemetry::record::{ProblemWhat, SealExit};
use review_telemetry::subscriber;
use review_telemetry::{Start, Telemetry};

/// One command's recording.
pub(crate) struct Recording {
    telemetry: Option<Telemetry>,
    command: tracing::Span,
    entered: Option<tracing::span::EnteredSpan>,
}

impl Recording {
    fn off() -> Self {
        Self {
            telemetry: None,
            command: tracing::Span::none(),
            entered: None,
        }
    }

    /// End the command: every run span it opened, then its `af.command` span with the exit
    /// code, then the seal. Every telemetry-only problem is one line on stderr.
    pub(crate) fn finish(mut self, code: i32, exit: SealExit) {
        let Some(telemetry) = self.telemetry.take() else {
            return;
        };
        review_pipeline::telemetry::end_runs();
        if !self.command.is_disabled() {
            self.command.record("process.exit_code", i64::from(code));
            self.command
                .record("af.status", if code == 0 { "ok" } else { "error" });
        }
        drop(subscriber::take_command_span());
        drop(self.entered.take());
        drop(std::mem::replace(&mut self.command, tracing::Span::none()));
        let finished = telemetry.finish(exit);
        for (what, reason) in finished.problems {
            eprintln!(
                "af: telemetry problem ({}), the command is unaffected: {reason}",
                what.as_str()
            );
        }
    }
}

/// A write hook a debug build installs for fixtures; release binaries never read these settings.
/// `AF_TEST_TELEMETRY_STALL_AFTER_WRITES=N`: after N writes the consumer thread blocks as a
/// write blocked on the volume would (the appending thread never does). `AF_TEST_TELEMETRY_QUOTA`:
/// every write fails as on a full quota.
fn test_hook() -> Option<review_telemetry::segment::writer::WriteHook> {
    #[cfg(debug_assertions)]
    {
        if std::env::var_os("AF_TEST_TELEMETRY_QUOTA").is_some() {
            return Some(std::sync::Arc::new(|_| {
                Err(std::io::Error::from_raw_os_error(nix::libc::EDQUOT))
            }));
        }
        let after: u64 = std::env::var("AF_TEST_TELEMETRY_STALL_AFTER_WRITES")
            .ok()?
            .parse()
            .ok()?;
        Some(std::sync::Arc::new(move |ordinal: u64| {
            if ordinal > after && std::thread::current().name() == Some("af-telemetry") {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3_600));
                }
            }
            Ok(())
        }))
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// Start recording a command that runs a kernel run of `kind`. Nothing is opened yet beyond
/// what the first record needs: the segment, and with it the directory, opens with the
/// `af.command` span.
pub(crate) fn begin(kind: RunKind) -> Recording {
    review_pipeline::telemetry::set_run_kind(kind);
    let settings = match super::settings() {
        Ok(settings) => settings,
        Err(refusal) => {
            eprintln!(
                "af: telemetry problem (configuration), nothing is recorded and the command is \
                 unaffected: {refusal}"
            );
            return Recording::off();
        }
    };
    if !settings.level.records() {
        return Recording::off();
    }
    let started = (|| -> Result<Start, String> {
        let (accounted, accounted_unknown) = super::accounted()?;
        Ok(Start {
            directory: super::directory(&settings)?,
            registry: super::registry()?,
            accounted,
            accounted_unknown,
            af_version: env!("CARGO_PKG_VERSION").to_string(),
            af_commit: crate::selfmgmt::COMMIT.to_string(),
            settings: settings.clone(),
            write_hook: test_hook(),
        })
    })();
    let start = match started {
        Ok(start) => start,
        Err(reason) => {
            eprintln!(
                "af: telemetry problem ({}), nothing is recorded and the command is unaffected: \
                 {reason}",
                ProblemWhat::Write.as_str()
            );
            return Recording::off();
        }
    };
    let mut telemetry = Telemetry::start(start);
    telemetry.install_subscriber();
    let command = tracing::info_span!(
        "af.command",
        process.command = "af",
        af.store_id = tracing::field::Empty,
        af.run_id = tracing::field::Empty,
        process.exit_code = tracing::field::Empty,
        af.status = tracing::field::Empty,
    );
    subscriber::set_command_span(&command);
    let entered = command.clone().entered();
    Recording {
        telemetry: Some(telemetry),
        command,
        entered: Some(entered),
    }
}
