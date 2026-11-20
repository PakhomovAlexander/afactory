//! Thin native Task entrypoints. Review has no separate Store, scheduler or allowance.
use crate::{cli, task_commands};

pub(crate) fn run(command: cli::ReviewCommand) -> Result<i32, String> {
    use cli::{ReviewCommand as R, TaskCommand as T};
    let task = match command {
        R::DecisionContext { task_id, inspect } => {
            let (repo, state) = crate::project::resolve_inspect(&inspect)?;
            return task_commands::review_decisions::context(&task_id, &repo, state.as_deref());
        }
        R::Decide {
            task_id,
            from,
            file,
            pipeline,
            authority,
            execute,
            inspect,
        } => {
            let (repo, state) = crate::project::resolve_inspect(&inspect)?;
            return task_commands::review_decisions::run(
                task_commands::review_decisions::Options {
                    task_id,
                    from,
                    file,
                    pipeline,
                    authority,
                    execute,
                    repo,
                    state,
                    json: inspect.json,
                },
            );
        }
        R::Run(args) => return start(args, false),
        R::Plan(args) => {
            crate::model_check::enable_plan_time();
            return start(args, true);
        }
        R::Inspect {
            task_id,
            tree,
            plan,
            inspect,
        } => T::Explain {
            task_id,
            tree,
            plan,
            inspect,
        },
        R::Show {
            task_id,
            checks,
            inspect,
        } => T::Show {
            task_id,
            checks,
            inspect,
        },
        R::Ledger {
            task_id,
            round,
            format,
            inspect,
        } => return task_commands::review_ledger::print(&task_id, round, format, &inspect),
        R::Report { task_id, inspect } => T::Report {
            task_ids: vec![task_id],
            pr_body: false,
            template: None,
            base: None,
            inspect,
        },
        R::Resume {
            task_id,
            confirm_plan,
            execute,
            retry_gate,
            inspect,
        } => T::Run {
            wake_route: None,
            task_id,
            from: None,
            round: None,
            goal: None,
            bindings: None,
            confirm_plan,
            execute,
            retry_gate,
            verbose: false,
            detach: false,
            quiet: false,
            inspect,
        },
        R::Continue {
            task_id,
            from,
            round,
            goal,
            bindings,
            execute,
            inspect,
        } => T::Run {
            wake_route: None,
            task_id,
            from: Some(from),
            round,
            goal,
            bindings,
            confirm_plan: None,
            execute,
            retry_gate: false,
            verbose: false,
            detach: false,
            quiet: false,
            inspect,
        },
        R::Export {
            task_id,
            proposal,
            finding,
            plan,
            allow_stale,
            inspect,
        } => {
            if inspect.json {
                return Err("Proposal export writes a patch; omit --json".into());
            }
            let (repo, state) = crate::project::resolve_inspect(&inspect)?;
            return task_commands::review_export::run(
                &task_id,
                proposal.as_deref(),
                finding.as_deref(),
                allow_stale,
                plan.as_deref(),
                &repo,
                state.as_deref(),
            );
        }
    };
    crate::task_command(task)
}

fn start(args: cli::RunArgs, plan_only: bool) -> Result<i32, String> {
    task_commands::start_review(task_commands::StartOptions {
        review_inputs: None,
        input_json: vec![],
        file: args.task_file,
        bindings: args.bindings,
        source_bindings: None,
        repo: args.repo,
        state: args.state,
        authority: args.authority,
        uncommitted: args.uncommitted,
        json: args.json,
        plan_only,
        timeout_secs: args.timeout_secs,
        optimization_history: None,
        fit: false,
        replace: false,
        review_selectors: Some(task_commands::ReviewSelectors {
            candidate: args.candidate,
            base: args.base,
        }),
    })
}
