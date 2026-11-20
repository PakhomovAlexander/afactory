//! The application wires native Tasks, Review, Scenarios and machine services.
//! Review entrypoints share Task execution, state, accounting and recovery.
use std::io::Write;
use std::path::{Path, PathBuf};
mod af_paths;
mod agent_setup;
mod caches;
mod cli;
mod command_catalog;
mod config;
mod doctor;
mod error_document;
mod human;
mod interrupt;
mod model_check;
mod model_failures;
mod onboard;
mod power;
mod project;
mod project_policy;
mod providers;
mod review_task;
mod sarif;
mod scenario;
mod self_management;
mod self_optimizer;
mod shipped_schemas;
mod storage;
mod store_release;
mod task_commands;
mod task_delivery;
mod telemetry;
mod topics;
mod tui;

fn xdg_state_root() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("XDG_STATE_HOME") {
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err("XDG_STATE_HOME must be absolute".to_string());
        }
        return Ok(config::canonical_root(&configured));
    }
    let user_home =
        std::env::var_os("HOME").ok_or("HOME is not set and XDG_STATE_HOME is absent")?;
    let user_home = PathBuf::from(user_home);
    if !user_home.is_absolute() {
        return Err("HOME must be absolute".to_string());
    }
    Ok(config::canonical_root(&user_home.join(".local/state")))
}

fn resolve_filesystem_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("reading current directory: {error}"))?
            .join(path)
    };
    let absolute = normalize_absolute(&absolute)?;
    let mut existing = absolute.clone();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("path {} has no existing ancestor", absolute.display()))?
            .to_os_string();
        suffix.push(name);
        existing
            .pop()
            .then_some(())
            .ok_or_else(|| format!("path {} has no existing ancestor", absolute.display()))?;
    }
    let mut resolved = std::fs::canonicalize(&existing).map_err(|error| {
        format!(
            "opening {}: {error}",
            terminal_text(&existing.display().to_string())
        )
    })?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    normalize_absolute(&resolved)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "path {} escapes its filesystem root",
                        path.display()
                    ));
                }
            }
        }
    }
    if !normalized.is_absolute() {
        return Err(format!(
            "path {} did not resolve absolutely",
            path.display()
        ));
    }
    Ok(normalized)
}

fn help_command(words: &[String], snippet: bool, json: bool) -> Result<i32, String> {
    use clap::CommandFactory as _;
    if json {
        return help_catalog(words);
    }
    if snippet {
        if words == ["agents"] {
            print!("{}", topics::agent_snippet());
            return Ok(0);
        }
        return Err("--snippet belongs to `af help agents`; run af help agents --snippet".into());
    }
    let mut root = cli::Af::command();
    if words.is_empty() {
        root.print_long_help().map_err(|error| error.to_string())?;
        println!("\n{}", topics::listing());
        return Ok(0);
    }
    match topics::help_page(words) {
        Some(Ok(page)) => {
            println!("{}", page.text_form());
            return Ok(0);
        }
        // `errors` and no known cause name no page: usage, as an unknown topic is.
        Some(Err(error)) => {
            eprintln!("af help: {}", terminal_text(&error));
            return Ok(2);
        }
        None => {}
    }
    let mut current = &mut root;
    for word in words {
        match current.find_subcommand_mut(word) {
            Some(sub) => current = sub,
            None => {
                let topics: Vec<&str> = topics::TOPICS.iter().map(|(name, _, _)| *name).collect();
                eprintln!(
                    "error: Topic or command {} not found in af help; topics: {}\n  try: af --help",
                    terminal_text(&words.join(" ")),
                    topics.join(", ")
                );
                return Ok(2);
            }
        }
    }
    let name = current.get_name().to_string();
    let path = format!("af {}", words.join(" "));
    current
        .clone()
        .name(name)
        .bin_name(path)
        .print_long_help()
        .map_err(|error| error.to_string())?;
    Ok(0)
}

fn help_catalog(words: &[String]) -> Result<i32, String> {
    let document = match command_catalog::document(words) {
        Ok(document) => document,
        Err(_) => match topics::help_page(words) {
            Some(Ok(page)) => page.document(),
            Some(Err(error)) => return Ok(help_refusal(&error)),
            None => {
                return Ok(help_refusal(&format!(
                    "no command or help page named {}; run af help for every page, or af help \
                     --json for every command",
                    words.join(" ")
                )));
            }
        },
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?
    );
    Ok(0)
}

fn help_refusal(error: &str) -> i32 {
    let error = terminal_text(error);
    println!("{}", error_document::document("af help", &error, 2, None));
    eprintln!("af help: {error}");
    2
}

fn main() {
    use af_contracts::perf_trace::{self, Phase};
    use clap::{CommandFactory as _, Parser as _};
    // Inert outside a `perf-trace` diagnostic build (ADR-0174): startup composition, then the
    // dispatched command, exported when the command returns through here.
    let startup = perf_trace::span(Phase::CliStartup);
    let argv: Vec<String> = match std::env::args_os()
        .map(|argument| argument.into_string())
        .collect()
    {
        Ok(argv) => argv,
        Err(argument) => {
            eprintln!(
                "af: command-line argument {} is not valid UTF-8",
                terminal_text(&argument.to_string_lossy())
            );
            std::process::exit(2);
        }
    };
    // The calibration loop of a laboratory slowdown trial runs inside this process, before
    // anything else (ADR-0181); a build without `perf-calibration` refuses its control.
    if let Err(error) = af_contracts::perf_calibration::configure_from_env() {
        eprintln!("af: {error}");
        std::process::exit(2);
    }
    if af_contracts::perf_calibration::inject().is_some()
        && let Some(line) = af_contracts::perf_calibration::report_line()
    {
        eprintln!("{line}");
    }
    if let Some(code) = providers::auth_handoff::supervise_early(&argv) {
        std::process::exit(code);
    }
    clap_complete::CompleteEnv::with_factory(cli::Af::command).complete();
    // Keep the result (including clap's rendered help/error) for ordinary dispatch rather
    // than constructing and parsing the same command tree a second time.
    let parsed = cli::Af::try_parse_from(&argv).map_err(cli::usage_error);
    // `af catalog digest` reads one directory and writes nothing (ADR-0157), so it runs and
    // exits before every self-management hook: it never dispatches to or installs a pinned
    // release, schedules an update check or touches af's own state.
    if let Ok(cli::Af {
        version: false,
        command:
            Some(cli::Command::Catalog {
                command: cli::CatalogCommand::Digest { package },
            }),
        ..
    }) = &parsed
    {
        if let Err(error) = task_commands::catalog::digest(package) {
            eprintln!("af catalog digest: {error}");
            std::process::exit(1);
        }
        return;
    }
    self_management::maybe_dispatch(&argv);
    // `parse_from`, with the trace exported before help, usage and errors exit.
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            drop(startup);
            perf_trace::finish();
            // Under --json a usage error is the one `af/error@2` document `af help json`
            // promises, beside clap's own message on stderr and with its exit code.
            if let Some(document) = error_document::usage(&argv, &error) {
                println!("{document}");
                let _ = std::io::stdout().flush();
            }
            error.exit()
        }
    };
    if parsed.version {
        if let Err(error) = self_management::print_version(parsed.json) {
            eprintln!("af: {}", terminal_lines(&error.to_string()));
            std::process::exit(1);
        }
        drop(startup);
        perf_trace::finish();
        return;
    }
    let Some(command) = parsed.command else {
        if !tui::wanted() {
            // Not a terminal: the usage error a bare `af` has always been, help on stderr, after
            // the first-run hints.
            eprintln!("{}\n", cli::FIRST_RUN_HINTS);
            let refused = cli::Af::command()
                .arg_required_else_help(true)
                .try_get_matches_from(["af"]);
            match refused {
                Err(error) => error.exit(),
                Ok(_) => std::process::exit(2),
            }
        }
        let code = tui::launch(parsed.repo.as_deref()).unwrap_or_else(|error| {
            eprintln!("af: {}", terminal_lines(&error.to_string()));
            1
        });
        self_management::after_command(&argv);
        std::process::exit(code);
    };
    // Still the only thread: every thread started from here inherits the blocked signals. A run
    // `af task run --detach` started outlives the terminal that started it (ADR-0211).
    if std::env::var_os(task_commands::runs::DETACHED_ENV).is_some()
        && matches!(
            &command,
            cli::Command::Task {
                command: cli::TaskCommand::Run { .. }
            }
        )
        && let Err(error) = interrupt::ignore_hangup()
    {
        eprintln!("af: {}", terminal_lines(&error.to_string()));
        std::process::exit(1);
    }
    if needs_interrupt_handling(&command)
        && let Err(error) = interrupt::install()
    {
        eprintln!("af: {}", terminal_lines(&error.to_string()));
        std::process::exit(1);
    }
    // The Storage Budget first: it names the sandbox root (`[storage] sandbox_root`) the stale
    // sandbox sweep below looks in.
    storage::set_output(runs_task_work(&command), verbose_sweeps(&command));
    // A Mac does not idle-sleep while Task work runs; `af task run --verbose` says so
    // (ADR-0209). Held until af exits.
    let _power = runs_task_work(&command)
        .then(|| {
            power::hold(matches!(
                &command,
                cli::Command::Task {
                    command: cli::TaskCommand::Run { verbose: true, .. }
                }
            ))
        })
        .flatten();
    if runs_task_work(&command)
        && let Err(error) = storage::install()
    {
        // The free-disk floor and the budget step before a new warm key (ADR-0144). Without
        // them no check or Worker may start, so work is refused rather than run unguarded.
        eprintln!(
            "af: the storage budget cannot be set up, so no Task, review or Scenario work starts: {}; \
             see `af config show --origin` for [storage] and the XDG directories",
            terminal_lines(&error.to_string())
        );
        std::process::exit(1);
    }
    let sandbox_sweep = runs_task_work(&command)
        .then(sweep_stale_sandboxes_in_background)
        .flatten();
    let collects_after = collects_after_run(&command);
    let task_work = runs_task_work(&command);
    // A command that runs a kernel run records it (ADR-0153); every other command opens no
    // telemetry directory.
    let mut recording =
        recorded_run(&command).map(|kind| telemetry::begin(kind, telemetry::command_chain(&argv)));
    // Registered best effort (ADR-0158): `af task plan|start` before `--workspace` resolves,
    // every other command once it ran.
    let registers = project::lazy_target(&command);
    if let Some(lazy) = registers.as_ref().filter(|lazy| lazy.first) {
        project::register_lazily(lazy);
    }
    let delivered = project::delivered_target(&command);
    // `af onboard --apply` records the first Survey once it registered (ADR-0159).
    let surveyed = match &command {
        cli::Command::Onboard(args) if args.apply => Some((args.repo.clone(), args.json)),
        _ => None,
    };
    // `af task events` prints JSON lines with no --json: its refusal is an `af/error@2` line too.
    let json_lines = matches!(
        &command,
        cli::Command::Task {
            command: cli::TaskCommand::Events { .. }
        }
    );
    drop(startup);
    let dispatched = perf_trace::span(Phase::CliCommand);
    let (prefix, outcome): (&str, Result<i32, String>) = match command {
        cli::Command::Review(namespace) => ("af review", review_task::run(namespace.command)),
        cli::Command::Provider { command } => ("af provider", provider_command(command)),
        cli::Command::Onboard(args) => ("af onboard", onboard::run_cli(args).map(|()| 0)),
        cli::Command::Catalog {
            command:
                cli::CatalogCommand::Init {
                    profile,
                    developer_public_key,
                    repo,
                    destination,
                    json,
                },
        } => (
            "af catalog init",
            task_commands::starter::init(
                &repo,
                &destination,
                &profile,
                developer_public_key.as_deref(),
                json,
            )
            .map(|()| 0),
        ),
        cli::Command::Catalog {
            command:
                cli::CatalogCommand::Test {
                    source,
                    revision,
                    manifest,
                    fixtures,
                    pipeline,
                    worker,
                    json,
                },
        } => (
            "af catalog test",
            task_commands::catalog::test(
                &source,
                &revision,
                &manifest,
                fixtures.as_deref(),
                pipeline.as_deref(),
                worker.as_deref(),
                json,
            )
            .map(|()| 0),
        ),
        cli::Command::Catalog {
            command: cli::CatalogCommand::Digest { package },
        } => (
            "af catalog digest",
            task_commands::catalog::digest(&package).map(|()| 0),
        ),
        cli::Command::Catalog {
            command:
                cli::CatalogCommand::Sync {
                    source,
                    revision,
                    manifest,
                    repo,
                    destination,
                    json,
                },
        } => (
            "af catalog sync",
            task_commands::catalog::sync(&source, &revision, &manifest, &repo, &destination, json)
                .map(|()| 0),
        ),
        cli::Command::Task { command } => ("af task", task_command(command)),
        cli::Command::Scenario { command } => ("af scenario", scenario::scenario(command)),
        cli::Command::Ant { command } => ("af ant", scenario::ant(command)),
        cli::Command::Project {
            command:
                cli::ProjectCli::Brief {
                    variant,
                    survey,
                    json,
                },
        } => ("af project", project::brief::command(variant, survey, json)),
        cli::Command::Project {
            command: cli::ProjectCli::Core(command),
        } => {
            let sections = match &command {
                cli::ProjectCommand::Survey { json: false } => cli::survey_sections(&argv),
                _ => Vec::new(),
            };
            ("af project", project::survey::run(command, &sections))
        }
        cli::Command::Config { command } => (
            "af config",
            match command {
                cli::ConfigCommand::Show { origin, json, repo } => {
                    config::show(repo.as_deref(), origin, json).map(|()| 0)
                }
                cli::ConfigCommand::Edit { layer, repo } => {
                    config::edit(layer, repo.as_deref()).map(|()| 0)
                }
                cli::ConfigCommand::Paths { repo } => config::paths(repo.as_deref()).map(|()| 0),
            },
        ),
        cli::Command::SelfCmd { command } => (
            "af self",
            match command {
                cli::SelfCommand::Optimize {
                    since,
                    all_history,
                    strategy,
                    history_config,
                    execute,
                    experiment,
                    candidate,
                    repo,
                    state,
                    json,
                } => self_optimizer::run(self_optimizer::Options {
                    since,
                    all_history,
                    strategy,
                    history_config,
                    execute,
                    experiment,
                    candidate,
                    repo,
                    state,
                    json,
                }),
                cli::SelfCommand::Status { json, refresh } => {
                    self_management::status(json, refresh).map(|()| 0)
                }
                cli::SelfCommand::Update { check, version, rc } => {
                    self_management::update(check, version, rc).map(|()| 0)
                }
                cli::SelfCommand::Channel { channel, no_update } => {
                    self_management::channel(channel.map(Into::into), no_update).map(|()| 0)
                }
                cli::SelfCommand::Rollback => self_management::rollback().map(|()| 0),
                cli::SelfCommand::Install { version } => {
                    self_management::install_command(&version).map(|()| 0)
                }
                cli::SelfCommand::Remove { version } => {
                    self_management::remove(&version).map(|()| 0)
                }
                cli::SelfCommand::Prune => self_management::prune().map(|()| 0),
                cli::SelfCommand::SetupShell {
                    shell,
                    write,
                    remove,
                } => self_management::setup_shell(shell, write, remove).map(|()| 0),
                cli::SelfCommand::Uninstall { purge } => {
                    self_management::uninstall(purge).map(|()| 0)
                }
                cli::SelfCommand::Man { out_dir } => self_management::man(&out_dir).map(|()| 0),
                cli::SelfCommand::RefreshCheck => self_management::refresh_check().map(|()| 0),
            },
        ),
        cli::Command::Storage {
            json,
            verbose,
            command,
        } => (
            "af storage",
            match command {
                None => storage::show(json, verbose),
                Some(cli::StorageCommand::Prune {
                    apply,
                    json: prune_json,
                }) => storage::prune(apply, json || prune_json, verbose),
            },
        ),
        cli::Command::Telemetry { command } => match command {
            cli::TelemetryCommand::Status { json, dir } => {
                ("af telemetry status", telemetry::status(json, dir))
            }
            cli::TelemetryCommand::Reindex { json, dir } => {
                ("af telemetry reindex", telemetry::reindex(json, dir))
            }
            cli::TelemetryCommand::Runs {
                since,
                kind,
                json,
                dir,
            } => ("af telemetry runs", telemetry::runs(json, dir, since, kind)),
            cli::TelemetryCommand::Show {
                run,
                store,
                attempt,
                span,
                depth,
                json,
                dir,
            } => (
                "af telemetry show",
                telemetry::show(json, dir, run, store, attempt, span, depth),
            ),
            cli::TelemetryCommand::Turns {
                run,
                store,
                attempt,
                json,
                dir,
            } => (
                "af telemetry turns",
                telemetry::turns(json, dir, run, store, attempt),
            ),
            cli::TelemetryCommand::Tools {
                run,
                store,
                attempt,
                json,
                dir,
            } => (
                "af telemetry tools",
                telemetry::tools(json, dir, run, store, attempt),
            ),
            cli::TelemetryCommand::Context {
                run,
                attempt,
                store,
                raw,
                json,
                dir,
            } => (
                "af telemetry context",
                telemetry::context(json, raw, dir, run, attempt, store),
            ),
            cli::TelemetryCommand::Reasoning {
                run,
                attempt,
                store,
                json,
                dir,
            } => (
                "af telemetry reasoning",
                telemetry::reasoning(json, dir, run, attempt, store),
            ),
            cli::TelemetryCommand::Search {
                text,
                since,
                run,
                store,
                json,
                dir,
            } => (
                "af telemetry search",
                telemetry::search(json, dir, text, since, run, store),
            ),
            cli::TelemetryCommand::Config {
                run,
                store,
                execution,
                json,
                dir,
            } => (
                "af telemetry config",
                telemetry::config(json, dir, run, store, execution),
            ),
            cli::TelemetryCommand::Tail {
                run,
                store,
                json,
                dir,
            } => ("af telemetry tail", telemetry::tail(json, dir, run, store)),
            cli::TelemetryCommand::Ledger {
                run,
                store,
                json,
                dir,
            } => (
                "af telemetry ledger",
                telemetry::ledger(json, dir, run, store),
            ),
            cli::TelemetryCommand::Gc { apply, json } => {
                ("af telemetry gc", telemetry::gc(json, apply))
            }
        },
        cli::Command::Completions { shell } => (
            "af completions",
            self_management::completion_script(shell).map(|script| {
                print!("{script}");
                0
            }),
        ),
        cli::Command::Doctor(args) => ("af doctor", doctor::run(&args.repo, args.json)),
        cli::Command::Help {
            words,
            snippet,
            json,
        } => ("af help", help_command(&words, snippet, json)),
        cli::Command::Schema { command } => match command {
            cli::SchemaCommand::Show { id } => {
                ("af schema show", shipped_schemas::show(id.as_deref()))
            }
        },
        cli::Command::Setup { command } => match command {
            cli::SetupCommand::Agents { repo, write } => {
                ("af setup agents", agent_setup::run(&repo, write))
            }
        },
    };
    if let Some(signal) = interrupt::received() {
        task_commands::print_last_lines();
        if let Some(recording) = recording.take() {
            recording.finish(
                interrupt::exit_code(signal),
                af_telemetry::record::SealExit::Signal,
            );
        }
        exit_interrupted(&argv, prefix, signal, outcome.err());
    }
    if let Some(lazy) = registers.as_ref().filter(|lazy| !lazy.first) {
        project::register_lazily(lazy);
    }
    if let Some((repo, json)) = &surveyed {
        project::survey::after_onboard(repo, *json);
    }
    if let (Some((worktree, task_id)), Ok(0)) = (&delivered, &outcome) {
        project::register_delivered(worktree, task_id);
    }
    if collects_after {
        // Whatever the run's outcome; a failure here is a warning, never the exit status.
        storage::after_run();
    }
    if task_work {
        // One line for every sweep the command ran, the floor's and a new warm key's included.
        storage::flush_summary();
    }
    let code = match outcome {
        Ok(code) => code,
        Err(error) => {
            // A classified refusal adds its cause, next command and facts (`af help json`).
            let detail = error_document::take(&error);
            if json_lines || argv.iter().skip(1).any(|word| word == "--json") {
                let document = error_document::document(prefix, &error, 1, detail.as_ref());
                println!("{document}");
            }
            eprintln!("{}", error_text(prefix, &error, detail.as_ref()));
            1
        }
    };
    if let Some(recording) = recording.take() {
        recording.finish(
            code,
            if code == 0 {
                af_telemetry::record::SealExit::Ok
            } else {
                af_telemetry::record::SealExit::Error
            },
        );
    }
    if let Some(sweep) = sandbox_sweep {
        // The command's own work is done; a sweep still removing a large leftover finishes
        // before the process ends, so a stale directory never outlives two Task-running commands.
        if let Ok(Some(line)) = sweep.join() {
            eprintln!("{line}");
        }
    }
    drop(dispatched);
    perf_trace::finish();
    self_management::after_command(&argv);
    // The run log's last line, the Task's state and next command, after everything the
    // command's housekeeping printed: a script reads it as stderr's last line.
    task_commands::print_last_lines();
    if code != 0 {
        std::process::exit(code);
    }
}

fn task_command(command: cli::TaskCommand) -> Result<i32, String> {
    let command = project::resolve_task_workspace(command)?;
    match command {
        cli::TaskCommand::Wake { command, inspect } => {
            task_commands::wake::execute(command, &inspect)
        }
        cli::TaskCommand::Start {
            file,
            execute,
            verbose: _,
            bindings,
            source_bindings,
            repo,
            state,
            authority,
            input_json,
            uncommitted,
            timeout_secs,
            json,
            workspace: _,
        } => task_commands::start(task_commands::StartOptions {
            review_inputs: None,
            review_selectors: None,
            file,
            bindings,
            source_bindings,
            repo,
            state,
            authority,
            input_json,
            uncommitted,
            json,
            plan_only: !execute,
            timeout_secs,
            optimization_history: None,
            fit: false,
            replace: false,
        }),
        cli::TaskCommand::Plan {
            file,
            bindings,
            source_bindings,
            repo,
            state,
            authority,
            input_json,
            uncommitted,
            fit,
            replace,
            json,
            workspace: _,
        } => {
            // A plan reads each bound Provider's catalog token-free and names every
            // Model binding's verdict (ADR-0148).
            model_check::enable_plan_time();
            task_commands::start(task_commands::StartOptions {
                review_inputs: None,
                review_selectors: None,
                file,
                bindings,
                source_bindings,
                repo,
                state,
                authority,
                input_json,
                uncommitted,
                json,
                plan_only: true,
                timeout_secs: None,
                optimization_history: None,
                fit,
                replace,
            })
        }
        cli::TaskCommand::DecisionPayload {
            task_id,
            developer,
            decision,
            reason,
            output,
            inspect,
        } => task_commands::developer::payload_file(
            &task_id,
            &developer,
            if decision == "approved" {
                af_contracts::task::plan::PlanDecisionKindV1::Approved
            } else {
                af_contracts::task::plan::PlanDecisionKindV1::Rejected
            },
            &reason,
            &output,
            &inspect,
        ),
        cli::TaskCommand::Approve {
            task_id,
            payload,
            signature,
            inspect,
        } => task_commands::developer::apply(
            &task_id,
            af_contracts::task::plan::PlanDecisionKindV1::Approved,
            &payload,
            &signature,
            &inspect,
        ),
        cli::TaskCommand::Reject {
            task_id,
            payload,
            signature,
            inspect,
        } => task_commands::developer::apply(
            &task_id,
            af_contracts::task::plan::PlanDecisionKindV1::Rejected,
            &payload,
            &signature,
            &inspect,
        ),
        cli::TaskCommand::Refresh {
            task_id,
            source_file,
            source_bindings,
            inspect,
        } => task_commands::refresh::refresh(
            &task_id,
            source_file.as_deref(),
            source_bindings.as_deref(),
            &inspect,
        ),
        cli::TaskCommand::Run {
            task_id,
            detach: true,
            wake_route,
            inspect,
            ..
        } => task_commands::runs::detach(&task_id, &inspect, wake_route.as_deref()),
        // `--from` conflicts with `--detach` at parse time (cli.rs), so the detach arm never
        // sees a `--from`; the continue arm precedes the foreground arm, which matches `..`.
        cli::TaskCommand::Run {
            task_id,
            from: Some(from),
            round,
            goal,
            bindings,
            execute,
            inspect,
            ..
        } => task_commands::run_from(task_commands::ContinueOptions {
            task_id,
            from,
            round,
            goal,
            bindings,
            repo: inspect.repo,
            state: inspect.state,
            json: inspect.json,
            execute,
        }),
        cli::TaskCommand::Run {
            task_id,
            confirm_plan,
            execute,
            retry_gate,
            verbose: _,
            detach: false,
            quiet,
            inspect,
            ..
        } => task_commands::run(
            &task_id,
            &inspect.repo,
            inspect.state.as_deref(),
            inspect.json,
            confirm_plan.as_deref(),
            execute,
            retry_gate,
            quiet,
        ),
        cli::TaskCommand::Wait {
            task_id,
            timeout,
            bell,
            notify,
            inspect,
        } => task_commands::runs::wait(&task_id, timeout, bell, notify, &inspect),
        cli::TaskCommand::Status { task_id, inspect } => {
            task_commands::runs::status(&task_id, &inspect)
        }
        cli::TaskCommand::Events {
            task_id,
            after,
            timeout,
            store,
        } => task_commands::events::events(&task_id, after, timeout, &store),
        cli::TaskCommand::Cancel { task_id, inspect } => {
            task_commands::runs::cancel(&task_id, &inspect)
        }
        cli::TaskCommand::Findings {
            task_id,
            round,
            format,
            inspect,
        } => task_commands::findings::print(&task_id, round, format, &inspect),
        cli::TaskCommand::Materialize {
            task_id,
            snapshot,
            into,
            allow_unverified,
            force,
            inspect,
        } => task_commands::candidate::materialize(
            &task_id,
            snapshot.as_deref(),
            &into,
            allow_unverified,
            force,
            &inspect,
        ),
        cli::TaskCommand::Diff {
            task_id,
            snapshot,
            base,
            stat,
            name_only,
            inspect,
        } => task_commands::candidate::diff(
            &task_id,
            snapshot.as_deref(),
            base.as_deref(),
            stat,
            name_only,
            &inspect,
        ),
        cli::TaskCommand::Output {
            task_id,
            port,
            format,
            output,
            inspect,
        } => task_commands::domain::write_output(
            &task_id,
            &port,
            &format,
            output.as_deref(),
            &inspect.repo,
            inspect.state.as_deref(),
            inspect.json,
        ),
        cli::TaskCommand::Export {
            task_id,
            name,
            destination,
            inspect,
        } => task_commands::export::run(
            &task_id,
            &name,
            &destination,
            &inspect.repo,
            inspect.state.as_deref(),
            inspect.json,
        ),
        cli::TaskCommand::Explain {
            task_id,
            tree,
            plan,
            inspect,
        } => task_commands::explain(
            &task_id,
            &inspect.repo,
            inspect.state.as_deref(),
            inspect.json,
            plan.as_deref(),
            tree,
        ),
        cli::TaskCommand::Deliver {
            task_id,
            repo,
            branch,
            worktree,
            confirm,
            state,
            json,
            workspace: _,
        } => {
            task_delivery::delivery_from_cli(task_id, repo, branch, worktree, confirm, state, json)
                .and_then(task_delivery::deliver)
                .map(|()| 0)
        }
        cli::TaskCommand::ObserveAdoption {
            task_id,
            commit,
            workload,
            model,
            environment,
            evidence_task,
            inspect,
        } => task_delivery::observe_adoption(
            &task_id,
            &commit,
            &workload,
            &model,
            &environment,
            evidence_task.as_deref(),
            &inspect.repo,
            inspect.state.as_ref(),
            inspect.json,
        )
        .map(|()| 0),
        cli::TaskCommand::List {
            sizes,
            attention,
            inspect,
        } => task_delivery::inspect_from_cli(None, inspect.repo, inspect.state, inspect.json)
            .and_then(|options| task_delivery::list(options, sizes, attention))
            .map(|()| 0),
        cli::TaskCommand::Gc {
            older_than,
            keep,
            apply,
            inspect,
        } => task_commands::collection::gc(
            older_than,
            keep,
            apply,
            &inspect.repo,
            inspect.state.as_deref(),
            inspect.json,
        ),
        cli::TaskCommand::Show {
            task_id,
            checks,
            inspect,
        } => task_delivery::inspect_from_cli(
            Some(task_id),
            inspect.repo,
            inspect.state,
            inspect.json,
        )
        .and_then(|options| task_delivery::show(options, checks))
        .map(|()| 0),
        cli::TaskCommand::Report {
            task_ids,
            pr_body,
            template,
            base,
            inspect,
        } => task_delivery::inspect_from_cli(None, inspect.repo, inspect.state, inspect.json)
            .and_then(|options| {
                let body = pr_body.then_some(task_delivery::PrBody {
                    template: template.as_deref(),
                    base: base.as_deref(),
                });
                task_delivery::report(options, &task_ids, body)
            })
            .map(|()| 0),
    }
}

fn runs_task_work(command: &cli::Command) -> bool {
    match command {
        cli::Command::Review(namespace) => {
            matches!(
                namespace.command,
                cli::ReviewCommand::Run(_)
                    | cli::ReviewCommand::Resume { .. }
                    | cli::ReviewCommand::Continue { execute: true, .. }
                    | cli::ReviewCommand::Decide { execute: true, .. }
            )
        }
        cli::Command::Provider { command } => matches!(command, cli::ProviderCommand::Doctor(_)),
        // `af task run --detach` only starts the run that does the work, in the background.
        cli::Command::Task { command } => matches!(
            command,
            cli::TaskCommand::Start { execute: true, .. }
                | cli::TaskCommand::Run { detach: false, .. }
        ),
        cli::Command::SelfCmd { command } => {
            matches!(command, cli::SelfCommand::Optimize { execute: true, .. })
        }
        cli::Command::Scenario { command } => matches!(
            command,
            scenario::ScenarioCommand::Start { execute: true, .. }
                | scenario::ScenarioCommand::Resume { .. }
        ),
        cli::Command::Ant { command } => matches!(
            command,
            scenario::AntCommand::Tick { .. }
                | scenario::AntCommand::Scheduler {
                    command: scenario::SchedulerCommand::Run { .. }
                }
        ),
        _ => false,
    }
}

fn verbose_sweeps(command: &cli::Command) -> bool {
    matches!(
        command,
        cli::Command::Task {
            command: cli::TaskCommand::Start { verbose: true, .. }
                | cli::TaskCommand::Run { verbose: true, .. }
        }
    )
}

fn recorded_run(command: &cli::Command) -> Option<af_runtime::telemetry::RunKind> {
    use af_runtime::telemetry::RunKind;
    match command {
        cli::Command::Review(namespace) => matches!(
            namespace.command,
            cli::ReviewCommand::Run(_)
                | cli::ReviewCommand::Resume { .. }
                | cli::ReviewCommand::Continue { execute: true, .. }
                | cli::ReviewCommand::Decide { execute: true, .. }
        )
        .then_some(RunKind::Review),
        cli::Command::Provider {
            command: cli::ProviderCommand::Doctor(_),
        } => Some(RunKind::Review),
        cli::Command::Task {
            command:
                cli::TaskCommand::Start { execute: true, .. }
                | cli::TaskCommand::Run { detach: false, .. },
        } => Some(RunKind::Task),
        cli::Command::SelfCmd {
            command: cli::SelfCommand::Optimize { execute: true, .. },
        } => Some(RunKind::Optimize),
        _ => None,
    }
}

fn collects_after_run(command: &cli::Command) -> bool {
    match command {
        cli::Command::Review(namespace) => {
            matches!(
                namespace.command,
                cli::ReviewCommand::Run(_)
                    | cli::ReviewCommand::Resume { .. }
                    | cli::ReviewCommand::Continue { execute: true, .. }
                    | cli::ReviewCommand::Decide { execute: true, .. }
            )
        }
        cli::Command::Task { command } => matches!(
            command,
            cli::TaskCommand::Start { execute: true, .. }
                | cli::TaskCommand::Run { detach: false, .. }
        ),
        cli::Command::Scenario { .. } | cli::Command::Ant { .. } => runs_task_work(command),
        _ => false,
    }
}

fn needs_interrupt_handling(command: &cli::Command) -> bool {
    runs_task_work(command)
        || matches!(
            command,
            cli::Command::Ant {
                command: scenario::AntCommand::Output { .. }
            } | cli::Command::Scenario {
                command: scenario::ScenarioCommand::Output { .. }
            }
        )
        || matches!(
            command,
            cli::Command::Provider {
                command: cli::ProviderCommand::Auth {
                    command: cli::ProviderAuthCommand::Begin { .. }
                }
            }
        )
}

fn exit_interrupted(
    argv: &[String],
    prefix: &str,
    signal: nix::sys::signal::Signal,
    error: Option<String>,
) -> ! {
    let code = interrupt::exit_code(signal);
    let task = interrupt::noted_task();
    // The same next command `af task show --json` names: `af task run ID` only while the
    // interrupted node has a recovery left (ADR-0209). Either carries the Store selectors this
    // command was given.
    let (resume, next_action) = match (&task, interrupt::noted_next()) {
        (Some(id), Some(next)) => (
            format!(
                "a node of Task {id} used up its recovery allowance, so `af task run {id}` \
                 cannot continue it; start it again with `{next}` (the Task file it was started \
                 from)"
            ),
            Some(next),
        ),
        (Some(id), None) => {
            let run = task_commands::with_selectors(&format!("af task run {id}"));
            (
                format!(
                    "resume it with `{run}`, which dispatches the interrupted node again without \
                     using one of its Attempts"
                ),
                Some(run),
            )
        }
        (None, _) => (
            "run the same command again to resume it".to_owned(),
            same_command(argv),
        ),
    };
    let login =
        argv.get(1).is_some_and(|s| s == "provider") && argv.get(2).is_some_and(|s| s == "auth");
    let next_action = next_action.filter(|command| error_document::pasteable(command));
    let (message, next_action) = if login {
        (
            format!(
                "interrupted by {signal}; native login stopped. Inspect the non-secret recovery status; a new login requires a fresh private host permission"
            ),
            login_status(argv),
        )
    } else {
        (
            format!(
                "interrupted by {signal}; its Worker processes were stopped and the Task was not finished. To continue, {resume}"
            ),
            next_action,
        )
    };
    if let Some(error) = error {
        if error != interrupt::INTERRUPTED {
            eprintln!("{prefix}: {}", terminal_lines(&error));
        }
        if argv.iter().skip(1).any(|word| word == "--json") {
            // The recovery a script runs is a field, not only prose (`af help json`).
            let mut detail = error_document::Detail::new(error_document::Cause::Interrupted)
                .with("signal", signal.as_str())
                .with("task", task.clone());
            detail.next_action = next_action;
            let document = error_document::document(prefix, &message, code, Some(&detail));
            println!("{document}");
        }
    }
    eprintln!("{prefix}: {}", terminal_lines(&message));
    let _ = std::io::stdout().flush();
    interrupt::exit(signal);
}

fn login_status(argv: &[String]) -> Option<String> {
    use clap::Parser as _;
    let parsed = cli::Af::try_parse_from(argv).ok()?;
    let Some(cli::Command::Provider {
        command:
            cli::ProviderCommand::Auth {
                command:
                    cli::ProviderAuthCommand::Begin {
                        id, kind, auth_dir, ..
                    },
            },
    }) = parsed.command
    else {
        return None;
    };
    let auth_dir = auth_dir.to_str()?;
    let command = format!(
        "af provider auth status {} --kind {} --auth-dir {}",
        shell_words::quote(&id),
        provider_kind(kind),
        shell_words::quote(auth_dir)
    );
    error_document::pasteable(&command).then_some(command)
}

fn same_command(argv: &[String]) -> Option<String> {
    let words = argv.get(1..).unwrap_or_default();
    if words.is_empty() {
        return None;
    }
    Some(format!("af {}", shell_words::join(words)))
        .filter(|command| error_document::pasteable(command))
}

fn provider_command(command: cli::ProviderCommand) -> Result<i32, String> {
    match command {
        cli::ProviderCommand::Detect {
            dirs,
            ids,
            kind,
            all,
            apply,
            json,
            identity,
        } => providers::detect::run(providers::detect::DetectOptions {
            dirs,
            ids,
            kind: kind.map(|kind| provider_kind(kind).to_string()),
            all,
            apply,
            json,
            identity,
        }),
        cli::ProviderCommand::Status {
            json,
            usage,
            verbose,
        } => {
            let usage = if usage {
                providers::UsageProbe::Probe
            } else {
                providers::UsageProbe::Skip
            };
            providers::print_status(json, usage, verbose)
        }
        cli::ProviderCommand::Setup {
            backend,
            sdk_entrypoint,
            id,
            kind,
            auth_dir,
            login,
            json,
        } => {
            let kind = provider_kind(kind);
            if kind == "pi" {
                providers::setup_pi(
                    &id,
                    backend.as_deref(),
                    auth_dir.as_deref(),
                    sdk_entrypoint.as_deref(),
                    login,
                    json,
                )
            } else {
                if backend.is_some() || sdk_entrypoint.is_some() {
                    return Err("--backend and --sdk-entrypoint apply only to Pi".into());
                }
                providers::setup(&id, kind, auth_dir.as_deref(), login, json)
            }
        }
        cli::ProviderCommand::Models {
            ids,
            json,
            resolve,
            page,
        } => providers::models::print_models(&ids, json, resolve.as_deref(), usize::from(page)),
        cli::ProviderCommand::Add {
            id,
            kind,
            auth_dir,
            unverified,
        } => {
            let kind = provider_kind(kind);
            providers::add(&id, kind, auth_dir.as_deref(), unverified)
        }
        cli::ProviderCommand::Remove { ids } => providers::remove(&ids),
        cli::ProviderCommand::Disable { id, duration } => {
            providers::availability::disable(&id, &duration)
        }
        cli::ProviderCommand::Enable { id } => providers::availability::enable(&id),
        cli::ProviderCommand::Auth { command } => match command {
            cli::ProviderAuthCommand::Begin {
                id,
                kind,
                auth_dir,
                host_read_fd,
                host_write_fd,
                timeout_secs,
            } => providers::auth_handoff::begin(
                &id,
                provider_kind(kind),
                &auth_dir,
                host_read_fd,
                host_write_fd,
                timeout_secs,
            ),
            cli::ProviderAuthCommand::Status { id, kind, auth_dir } => {
                providers::auth_handoff::status(&id, provider_kind(kind), &auth_dir)
            }
            cli::ProviderAuthCommand::Cancel {
                id,
                kind,
                auth_dir,
                recovery_id,
            } => providers::auth_handoff::cancel(&id, provider_kind(kind), &auth_dir, &recovery_id),
        },
        cli::ProviderCommand::Recover => providers::recover().map(|()| 0),
        cli::ProviderCommand::Doctor(args) => task_commands::provider_doctor(args),
    }
}

fn provider_kind(kind: cli::ProviderKindArg) -> &'static str {
    match kind {
        cli::ProviderKindArg::Pi => "pi",
        cli::ProviderKindArg::Claude => "claude",
        cli::ProviderKindArg::Codex => "codex",
    }
}

pub(crate) fn terminal_text(value: &str) -> String {
    let mut shown = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() {
            shown.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            shown.push(c);
        }
    }
    shown
}

fn terminal_lines(message: &str) -> String {
    message
        .split('\n')
        .map(terminal_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_text(prefix: &str, error: &str, detail: Option<&error_document::Detail>) -> String {
    detail
        .and_then(|detail| detail.next_action.as_deref())
        .filter(|next| error_document::pasteable(next))
        .zip(human::stderr_columns())
        .and_then(|(next, width)| {
            let sentence = error.strip_suffix(&format!(" then run `{next}` again"))?;
            let head = format!("{prefix}: {}", terminal_lines(sentence));
            Some(human::fit_refusal(&head, next, width))
        })
        .unwrap_or_else(|| format!("{prefix}: {}", terminal_lines(error)))
}

fn sweep_stale_sandboxes_in_background() -> Option<std::thread::JoinHandle<Option<String>>> {
    std::thread::Builder::new()
        .name("af-sandbox-sweep".into())
        .spawn(|| {
            let report = af_sandbox::sweep_stale_sandboxes();
            (report.removed > 0).then(|| {
                format!(
                    "af: removed {} stale sandbox director{} left by exited processes under {}",
                    report.removed,
                    if report.removed == 1 { "y" } else { "ies" },
                    terminal_text(&af_sandbox::sandbox_root().display().to_string())
                )
            })
        })
        .ok()
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{login_status, same_command, terminal_lines, terminal_text};

    /// #331 item 10: the `next_action` of an interrupted command that ran no Task is that same
    /// command, quoted as typed; an interrupted `af provider auth begin` names the status of the
    /// same login. Each parses with the CLI; a word no pasted line carries gives none.
    #[test]
    fn an_interrupted_command_names_the_command_that_continues_it() {
        use clap::Parser as _;
        let argv = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        let again = same_command(&argv(&["/opt/af/bin/af", "review", "run", "--file", "a b"]));
        assert_eq!(again.as_deref(), Some("af review run --file 'a b'"));
        assert_eq!(same_command(&argv(&["af", "review", "run", "x\ny"])), None);
        assert_eq!(same_command(&argv(&["af"])), None);
        let begin = argv(&[
            "af",
            "provider",
            "auth",
            "begin",
            "--kind",
            "claude",
            "--auth-dir",
            "/h/c d",
            "--host-read-fd",
            "3",
            "--host-write-fd",
            "4",
            "claude-main",
        ]);
        let status = login_status(&begin).unwrap();
        assert_eq!(
            status,
            "af provider auth status claude-main --kind claude --auth-dir '/h/c d'"
        );
        crate::cli::Af::try_parse_from(shell_words::split(&status).unwrap()).unwrap();
        assert_eq!(login_status(&argv(&["af", "task", "run", "t"])), None);
    }

    /// A message af composed keeps its own line breaks; every line of it is shown through
    /// `terminal_text`, so a value interpolated into it sends no control character (audit C3).
    #[test]
    fn a_message_keeps_its_lines_and_sends_no_control_character() {
        assert_eq!(
            terminal_lines("opening esc\x1b]0;PWNED\x07repo: gone\n  try: af --help\r"),
            "opening esc\\u{1b}]0;PWNED\\u{7}repo: gone\n  try: af --help\\u{d}"
        );
        assert_eq!(terminal_lines("plain"), "plain");
        assert_eq!(terminal_lines(""), "");
    }

    #[test]
    fn every_control_character_is_shown_not_sent() {
        assert_eq!(
            terminal_text("esc\x1b]0;PWNED\x07name"),
            "esc\\u{1b}]0;PWNED\\u{7}name"
        );
        assert_eq!(
            terminal_text("new\nline\ttab\r"),
            "new\\u{a}line\\u{9}tab\\u{d}"
        );
        assert_eq!(
            terminal_text("del\x7f c1\u{9b}31m"),
            "del\\u{7f} c1\\u{9b}31m"
        );
        // Printable text, Unicode included, is untouched: nothing is dropped or bounded.
        let plain = "src/caf\u{e9}.rs \u{2014} a title with `code` and 100% of it";
        assert_eq!(terminal_text(plain), plain);
        let long = "x".repeat(10_000);
        assert_eq!(terminal_text(&long), long);
        for value in ["esc\x1b[2J", "bell\x07", "a\nb", "c1\u{85}", "\x00"] {
            let shown = terminal_text(value);
            assert!(!shown.chars().any(char::is_control), "{shown:?}");
        }
    }
}
