//! Developer decisions are detached signatures over the exact plan authorization payload.
//! Signing keys never enter the Task host, its captured catalog, or a Worker context.
use super::*;
use review_pipeline::task::host::TaskDeveloper;
use review_store::store::task::DeveloperGrant;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeveloperPolicy {
    pub schema: String,
    /// Developer label to minisign public key, captured from project authority.
    pub keys: BTreeMap<String, String>,
}
impl DeveloperPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.task-developers/1" || self.keys.is_empty() || self.keys.len() > 32 {
            return Err("Developer authority requires one to 32 captured signing keys".into());
        }
        for (developer, key) in &self.keys {
            if !is_name(developer) || key.len() > 4096 {
                return Err("Invalid developer identity or public key bound".into());
            }
            minisign_verify::PublicKey::decode(key)
                .map_err(|e| format!("Developer {developer} public key: {e}"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthorizationRequest {
    pub schema: String,
    pub task_revision_id: String,
    pub plan_id: String,
    pub policy_id: String,
    pub developer: String,
    pub decision: PlanDecisionKindV1,
    pub reason: String,
    pub valid_until_unix_ms: u64,
}
impl AuthorizationRequest {
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        if self.schema != "af.task-plan-authorization/1"
            || ![&self.task_revision_id, &self.plan_id, &self.policy_id]
                .into_iter()
                .all(|id| review_core::is_digest(id))
            || !is_name(&self.developer)
            || self.reason.trim().is_empty()
            || self.reason.len() > 65536
            || self.valid_until_unix_ms == 0
            || self.valid_until_unix_ms > review_core::json::SAFE_INTEGER_MAX as u64
        {
            return Err("Invalid exact developer authorization payload".into());
        }
        let mut bytes = b"af/task-plan-authorization/1\n".to_vec();
        bytes.extend(
            review_store::canonicalize(&serde_json::to_value(self).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignedAuthorization {
    pub schema: String,
    pub request: AuthorizationRequest,
    pub signature: String,
}

pub(super) struct SignedTaskDeveloper<'a> {
    pub cas: &'a Cas,
    pub policy: &'a DeveloperPolicy,
    /// Only an explicit developer command supplies a new signed request. Runtime hosts only
    /// verify recorded signatures; they have no signing or unattended decision capability.
    pub submitted: Option<&'a SignedAuthorization>,
}
impl SignedTaskDeveloper<'_> {
    fn verify(
        &self,
        signed: &SignedAuthorization,
    ) -> Result<(TaskRevisionV1, ExecutionPlanV1), String> {
        self.policy.validate()?;
        if signed.schema != "af.signed-task-authorization/1" || signed.signature.len() > 8192 {
            return Err("Invalid signed developer authorization".into());
        }
        let request = &signed.request;
        let bytes = request.signing_bytes()?;
        let key = self
            .policy
            .keys
            .get(&request.developer)
            .ok_or("Developer signing key is not trusted by this Task")?;
        let key = minisign_verify::PublicKey::decode(key).map_err(|e| e.to_string())?;
        let signature =
            minisign_verify::Signature::decode(&signed.signature).map_err(|e| e.to_string())?;
        key.verify(&bytes, &signature, false)
            .map_err(|_| "Developer signature does not authorize these exact bytes")?;
        let revision: TaskRevisionV1 =
            artifact(self.cas, &request.task_revision_id, TASK_REVISION_V1)?;
        let plan: ExecutionPlanV1 = artifact(self.cas, &request.plan_id, EXECUTION_PLAN_V1)?;
        revision.validate()?;
        plan.validate()?;
        let authority: RunAuthority = serde_json::from_value(
            self.cas
                .get_json(&revision.authority.policy_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if authority.developers.as_ref() != Some(self.policy) {
            return Err("Developer signing keys differ from this Task's captured authority".into());
        }
        if plan.task_revision_id != request.task_revision_id
            || plan.authority != revision.authority
            || request.policy_id != revision.authority.policy_id
            || plan.limits != revision.limits
            || plan.inputs != revision.inputs
            || request.valid_until_unix_ms > revision.limits.deadline_unix_ms
            || clock()? >= request.valid_until_unix_ms
        {
            return Err(
                "Developer authorization is expired or names another Task, plan, policy or budget"
                    .into(),
            );
        }
        Ok((revision, plan))
    }
}
impl TaskDeveloper for SignedTaskDeveloper<'_> {
    fn decide(
        &self,
        task: &TaskRevisionV1,
        plan_id: &str,
        decision: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        let signed = self
            .submitted
            .ok_or("This process has no submitted developer signature")?;
        let (revision, _) = self.verify(signed)?;
        if &revision != task
            || signed.request.plan_id != plan_id
            || signed.request.decision != decision
        {
            return Err("Developer signature names another Task or decision".into());
        }
        let authorization_id = self
            .cas
            .put_json(&serde_json::to_value(signed).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        Ok(DeveloperGrant {
            developer: signed.request.developer.clone(),
            authorization_id,
            valid_until_unix_ms: signed.request.valid_until_unix_ms,
        })
    }
    fn current(&self, decision: &PlanDecisionV1) -> Result<(), String> {
        let signed: SignedAuthorization = serde_json::from_value(
            self.cas
                .get_json(&decision.authorization_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.verify(&signed)?;
        let request = &signed.request;
        if request.task_revision_id != decision.task_revision_id
            || request.plan_id != decision.plan_id
            || request.policy_id != decision.policy_id
            || request.developer != decision.developer
            || request.decision != decision.decision
            || request.reason != decision.reason
        {
            return Err(
                "Recorded decision differs from its authenticated developer request".into(),
            );
        }
        Ok(())
    }
}

pub(super) fn host<'a>(
    cas: &'a Cas,
    authority: &'a RunAuthority,
    submitted: Option<&'a SignedAuthorization>,
) -> Box<dyn TaskDeveloper + 'a> {
    match &authority.developers {
        Some(policy) => Box::new(SignedTaskDeveloper {
            cas,
            policy,
            submitted,
        }),
        None => Box::new(NoTaskDeveloper),
    }
}

pub(super) struct DecisionAuthority<'a> {
    pub compiler: &'a TaskPlanCompiler,
    pub developer: &'a dyn TaskDeveloper,
}
impl review_store::store::task::TaskAuthority for DecisionAuthority<'_> {
    fn validate_planning_inputs(
        &self,
        cas: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<(), String> {
        self.compiler
            .validate_planning_inputs(cas, previous, next, plan)
    }
    fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        self.compiler.validate_plan(cas, task, plan)
    }
    fn authorize_decision(
        &self,
        task: &TaskRevisionV1,
        plan_id: &str,
        decision: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        self.developer.decide(task, plan_id, decision)
    }
    fn authorization_current(&self, decision: &PlanDecisionV1) -> Result<(), String> {
        self.developer.current(decision)
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("A developer decision command cannot finish a Task".into())
    }
}

fn captured(
    cas: &Cas,
    store: &EventStore,
    id: &str,
    allow_admitted: bool,
) -> Result<(TaskProjection, RunAuthority, TaskPlanCompiler), String> {
    let projection = store
        .task_projection(cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    let authority: RunAuthority = serde_json::from_value(
        cas.get_json(&projection.revision.authority.policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    authority
        .developers
        .as_ref()
        .ok_or("This Task has no configured developer signing keys")?
        .validate()?;
    let mut compiler = planning::restore(cas, &projection, &authority)?;
    let plan: ExecutionPlanV1 = artifact(
        cas,
        projection
            .plan_id
            .as_deref()
            .ok_or("Task has no captured plan")?,
        EXECUTION_PLAN_V1,
    )?;
    let graph: CompiledTask = artifact(cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
    let _ = bind_models(
        cas,
        &mut compiler,
        &authority,
        &projection.revision_id,
        &graph
            .calls
            .get("root")
            .ok_or("Task has no captured root Pipeline")?
            .pipeline,
    )?;
    compiler.validate_plan(cas, &projection.revision, &plan)?;
    if (!allow_admitted && projection.admitted)
        || matches!(projection.phase, TaskPhaseV1::Finished { .. })
    {
        return Err("Task is already admitted or finished".into());
    }
    Ok((projection, authority, compiler))
}

pub(crate) fn payload_file(
    task_id: &str,
    developer: &str,
    decision: PlanDecisionKindV1,
    reason: &str,
    output: &Path,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    use std::io::Write;
    let (_, state) = state_path(&inspect.repo, inspect.state.as_deref())?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let (projection, authority, _) = captured(
        &cas,
        &store,
        task_id,
        decision == PlanDecisionKindV1::Rejected,
    )?;
    if !authority
        .developers
        .as_ref()
        .ok_or("No developer policy")?
        .keys
        .contains_key(developer)
    {
        return Err("Developer signing key is not trusted by this Task".into());
    }
    let request = AuthorizationRequest {
        schema: "af.task-plan-authorization/1".into(),
        task_revision_id: projection.revision_id,
        plan_id: projection.plan_id.ok_or("Task has no plan")?,
        policy_id: projection.revision.authority.policy_id,
        developer: developer.into(),
        decision,
        reason: reason.into(),
        valid_until_unix_ms: projection.revision.limits.deadline_unix_ms,
    };
    if clock()? >= request.valid_until_unix_ms {
        return Err("Task deadline expired".into());
    }
    let bytes = request.signing_bytes()?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| format!("Authorization payload requires an absent output path: {e}"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    if inspect.json {
        println!(
            "{}",
            json!({"schema":"af/task-decision-payload@1","task_id":task_id,"payload":request,"output":output})
        );
    } else {
        println!(
            "Authorization payload written to {}. Sign these exact bytes with the trusted developer key.",
            output.display()
        );
    }
    Ok(0)
}

fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Developer authorization exceeds its input bound".into());
    }
    Ok(bytes)
}

pub(crate) fn apply(
    task_id: &str,
    decision: PlanDecisionKindV1,
    payload: &Path,
    signature: &Path,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    let bytes = bounded_read(payload, 70 * 1024)?;
    let value = bytes
        .strip_prefix(b"af/task-plan-authorization/1\n")
        .ok_or("Authorization payload has the wrong signing domain")?;
    let request: AuthorizationRequest = serde_json::from_slice(value).map_err(|e| e.to_string())?;
    if request.signing_bytes()? != bytes || request.decision != decision {
        return Err("Authorization bytes are not canonical or name another decision".into());
    }
    let signed = SignedAuthorization {
        schema: "af.signed-task-authorization/1".into(),
        request,
        signature: String::from_utf8(bounded_read(signature, 8192)?).map_err(|e| e.to_string())?,
    };
    let (_, state) = state_path(&inspect.repo, inspect.state.as_deref())?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let (projection, authority, compiler) = captured(
        &cas,
        &store,
        task_id,
        decision == PlanDecisionKindV1::Rejected,
    )?;
    if projection.revision_id != signed.request.task_revision_id
        || projection.plan_id.as_ref() != Some(&signed.request.plan_id)
    {
        return Err("Developer signature names another Task or stale plan".into());
    }
    let developer = host(&cas, &authority, Some(&signed));
    // Authenticate before taking a writer lease; malformed signatures cannot pause execution.
    developer.decide(&projection.revision, &signed.request.plan_id, decision)?;
    let trusted = DecisionAuthority {
        compiler: &compiler,
        developer: developer.as_ref(),
    };
    let lease = store
        .take_task_lease(
            &cas,
            task_id,
            &format!("cli-{}", std::process::id()),
            15_000,
        )
        .map_err(|e| e.to_string())?;
    let outcome = if decision == PlanDecisionKindV1::Rejected
        && projection.plan_decision(&signed.request.plan_id) == Some(PlanDecisionKindV1::Approved)
    {
        store.revoke_task_approval(
            &cas,
            &lease,
            &signed.request.plan_id,
            &signed.request.reason,
            &trusted,
        )
    } else {
        store.decide_task_plan(
            &cas,
            &lease,
            &signed.request.plan_id,
            decision,
            &signed.request.reason,
            &trusted,
        )
    }
    .map(|_| ())
    .map_err(|e| e.to_string());
    release(&cas, &mut store, &lease, outcome)?;
    present(&cas, &store, task_id, inspect.json, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema(name: &str) -> jsonschema::Validator {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("schemas").join(name)).unwrap())
                .unwrap();
        jsonschema::validator_for(&value).unwrap()
    }
    #[test]
    fn developer_authorizations_are_closed_versioned_and_domain_separated() {
        let request = AuthorizationRequest {
            schema: "af.task-plan-authorization/1".into(),
            task_revision_id: format!("sha256:{}", "1".repeat(64)),
            plan_id: format!("sha256:{}", "2".repeat(64)),
            policy_id: format!("sha256:{}", "3".repeat(64)),
            developer: "owner".into(),
            decision: PlanDecisionKindV1::Approved,
            reason: "Reviewed the exact graph".into(),
            valid_until_unix_ms: 10000,
        };
        assert!(
            request
                .signing_bytes()
                .unwrap()
                .starts_with(b"af/task-plan-authorization/1\n{")
        );
        let request_schema = schema("task-plan-authorization-v1.json");
        let mut value = serde_json::to_value(&request).unwrap();
        assert!(request_schema.is_valid(&value));
        value["extra"] = json!(true);
        assert!(!request_schema.is_valid(&value));
        assert!(serde_json::from_value::<AuthorizationRequest>(value).is_err());
        for (field, invalid) in [
            ("schema", json!("reviewed-by-worker")),
            ("valid_until_unix_ms", json!(0)),
            ("plan_id", json!("HEAD")),
            ("developer", json!("")),
        ] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = invalid;
            assert!(!request_schema.is_valid(&value));
            assert!(
                serde_json::from_value::<AuthorizationRequest>(value)
                    .unwrap()
                    .signing_bytes()
                    .is_err()
            );
        }
        let signed = SignedAuthorization {
            schema: "af.signed-task-authorization/1".into(),
            request,
            signature: "signature syntax is checked cryptographically".into(),
        };
        let signed_schema = schema("signed-task-authorization-v1.json");
        let mut value = serde_json::to_value(signed).unwrap();
        assert!(signed_schema.is_valid(&value));
        value["request"]["worker_can_approve"] = json!(true);
        assert!(!signed_schema.is_valid(&value));
        assert!(serde_json::from_value::<SignedAuthorization>(value).is_err());
        let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let policy = DeveloperPolicy {
            schema: "af.task-developers/1".into(),
            keys: BTreeMap::from([("owner".into(), key.pk.to_box().unwrap().into_string())]),
        };
        policy.validate().unwrap();
        let policy_schema = schema("task-developers-v1.json");
        let mut value = serde_json::to_value(&policy).unwrap();
        assert!(policy_schema.is_valid(&value));
        value["keys"] = json!({});
        assert!(!policy_schema.is_valid(&value));
        assert!(
            serde_json::from_value::<DeveloperPolicy>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
