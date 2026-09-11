//! Export data has no execution authority. Typed ports are the reusable parameters; captured
//! Task values and local effective bindings never become defaults in the shared definition.
use super::*;
use crate::task::shared::SharedTaskCatalog;
use review_core::task::pipeline::{PipelineApplicabilityV1, PipelineContractV1, ValueRefV1};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineContractFixture {
    pub contract: PipelineContractV1,
    pub accepts: PipelineApplicabilityV1,
    pub coverage: BTreeMap<String, ValueRefV1>,
    pub max_attempts: u32,
    pub max_parallel: u32,
}

impl From<&PipelineDefinitionV1> for PipelineContractFixture {
    fn from(pipeline: &PipelineDefinitionV1) -> Self {
        Self {
            contract: pipeline.contract.clone(),
            accepts: pipeline.accepts.clone(),
            coverage: pipeline.coverage.clone(),
            max_attempts: pipeline.max_attempts,
            max_parallel: pipeline.max_parallel,
        }
    }
}

/// Reviewable interface expectations. A contract test checks these against pinned definitions;
/// it does not claim that a Worker has performed business acceptance or run a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogContractFixtures {
    pub schema: String,
    pub pipelines: BTreeMap<String, PipelineContractFixture>,
    pub workers: BTreeMap<String, OperatorSignature>,
    pub kinds: BTreeMap<String, TaskKindManifest>,
}

#[derive(Debug)]
pub struct ExportedCatalog {
    pub root: String,
    pub catalog: SharedTaskCatalog,
    pub contracts: CatalogContractFixtures,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl TaskPlanCompiler {
    fn export_closure(
        &self,
        name: &str,
        depth: usize,
        stack: &mut BTreeSet<String>,
        closure: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if depth >= 4 || stack.contains(name) {
            return Err("Exported Pipeline closure exceeds depth four or contains a cycle".into());
        }
        if name.starts_with("local/") || name.starts_with("af-internal/") {
            return Err(format!(
                "Cannot export {name}: choose a reviewed shared default before exporting"
            ));
        }
        let pipeline = self
            .pipelines
            .get(name)
            .ok_or("Export root or child is not a Pipeline")?;
        // Traverse each route, including previously visited children, to enforce depth.
        stack.insert(name.into());
        closure.insert(name.into());
        for slot in pipeline.slots.values() {
            if ["local/", "generated/", "af-internal/"]
                .iter()
                .any(|p| slot.worker.starts_with(p))
                || !self.workers.contains_key(&slot.worker)
            {
                return Err(format!(
                    "Cannot export {}: slot default {} needs a reviewed shared Worker",
                    pipeline.name, slot.worker
                ));
            }
            closure.insert(slot.worker.clone());
        }
        if closure.len() > 128 {
            return Err("Exported catalog exceeds 128 packages".into());
        }
        for node in &pipeline.nodes {
            match &node.operator {
                TaskOperatorV1::Call { pipeline, .. } => {
                    self.export_closure(pipeline, depth + 1, stack, closure)?;
                }
                TaskOperatorV1::PlanningContext {} => {
                    return Err("The trusted preparation pipeline cannot be exported".into());
                }
                _ => (),
            }
        }
        stack.remove(name);
        Ok(())
    }

    /// Walk the original shared defaults, not a plan's effective local replacement closure.
    /// This method neither mutates the compiler nor installs or approves the exported root.
    pub fn export_catalog(&self, root: &str, name: &str) -> Result<ExportedCatalog, String> {
        if !is_package_name(name)
            || ["local/", "generated/", "af-internal/"]
                .iter()
                .any(|p| name.starts_with(p))
        {
            return Err("Export needs a shared package name outside reserved namespaces".into());
        }
        let mut closure = BTreeSet::new();
        self.export_closure(root, 0, &mut BTreeSet::new(), &mut closure)?;
        if let Some(kind) = &self.active_kind {
            if ["local/", "generated/", "af-internal/"]
                .iter()
                .any(|p| kind.starts_with(p))
            {
                return Err(
                    "Export requires a shared Task-kind default outside reserved namespaces".into(),
                );
            }
            closure.insert(kind.clone());
        }
        let mut names = BTreeMap::new();
        let mut targets = BTreeSet::new();
        for original in &closure {
            let target = if original == root {
                name.to_string()
            } else if let Some(child) = original.strip_prefix("generated/") {
                format!("{name}/{child}")
            } else {
                original.clone()
            };
            if !is_package_name(&target) || !targets.insert(target.clone()) {
                return Err("Export package renaming is ambiguous or exceeds name bounds".into());
            }
            names.insert(original.clone(), target);
        }
        let mut files = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut contracts = CatalogContractFixtures {
            schema: "af.catalog-contract-fixtures/1".into(),
            pipelines: BTreeMap::new(),
            workers: BTreeMap::new(),
            kinds: BTreeMap::new(),
        };
        let mut total = 0usize;
        for original in closure {
            let name = &names[&original];
            let package = &self.packages[&original];
            let content = if let Some(pipeline) = self.pipelines.get(&original) {
                let mut pipeline = pipeline.clone();
                pipeline.name = name.clone();
                for node in &mut pipeline.nodes {
                    if let TaskOperatorV1::Call { pipeline, .. } = &mut node.operator {
                        *pipeline = names[pipeline].clone();
                    }
                }
                pipeline.validate()?;
                contracts.pipelines.insert(name.clone(), (&pipeline).into());
                // Pipeline execution reads only the typed definition. Omit comments and
                // auxiliary package files rather than copying possible generation scratch.
                BTreeMap::from([(
                    "pipeline.toml".into(),
                    toml::to_string(&pipeline)
                        .map_err(|e| e.to_string())?
                        .into_bytes(),
                )])
            } else if let Some(kind) = self.kinds.get(&original) {
                contracts.kinds.insert(name.clone(), kind.clone());
                BTreeMap::from([(
                    "kind.toml".into(),
                    toml::to_string(kind)
                        .map_err(|e| e.to_string())?
                        .into_bytes(),
                )])
            } else {
                let worker = &self.workers[&original];
                contracts
                    .workers
                    .insert(name.clone(), worker.signature.clone());
                package.bytes.files.clone()
            };
            let path = crate::task::shared::package_directory(name);
            packages.insert(
                name.clone(),
                TaskPackagePin {
                    version: package.bytes.version.clone(),
                    digest: package_digest_from_files(&content),
                    path: path.clone(),
                },
            );
            for (suffix, bytes) in content {
                total = total
                    .checked_add(bytes.len())
                    .ok_or("Export size overflow")?;
                if files.len() >= 4096 || total > 64 * 1024 * 1024 {
                    return Err("Export exceeds captured file or byte bounds".into());
                }
                files.insert(format!("{path}/{suffix}"), bytes);
            }
        }
        let catalog = SharedTaskCatalog {
            schema: "af.shared-task-catalog/1".into(),
            packages,
            imports: BTreeSet::new(),
            path_base: crate::task::shared::CatalogPathBase::Manifest,
        };
        catalog.validate()?;
        files.insert(
            "catalog.toml".into(),
            toml::to_string(&catalog)
                .map_err(|e| e.to_string())?
                .into_bytes(),
        );
        files.insert(
            "contracts.json".into(),
            serde_json::to_vec_pretty(&contracts).map_err(|e| e.to_string())?,
        );
        Ok(ExportedCatalog {
            root: name.into(),
            catalog,
            contracts,
            files,
        })
    }

    pub fn check_contract_fixtures(
        &self,
        fixtures: &CatalogContractFixtures,
    ) -> Result<(), String> {
        let expected = CatalogContractFixtures {
            schema: "af.catalog-contract-fixtures/1".into(),
            pipelines: self
                .pipelines
                .iter()
                .map(|(name, p)| (name.clone(), p.into()))
                .collect(),
            workers: self
                .workers
                .iter()
                .map(|(name, w)| (name.clone(), w.signature.clone()))
                .collect(),
            kinds: self.kinds.clone(),
        };
        if *fixtures != expected {
            return Err(
                "Contract fixtures differ from the exact catalog interfaces or coverage".into(),
            );
        }
        Ok(())
    }
}
