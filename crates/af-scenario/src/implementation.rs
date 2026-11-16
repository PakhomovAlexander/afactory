//! Immutable startup authority. This capability never authorizes publication.
use crate::contract::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBinding {
    pub provider: String,
    pub provider_kind: String,
    pub model: String,
    pub effort: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub schema: String,
    pub owner: String,
    pub recipe_id: String,
    pub repo: PathBuf,
    pub task_state: PathBuf,
    pub repository_id: String,
    pub source_commit: String,
    pub source_content_digest: String,
    pub repository: String,
    pub destination_repository_id: u64,
    pub actor_id: u64,
    pub actor_login: String,
    pub branch_prefix: String,
    pub base: String,
    #[serde(deserialize_with = "super::contract::unique")]
    pub grants: BTreeSet<Grant>,
    pub templates: BTreeMap<String, BTreeMap<String, ModelBinding>>,
    pub limits: Limits,
    pub max_repairs: u32,
    pub expires_unix_ms: u64,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/implementation-startup-scope@1",
            "Unsupported implementation startup scope",
        )?;
        name(&self.owner)?;
        for value in [
            &self.repository_id,
            &self.repository,
            &self.actor_login,
            &self.base,
        ] {
            text(value)?;
        }
        require(
            af_contracts::is_digest(&self.recipe_id)
                && af_contracts::is_digest(&self.source_content_digest),
            "Invalid startup identity",
        )?;
        require(
            self.repo.is_absolute()
                && self.repo.components().all(|c| {
                    !matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                }),
            "Startup repository must be absolute and normalized",
        )?;
        require(
            self.source_commit.len() == 40
                && self.source_commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "Startup source must be an exact commit",
        )?;
        require(
            self.destination_repository_id > 0
                && self.actor_id > 0
                && self.expires_unix_ms > 0
                && self.expires_unix_ms <= af_contracts::json::SAFE_INTEGER_MAX as u64,
            "Invalid startup actor or expiry",
        )?;
        require(
            self.branch_prefix.ends_with('/')
                && !self.branch_prefix.starts_with('/')
                && !self.branch_prefix.contains("..")
                && self
                    .branch_prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/-_".contains(&b)),
            "Invalid startup branch namespace",
        )?;
        require(
            self.task_state.is_absolute()
                && !self.task_state.starts_with(&self.repo)
                && self.task_state.components().all(|c| {
                    !matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                }),
            "Startup Task Store must be external and normalized",
        )?;
        self.limits.validate()?;
        require(
            self.max_repairs <= 16
                && !self.templates.is_empty()
                && self.templates.len() <= 256
                && self.grants.len() <= 32,
            "Invalid startup bounds",
        )?;
        for (id, bindings) in &self.templates {
            require(
                af_contracts::is_digest(id) && !bindings.is_empty() && bindings.len() <= 256,
                "Invalid startup template pool",
            )?;
            for binding in bindings.values() {
                require(
                    binding.provider == "codex-personal" && binding.provider_kind == "codex",
                    "Startup requires codex-personal",
                )?;
                text(&binding.model)?;
                text(&binding.effort)?;
            }
        }
        for grant in &self.grants {
            require(
                grant.destination == format!("github:{}", self.repository)
                    && matches!(
                        grant.operation,
                        Operation::PrepareCandidateV1
                            | Operation::PublishPr
                            | Operation::Observe
                            | Operation::CleanupWorkspaceV1
                    ),
                "Startup operation or destination is outside implementation scope",
            )?;
        }
        Ok(())
    }
    pub fn admits(&self, bundle: &Bundle) -> Result<()> {
        self.validate()?;
        require(
            self.recipe_id == hash(bundle)?,
            "Startup recipe differs from the complete effective bundle",
        )?;
        let mut templates = BTreeSet::new();
        let mut grants = BTreeSet::new();

        for definition in bundle.definitions.values() {
            require(
                definition.limits.within(&self.limits),
                "Startup recipe exceeds scope budgets",
            )?;
            grants.extend(definition.authority.grants.iter().cloned());
            require(
                definition.authority.task_plans.is_empty() && definition.admission.is_none(),
                "Startup requires fresh implementation templates without roadmap admission",
            )?;
            for step in &definition.steps {
                match &step.operation {
                    Kind::Task { .. } => {
                        return Err("Startup refuses pre-existing Task execution".into());
                    }
                    Kind::FreshTask { template, .. } => {
                        require(
                            template.state == self.task_state
                                && template.repo == self.repo
                                && template.repository_id == self.repository_id
                                && template.authority_commit == self.source_commit
                                && template.source_content_digest == self.source_content_digest,
                            "Startup template source differs",
                        )?;
                        templates.insert(template.id()?);
                    }
                    _ => {}
                }
            }
        }
        fn count(bundle: &Bundle, name: &str, depth: u32) -> Result<u32> {
            require(depth <= 16, "Startup recipe recursion refused")?;
            let definition = bundle
                .definitions
                .get(name)
                .ok_or("Startup child missing")?;
            definition.steps.iter().try_fold(0_u32, |total, step| {
                let n = match &step.operation {
                    Kind::FreshTask { .. } => 1,
                    Kind::Scenario { scenario } => count(bundle, scenario, depth + 1)?,
                    _ => 0,
                };
                total
                    .checked_add(
                        n.checked_mul(u32::from(step.max_attempts))
                            .ok_or("Startup repair count overflow")?,
                    )
                    .ok_or_else(|| "Startup repair count overflow".into())
            })
        }
        let tasks = count(bundle, &bundle.root, 0)?;
        require(
            tasks > 0 && tasks <= self.limits.max_dispatches,
            "Startup recipe exceeds total Task dispatch bound",
        )?;
        require(
            templates == self.templates.keys().cloned().collect() && grants == self.grants,
            "Startup template or operation pool differs",
        )
    }
    pub fn live(&self, host: &dyn crate::Host) -> Result<()> {
        require(
            host.authorization_time()? < self.expires_unix_ms,
            "Implementation startup scope expired",
        )
    }
}

/// Complete preflight is separate from replay: inspection validates retained identities without
/// invoking credentials; live admission and every drive recheck native and machine prerequisites.
pub fn preflight(host: &dyn crate::Host, pin: &Pin) -> Result<()> {
    let Some(scope) = &pin.policy.implementation_startup else {
        return Ok(());
    };
    scope.admits(&pin.bundle)?;
    scope.live(host)?;
    for definition in pin.bundle.definitions.values() {
        for step in &definition.steps {
            if let Kind::FreshTask { template, .. } = &step.operation {
                host.preflight_implementation_template(
                    template,
                    &scope.templates[&template.id()?],
                )?;
            }
        }
    }
    for adapter in scope
        .grants
        .iter()
        .map(|g| &g.adapter)
        .collect::<BTreeSet<_>>()
    {
        let installed = pin
            .policy
            .adapters
            .get(adapter)
            .ok_or("Startup adapter missing")?;
        host.preflight_implementation_adapter(installed, scope)?;
    }
    scope.live(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Host,
        state::Run,
        store::Store,
        tests::{DAY, START, TestHost, valid},
    };
    use std::sync::{Arc, atomic::Ordering};

    fn fixture() -> (Pin, String) {
        let (_, mut pin) = crate::admission_tests::fixture();
        let root = pin.bundle.definitions.get_mut("main").unwrap();
        root.admission = None;
        let mut repair = root.steps[0].clone();
        repair.id = "repair".into();
        // A declared conditional branch must be checked even when it will be skipped.
        repair.when = Some(Binding::Literal {
            value: Box::new(Datum::Boolean(false)),
        });
        let Kind::FreshTask { template, .. } = &mut repair.operation else {
            panic!()
        };
        template.document["goal"] = "repair".into();
        let repair_id = template.id().unwrap();
        root.authority.task_templates.insert(repair_id.clone());
        pin.policy.task_templates.insert(repair_id.clone());
        root.steps.push(repair);
        let Kind::FreshTask { template, .. } = &root.steps[0].operation else {
            panic!()
        };
        let template = template.clone();
        let limits = root.limits.clone();
        let scope = Scope {
            schema: "af/implementation-startup-scope@1".into(),
            owner: "owner".into(),
            recipe_id: hash(&pin.bundle).unwrap(),
            repo: template.repo.clone(),
            task_state: template.state.clone(),
            repository_id: template.repository_id.clone(),
            source_commit: template.authority_commit.clone(),
            source_content_digest: template.source_content_digest.clone(),
            repository: "owner/project".into(),
            destination_repository_id: 1,
            actor_id: 2,
            actor_login: "owner".into(),
            branch_prefix: "implementation/".into(),
            base: "main".into(),
            grants: BTreeSet::new(),
            templates: pin
                .policy
                .task_templates
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        BTreeMap::from([(
                            "fixture/worker".into(),
                            ModelBinding {
                                provider: "codex-personal".into(),
                                provider_kind: "codex".into(),
                                model: "gpt-6.1-sol".into(),
                                effort: "medium".into(),
                            },
                        )]),
                    )
                })
                .collect(),
            limits,
            max_repairs: 1,
            expires_unix_ms: START + DAY,
        };
        pin.policy.implementation_startup = Some(scope);
        (
            Pin::new(&TestHost::default(), pin.bundle, pin.policy).unwrap(),
            repair_id,
        )
    }
    #[test]
    fn complete_conditional_pool_is_preflighted_before_any_task_planning() {
        let (pin, repair) = fixture();
        valid(
            "implementation-startup-scope-v1.json",
            pin.policy.implementation_startup.as_ref().unwrap(),
        );
        valid(
            "implementation-startup-preflight-v1.json",
            &serde_json::json!({"schema":"af/implementation-startup-preflight@1", "scope_id":hash(pin.policy.implementation_startup.as_ref().unwrap()).unwrap(), "scope":pin.policy.implementation_startup}),
        );
        valid(
            "implementation-startup-ready-v1.json",
            &serde_json::json!({"schema":"af/implementation-startup-ready@1", "scope_id":hash(pin.policy.implementation_startup.as_ref().unwrap()).unwrap()}),
        );
        valid("scenario-pin-v8.json", &pin);
        let host = TestHost {
            startup_reject: Some(repair.clone()),
            ..Default::default()
        };
        let error = Run::new(
            &host,
            "refused".into(),
            pin.clone(),
            BTreeMap::new(),
            pin.bundle.definitions["main"].limits.clone(),
            START,
            None,
        )
        .unwrap_err();
        assert_eq!(error, "native startup refused");
        assert!(host.startup_templates.lock().unwrap().contains(&repair));
        assert!(host.planned_tasks.is_none());
        let run = Run::new(
            &TestHost::default(),
            "admitted".into(),
            pin.clone(),
            BTreeMap::new(),
            pin.bundle.definitions["main"].limits.clone(),
            START,
            None,
        )
        .unwrap();
        valid("scenario-run-v8.json", &run);
        assert_eq!(
            run.pin.policy.implementation_startup,
            pin.policy.implementation_startup
        );
    }
    #[test]
    fn expiry_during_real_task_planning_refuses_the_paid_dispatch() {
        let (pin, _) = fixture();
        let host = Arc::new(TestHost {
            planned_tasks: Some(Default::default()),
            expire_startup_on_plan: true,
            ..Default::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(&dir.path().join("state")).unwrap();
        store.host = host.clone();
        let run = Run::new(
            host.as_ref(),
            "planning-expiry".into(),
            pin.clone(),
            BTreeMap::new(),
            pin.bundle.definitions["main"].limits.clone(),
            START,
            None,
        )
        .unwrap();
        store.update(|world| world.admit_run(run)).unwrap();
        assert!(
            crate::runtime::resume(&store, "planning-expiry", Some(START))
                .unwrap_err()
                .contains("expired")
        );
        assert_eq!(
            host.planned_tasks.as_ref().unwrap().lock().unwrap().len(),
            1
        );
        assert_eq!(host.native_dispatches.load(Ordering::Relaxed), 0);
        assert_eq!(
            store.read().unwrap().runs["planning-expiry"]
                .fresh_tasks
                .len(),
            1
        );
    }

    #[test]
    fn scoped_creation_and_elapsed_budget_use_live_time_instead_of_inspection_time() {
        let (mut pin, _) = fixture();
        pin.policy
            .implementation_startup
            .as_mut()
            .unwrap()
            .expires_unix_ms = START + 20 * DAY;
        let host = Arc::new(TestHost::default());
        let pin = Pin::new(host.as_ref(), pin.bundle, pin.policy).unwrap();
        let run = Run::new(
            host.as_ref(),
            "wall".into(),
            pin.clone(),
            BTreeMap::new(),
            pin.bundle.definitions["main"].limits.clone(),
            START + DAY,
            None,
        )
        .unwrap();
        assert_eq!(run.created_unix_ms, START);
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(&dir.path().join("state")).unwrap();
        store.host = host.clone();
        store.update(|world| world.admit_run(run)).unwrap();
        host.startup_time.store(START + 10 * DAY, Ordering::Relaxed);
        let stopped = crate::runtime::resume(&store, "wall", Some(START)).unwrap();
        assert_eq!(stopped.status, crate::state::Status::Failed);
        assert!(stopped.failure.unwrap().contains("elapsed-time allowance"));
        assert_eq!(host.native_dispatches.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_repair_reservations_exclude_verifiers_and_reports_and_survive_restart() {
        // Coordinator settlement fixtures only; native command-backed lifecycle evidence
        // lives in af::it. Nonzero charges here exercise cumulative reservation arithmetic.
        let (mut pin, _) = fixture();
        let root = pin.bundle.definitions.get_mut("main").unwrap();
        let first = root.steps[0].clone();
        root.steps.clear();
        root.authority.task_templates.clear();
        pin.policy.task_templates.clear();
        let models = pin
            .policy
            .implementation_startup
            .as_ref()
            .unwrap()
            .templates
            .values()
            .next()
            .unwrap()
            .clone();
        let mut templates = BTreeMap::new();
        for (id, kind) in [
            ("initial", "implement"),
            ("verify", "verification"),
            ("report", "report"),
            ("repair", "implement"),
            ("over-limit", "implement"),
        ] {
            let mut step = first.clone();
            step.id = id.into();
            step.when = None;
            let Kind::FreshTask { template, .. } = &mut step.operation else {
                panic!()
            };
            template.document["kind"] = kind.into();
            template.document["goal"] = id.into();
            let template_id = template.id().unwrap();
            root.authority.task_templates.insert(template_id.clone());
            pin.policy.task_templates.insert(template_id.clone());
            templates.insert(template_id, models.clone());
            root.steps.push(step);
        }
        let scope = pin.policy.implementation_startup.as_mut().unwrap();
        scope.templates = templates;
        scope.recipe_id = hash(&pin.bundle).unwrap();
        let host = Arc::new(TestHost {
            planned_tasks: Some(Default::default()),
            settle_all_planned: true,
            ..Default::default()
        });
        let pin = Pin::new(host.as_ref(), pin.bundle, pin.policy).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state");
        let mut store = Store::isolated(&path).unwrap();
        store.host = host.clone();
        let run = Run::new(
            host.as_ref(),
            "repair-limit".into(),
            pin.clone(),
            BTreeMap::new(),
            pin.bundle.definitions["main"].limits.clone(),
            START,
            None,
        )
        .unwrap();
        store.update(|world| world.admit_run(run)).unwrap();
        let stopped = crate::runtime::resume(&store, "repair-limit", Some(START)).unwrap();
        assert_eq!(stopped.status, crate::state::Status::Failed);
        assert!(
            stopped
                .failure
                .as_deref()
                .unwrap()
                .contains("repair allowance")
        );
        assert_eq!(stopped.fresh_tasks.len(), 4);
        assert_eq!(stopped.task_charges.len(), 4);
        assert_eq!(stopped.tokens().unwrap(), 68);
        assert!(
            stopped
                .task_charges
                .values()
                .all(|c| c.charged == Some(17) && !c.unknown_usage)
        );
        assert_eq!(host.native_dispatches.load(Ordering::Relaxed), 4);
        drop(store);
        let mut reopened = Store::isolated(&path).unwrap();
        reopened.host = host.clone();
        let resumed = crate::runtime::resume(&reopened, "repair-limit", Some(START)).unwrap();
        assert_eq!(resumed, stopped);
        assert_eq!(host.native_dispatches.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn missing_machine_preflight_support_refuses_before_task_planning() {
        let (mut pin, _) = fixture();
        let grant = Grant {
            adapter: "workspace".into(),
            operation: Operation::Observe,
            destination: "github:owner/project".into(),
        };
        pin.bundle
            .definitions
            .get_mut("main")
            .unwrap()
            .authority
            .grants
            .insert(grant.clone());
        pin.policy.adapters.insert(
            "workspace".into(),
            Adapter {
                executable: "/installed/workspace".into(),
                digest: digest(b"adapter"),
                operations: [Operation::Observe].into(),
                destinations: [grant.destination.clone()].into(),
                max_wall_ms: 1000,
                max_output_bytes: 4096,
            },
        );
        let scope = pin.policy.implementation_startup.as_mut().unwrap();
        scope.grants.insert(grant);
        scope.recipe_id = hash(&pin.bundle).unwrap();
        let host = TestHost::default();
        let pin = Pin::new(&host, pin.bundle, pin.policy).unwrap();
        let error = Run::new(
            &host,
            "missing-prerequisite".into(),
            pin.clone(),
            BTreeMap::new(),
            pin.bundle.definitions["main"].limits.clone(),
            START,
            None,
        )
        .unwrap_err();
        assert!(error.contains("adapter preflight"));
        assert_eq!(host.native_dispatches.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn recipe_pool_budget_source_and_old_versions_fail_closed() {
        let (pin, _) = fixture();
        for case in 0..7 {
            let mut scope = pin.policy.implementation_startup.clone().unwrap();
            match case {
                0 => scope.recipe_id = digest(b"other"),
                1 => scope.templates.clear(),
                2 => scope.limits.max_dispatches = 1,
                3 => scope.source_commit = "b".repeat(40),
                4 => scope
                    .grants
                    .insert(Grant {
                        adapter: "other".into(),
                        operation: Operation::Followup,
                        destination: "github:owner/project".into(),
                    })
                    .then_some(())
                    .unwrap(),
                5 => scope.limits.max_tokens = 1,
                _ => {
                    scope
                        .templates
                        .values_mut()
                        .next()
                        .unwrap()
                        .values_mut()
                        .next()
                        .unwrap()
                        .provider = "other".into()
                }
            }
            assert!(scope.admits(&pin.bundle).is_err(), "case {case}");
        }
        let mut old = pin.clone();
        old.policy.schema = "af/scenario-policy@5".into();
        assert!(old.validate(&TestHost::default()).is_err());
        for retired in ["af/scenario-pin@6", "af/scenario-pin@7"] {
            old = pin.clone();
            old.schema = retired.into();
            assert!(old.validate(&TestHost::default()).is_err());
        }
        let mut changed = pin;
        changed.bundle.definitions.get_mut("main").unwrap().steps[1].when = None;
        assert!(changed.validate(&TestHost::default()).is_err());
    }
    #[test]
    fn replay_remains_inspectable_but_live_expiry_ignores_inspection_clock_and_cancellation_stops()
    {
        let (pin, _) = fixture();
        let host = Arc::new(TestHost::default());
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::isolated(&root.path().join("state")).unwrap();
        store.host = host.clone();
        let run = Run::new(
            host.as_ref(),
            "expired".into(),
            pin.clone(),
            BTreeMap::new(),
            pin.bundle.definitions["main"].limits.clone(),
            START,
            None,
        )
        .unwrap();
        store.update(|world| world.admit_run(run)).unwrap();
        host.startup_time.store(START + DAY, Ordering::Relaxed);
        assert!(store.read().is_ok());
        let mut old = store.read().unwrap();
        for retired in ["af/scenario-store@10", "af/scenario-store@11"] {
            old.schema = retired.into();
            assert!(old.validate(host.as_ref()).is_err());
        }
        let mut old_run = old.runs["expired"].clone();
        for retired in ["af/scenario-run@6", "af/scenario-run@7"] {
            old_run.schema = retired.into();
            assert!(old_run.validate(host.as_ref()).is_err());
        }
        assert!(
            crate::runtime::resume(&store, "expired", Some(START))
                .unwrap_err()
                .contains("expired")
        );
        assert!(store.read().unwrap().runs["expired"].fresh_tasks.is_empty());
        crate::runtime::cancel(&store, "expired", "owner cancelled".into(), START).unwrap();
        host.startup_time.store(START, Ordering::Relaxed);
        let calls = host.startup_templates.lock().unwrap().len();
        let run = crate::runtime::resume(&store, "expired", Some(START)).unwrap();
        assert_eq!(run.status, crate::state::Status::Cancelled);
        assert_eq!(host.startup_templates.lock().unwrap().len(), calls);
        assert!(run.fresh_tasks.is_empty());
        assert_eq!(host.authorization_time().unwrap(), START);
    }
}
