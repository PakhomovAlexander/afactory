//! Token-free, additive installation of a pinned Task catalog.

use super::*;
use review_config::lock::package_digest_from_files;

const DEFAULTS: &str = "\n[task.defaults]\npipeline = \"onboard/implementation-reviewed\"\nfallback = \"refuse\"\n\n[task.defaults.limits]\ntokens = 6000000\nmax_attempts = 16\nwall_ms = 22000000\n\n[task.defaults.limits.verification]\ntokens = 2000000\nattempts = 8\nwall_ms = 10800000\n";
const IMPLEMENTER: &str = "# Implementer\n\nImplement the Task goal in the private source sandbox. Read the declared requirements and source. Make the smallest complete change, run the project's checks, and report the changed behavior and tests. Never publish or change authority outside the sandbox.\n";
const BUGS: &str = "# Bugs reviewer\n\nReview the exact selected change for concrete defects that can cause a wrong result. Trace the changed behavior through callers and tests. Report each reproducible finding with a bounded fix; otherwise return a clean result.\n";
const CORRECTNESS: &str = "# Correctness reviewer\n\nReview the exact selected change against its requirements and the repository's contracts. Check state transitions, errors, budgets and sandbox boundaries. Report only concrete defects with evidence and a fix.\n";
const EVALUATOR: &str = "# Goal evaluator\n\nJudge the Task's goal against the exact source Snapshot and check receipts. Verify each acceptance item independently. Cite evidence for a pass and name unmet items; do not infer a pass from the implementer's report.\n";
const TASK_GUIDE: &str = "\n## Model-driven Tasks\n\nThis bundle also pins `onboard/implementation-reviewed` and its implementer, bugs, correctness\nand evaluator Workers in `.af/task-catalog.toml`. Its required checks live in\n`.af/code-policy.toml`; `[task.defaults]` in `.af/af.toml` selects the Pipeline and limits.\nReview and commit all of `.af/` before planning a three-field Task file with\n`af task plan --file ticket.json`. Register the selected Provider kind with\n`af provider detect` or `af provider setup`, then use machine-local `--bindings` if its\nID differs from the catalog alias.\n";

#[derive(Serialize)]
struct TaskReport {
    schema: &'static str,
    status: &'static str,
    repository: String,
    files: Vec<String>,
    append_text: &'static str,
    gate: Vec<Gate>,
    provider_kind: String,
    warnings: Vec<String>,
    next_steps: Vec<String>,
}

pub(super) fn run(options: &Options) -> Result<(), String> {
    let repo = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    if !repo.join(".git").exists() {
        return Err(format!(
            "{} is not a Git repository root; choose a checkout with --repo",
            repo.display()
        ));
    }
    let authority = repo.join(".af");
    let existing =
        match std::fs::symlink_metadata(&authority) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => true,
            Ok(_) => return Err(
                "refusing .af/: it is not a regular authority directory; choose another repository"
                    .into(),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(format!("inspecting .af/: {error}")),
        };
    let gates = if options.gates.is_empty() {
        discover_gates(&repo)?
    } else {
        options.gates.clone()
    };
    if gates.is_empty() {
        return Err("no unambiguous acceptance Gate found; supply a trusted literal such as --gate 'check=make check'".into());
    }
    let registry = crate::providers::read_registry_entries();
    let profile = options.runner.unwrap_or_else(|| {
        if !registry.ids_of("codex").is_empty() && !registry.ids_of("claude").is_empty() {
            RunnerProfile::Mixed
        } else if !registry.ids_of("codex").is_empty() {
            RunnerProfile::Codex
        } else if !registry.ids_of("claude").is_empty() {
            RunnerProfile::Claude
        } else {
            RunnerProfile::Codex
        }
    });
    let kind = match profile {
        RunnerProfile::Claude => "claude",
        RunnerProfile::Codex => "codex",
        RunnerProfile::Mixed => "mixed",
    };
    let task_files = files(&gates, profile)?;
    let mut command = format!(
        "af onboard --repo {} --tasks --runner {}",
        shell_words::quote(repo.to_str().ok_or("repository path is not UTF-8")?),
        profile.name()
    );
    for gate in &gates {
        command.push_str(" --gate ");
        command.push_str(&shell_words::quote(&format!(
            "{}={}",
            gate.name,
            gate.command_line()
        )));
    }
    if let Some(version) = options.af.as_deref() {
        command.push_str(" --af ");
        command.push_str(&shell_words::quote(version));
    }
    command.push_str(" --apply");
    let mut report = TaskReport {
        schema: "af/onboard-tasks@1",
        status: "preview",
        repository: repo.display().to_string(),
        files: Vec::new(),
        append_text: DEFAULTS,
        gate: gates.clone(),
        provider_kind: kind.into(),
        warnings: Vec::new(),
        next_steps: vec![
            format!("Review the preview, then run {command}"),
            "Stage and review the generated authority: git add .af && git diff --cached -- .af".into(),
            "After review, commit it: git commit -m 'af Task authority'".into(),
            "Register a Provider if needed: af provider detect. Use --bindings to remap the catalog's Provider aliases on this machine.".into(),
            "After committing, plan a Task: af task plan --file ticket.json".into(),
        ],
    };
    if existing {
        let original = preflight(&repo, &task_files)?;
        report.files = task_files.keys().cloned().collect();
        report.files.push(".af/af.toml".into());
        if options.apply {
            apply_additive(&repo, &task_files, &original)?;
            report.status = "created";
        }
    } else {
        let review_profile = options.runner.unwrap_or(profile);
        let mut bundle = build_bundle(&repo, review_profile, gates, options.af.as_deref())?;
        validate_bundle(&bundle)?;
        report.warnings = bundle.report.warnings.clone();
        for (path, bytes) in task_files {
            bundle.files.insert(path, bytes);
        }
        bundle
            .files
            .get_mut(".af/af.toml")
            .ok_or("generated review bundle omitted af.toml")?
            .extend_from_slice(DEFAULTS.as_bytes());
        bundle
            .files
            .get_mut(".af/README.md")
            .ok_or("generated review bundle omitted README.md")?
            .extend_from_slice(TASK_GUIDE.as_bytes());
        report.files = bundle.files.keys().cloned().collect();
        if options.apply {
            apply_bundle(&repo, &bundle)?;
            report.status = "created";
        }
    }
    if options.apply {
        report.next_steps.remove(0);
    }
    for needed in match profile {
        RunnerProfile::Mixed => &["claude", "codex"][..],
        RunnerProfile::Claude => &["claude"][..],
        RunnerProfile::Codex => &["codex"][..],
    } {
        if registry.ids_of(needed).is_empty() {
            report.next_steps.insert(0, format!("No {needed} Provider is registered; run af provider detect or af provider setup {needed}-main --kind {needed} --login"));
        }
    }
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    } else {
        println!("af onboard --tasks: {}", report.status);
        println!("repository  {}", report.repository);
        println!(
            "provider    {} (distinct models for independent Workers)",
            report.provider_kind
        );
        for warning in &report.warnings {
            println!("warning     {warning}");
        }
        println!("files");
        for path in &report.files {
            println!("  {path}");
        }
        println!("append to .af/af.toml exactly:");
        print!("{}", report.append_text);
        println!("next");
        for step in &report.next_steps {
            println!("  {step}");
        }
    }
    Ok(())
}

fn files(gates: &[Gate], profile: RunnerProfile) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let names = ["implementer", "bugs", "correctness", "evaluator"];
    let manifests = [
        include_str!("templates/implementer/worker.toml"),
        include_str!("templates/bugs/worker.toml"),
        include_str!("templates/correctness/worker.toml"),
        include_str!("templates/evaluator/worker.toml"),
    ];
    let inputs = [
        include_bytes!("templates/implementer/input.schema.json").as_slice(),
        include_bytes!("templates/bugs/input.schema.json").as_slice(),
        include_bytes!("templates/correctness/input.schema.json").as_slice(),
        include_bytes!("templates/evaluator/input.schema.json").as_slice(),
    ];
    let outputs = [
        include_bytes!("templates/implementer/outputs/report.schema.json").as_slice(),
        include_bytes!("templates/bugs/outputs/result.schema.json").as_slice(),
        include_bytes!("templates/correctness/outputs/result.schema.json").as_slice(),
        include_bytes!("templates/evaluator/outputs/result.schema.json").as_slice(),
    ];
    let prompts = [IMPLEMENTER, BUGS, CORRECTNESS, EVALUATOR];
    let mut catalog = String::from(
        "schema = \"af.task-catalog/2\"\ncode_policy = \".af/code-policy.toml\"\nno_match = \"refuse\"\n\n[review]\ngeneration = 2\ngate = \"major\"\nclean_rounds = 1\nmax_rounds = 2\nallow_targeted_repairs = false\n\n[review.reviewers]\nbugs = \"required\"\ncorrectness = \"required\"\n\n[independence]\ncommand_workers_by_package = true\ndistinct_principals = false\ndistinct_providers = false\ndistinct_models = true\n\n[provider_admission]\ntokens = 32768\nwall_ms = 45000\n",
    );
    let mut provider_lines = String::from("\n[providers]\n");
    for (index, name) in names.iter().enumerate() {
        let kind = match profile {
            RunnerProfile::Claude => "claude",
            RunnerProfile::Codex => "codex",
            RunnerProfile::Mixed if index == 0 => "claude",
            RunnerProfile::Mixed => "codex",
        };
        let model = if index == 0 {
            if kind == "codex" {
                "gpt-6-sol"
            } else {
                "claude-opus-5-5"
            }
        } else if kind == "codex" {
            "gpt-6-astra"
        } else {
            "claude-sonnet-5-5"
        };
        let manifest = manifests[index]
            .replace("{{PROVIDER_KIND}}", kind)
            .replace("{{MODEL}}", model);
        let root = format!(".af/task-packages/onboard/{name}");
        files.insert(format!("{root}/worker.toml"), manifest.into_bytes());
        files.insert(
            format!("{root}/instructions.md"),
            prompts[index].as_bytes().to_vec(),
        );
        files.insert(format!("{root}/input.schema.json"), inputs[index].to_vec());
        files.insert(
            format!(
                "{root}/outputs/{}.schema.json",
                if index == 0 { "report" } else { "result" }
            ),
            outputs[index].to_vec(),
        );
        provider_lines.push_str(&format!("\"onboard/{name}\" = \"{kind}-main\"\n"));
    }
    let checks = gates
        .iter()
        .map(|gate| format!("\"{}\"", gate.name))
        .collect::<Vec<_>>()
        .join(", ");
    for (name, source) in [
        (
            "review-code",
            include_str!("templates/review-code/pipeline.toml"),
        ),
        (
            "implementation-reviewed",
            include_str!("templates/implementation-reviewed/pipeline.toml"),
        ),
    ] {
        let pipeline = source.replace("checks = [\"{{GATES}}\"]", &format!("checks = [{checks}]"));
        files.insert(
            format!(".af/task-packages/onboard/{name}/pipeline.toml"),
            pipeline.into_bytes(),
        );
    }
    for name in names
        .into_iter()
        .chain(["review-code", "implementation-reviewed"])
    {
        let prefix = format!(".af/task-packages/onboard/{name}/");
        let package: BTreeMap<String, Vec<u8>> = files
            .iter()
            .filter_map(|(path, bytes)| {
                path.strip_prefix(&prefix)
                    .map(|relative| (relative.to_owned(), bytes.clone()))
            })
            .collect();
        let manifest = if name == "review-code" || name == "implementation-reviewed" {
            "pipeline.toml"
        } else {
            "worker.toml"
        };
        let version = toml::from_str::<toml::Value>(
            std::str::from_utf8(&package[manifest]).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or("generated package has no version")?
        .to_owned();
        catalog.push_str(&format!("\n[packages.\"onboard/{name}\"]\nversion = \"{version}\"\ndigest = \"{}\"\npath = \".af/task-packages/onboard/{name}\"\n", package_digest_from_files(&package)));
    }
    catalog.push_str(&provider_lines);
    files.insert(".af/task-catalog.toml".into(), catalog.into_bytes());
    files.insert(
        ".af/code-policy.toml".into(),
        code_policy(gates).into_bytes(),
    );
    Ok(files)
}

fn code_policy(gates: &[Gate]) -> String {
    let mut text = String::from(
        "schema = \"af.code-task-policy/1\"\ncheck_wall_ms = 5400000\nrequire_container = false\n",
    );
    for gate in gates {
        let key = toml::Value::String(gate.name.clone());
        text.push_str(&format!("\n[checks.{key}]\nname = {key}\nrequired = true\n\n[checks.{key}.command]\nprogram = {program}\n", program = toml::Value::String(gate.program.clone())));
        for arg in &gate.args {
            text.push_str(&format!(
                "\n[[checks.{key}.command.args]]\nvalue = {}\nprovenance = \"literal\"\n",
                toml::Value::String(arg.clone())
            ));
        }
    }
    text
}

/// Check the whole write set before staging anything under authority. Existing project bytes
/// are input, never a source of Task defaults or a place to merge a partial Task declaration.
fn preflight(repo: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
    let authority = repo.join(".af");
    let project = authority.join("af.toml");
    let meta = std::fs::symlink_metadata(&project).map_err(|error| {
        format!(
            "inspecting .af/af.toml: {error}; restore review authority before af onboard --tasks"
        )
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(
            ".af/af.toml is not a regular file; restore review authority before af onboard --tasks"
                .into(),
        );
    }
    let original =
        std::fs::read(&project).map_err(|error| format!("reading .af/af.toml: {error}"))?;
    if original.len() > MAX_DISCOVERY_FILE_BYTES as usize {
        return Err(".af/af.toml is too large to append Task defaults; inspect it before af onboard --tasks".into());
    }
    let value: toml::Value = toml::from_str(
        std::str::from_utf8(&original)
            .map_err(|error| format!(".af/af.toml is not UTF-8: {error}"))?,
    )
    .map_err(|error| format!("parsing .af/af.toml: {error}"))?;
    if value.get("task").is_some() {
        return Err(".af/af.toml already declares [task]; af onboard --tasks refuses to merge Task settings. Review that table by hand".into());
    }
    let package_root = authority.join("task-packages");
    match std::fs::symlink_metadata(&package_root) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(
                ".af/task-packages is not a regular directory; repair it before af onboard --tasks"
                    .into(),
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspecting .af/task-packages: {error}")),
    }
    for path in files.keys() {
        match std::fs::symlink_metadata(repo.join(path)) {
            Ok(_) => {
                return Err(format!(
                    "{path} already exists; af onboard --tasks adds only absent files. Review it before retrying"
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspecting {path}: {error}")),
        }
    }
    let package = package_root.join("onboard");
    match std::fs::symlink_metadata(&package) {
        Ok(_) => return Err(".af/task-packages/onboard already exists; af onboard --tasks adds only absent packages".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspecting .af/task-packages/onboard: {error}")),
    }
    Ok(original)
}

fn apply_additive(
    repo: &Path,
    files: &BTreeMap<String, Vec<u8>>,
    original: &[u8],
) -> Result<(), String> {
    let authority = repo.join(".af");
    let now = preflight(repo, files)?;
    if now != original {
        return Err(".af/af.toml changed during onboarding; rerun af onboard --tasks".into());
    }
    let stage = tempfile::Builder::new()
        .prefix(".af-onboard-tasks-")
        .tempdir_in(repo)
        .map_err(|error| format!("staging Task authority: {error}"))?;
    let staged = stage.path().join("authority");
    let authority_meta = std::fs::symlink_metadata(&authority)
        .map_err(|error| format!("rechecking .af/: {error}"))?;
    if authority_meta.file_type().is_symlink() || !authority_meta.is_dir() {
        return Err(".af/ changed during onboarding; rerun af onboard --tasks".into());
    }
    // Existing regular files keep their exact inode, bytes, mode and ACL through hard links.
    // Opened directory descriptors and no-follow child opens keep staging inside authority.
    // The one changed file is written separately. No published path changes before exchange.
    stage_authority(&authority, &staged)?;
    let mut updated = original.to_vec();
    updated.extend_from_slice(DEFAULTS.as_bytes());
    let staged_project = staged.join("af.toml");
    let project_meta = std::fs::symlink_metadata(&staged_project)
        .map_err(|error| format!("inspecting staged .af/af.toml: {error}"))?;
    std::fs::remove_file(&staged_project)
        .map_err(|error| format!("preparing staged .af/af.toml: {error}"))?;
    let mut project = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged_project)
        .map_err(|error| format!("creating staged .af/af.toml: {error}"))?;
    project
        .set_permissions(project_meta.permissions())
        .map_err(|error| format!("preserving .af/af.toml permissions: {error}"))?;
    project
        .write_all(&updated)
        .and_then(|()| project.sync_all())
        .map_err(|error| format!("writing staged .af/af.toml: {error}"))?;
    for (path, bytes) in files {
        let relative = path
            .strip_prefix(".af/")
            .ok_or("generated Task path is outside .af/")?;
        let target = staged.join(relative);
        std::fs::create_dir_all(target.parent().ok_or("generated Task path has no parent")?)
            .map_err(|error| format!("staging {path}: {error}"))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("staging {path}: {error}"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("writing staged {path}: {error}"))?;
    }
    sync_tree_directories(&staged)?;
    std::fs::File::open(stage.path())
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing staged Task authority: {error}"))?;
    // Check again after staging. A concurrent addition or project edit is a refusal, not an
    // overwrite; the staged tree disappears with TempDir while the original remains mounted.
    if preflight(repo, files)? != original {
        return Err(".af/af.toml changed during onboarding; rerun af onboard --tasks".into());
    }
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        &staged,
        rustix::fs::CWD,
        &authority,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(|error| format!("installing Task authority atomically: {error}"))?;
    if let Err(error) = std::fs::File::open(repo).and_then(|directory| directory.sync_all()) {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &staged,
            rustix::fs::CWD,
            &authority,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .map_err(|rollback| {
            format!("syncing repository after Task onboarding failed ({error}); restoring original authority also failed: {rollback}")
        })?;
        return Err(format!("syncing repository after Task onboarding: {error}"));
    }
    Ok(())
}

fn stage_authority(source: &Path, target: &Path) -> Result<(), String> {
    use rustix::fs::{Mode, OFlags, open};
    let directory: std::fs::File = open(
        source,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("opening existing .af/ without following links: {error}"))?
    .into();
    std::fs::create_dir(target)
        .map_err(|error| format!("staging existing .af/ directory: {error}"))?;
    stage_authority_directory(&directory, source, target)
}

fn stage_authority_directory(
    directory: &std::fs::File,
    source: &Path,
    target: &Path,
) -> Result<(), String> {
    use rustix::fs::{AtFlags, Dir, Mode, OFlags, linkat, openat};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let destination = std::fs::File::open(target)
        .map_err(|error| format!("opening staged authority {}: {error}", target.display()))?;
    let entries = Dir::read_from(directory)
        .map_err(|error| format!("reading existing authority: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("reading existing authority: {error}"))?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = std::ffi::OsStr::from_bytes(bytes);
        let path = target.join(name);
        let source_entry = source.join(name);
        let opened: std::fs::File = openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| {
            format!(
                "opening existing authority entry {} without following links: {error}",
                source_entry.display()
            )
        })?
        .into();
        let metadata = opened.metadata().map_err(|error| {
            format!(
                "inspecting existing authority entry {}: {error}",
                source_entry.display()
            )
        })?;
        if metadata.is_dir() {
            std::fs::create_dir(&path).map_err(|error| {
                format!("staging authority directory {}: {error}", path.display())
            })?;
            stage_authority_directory(&opened, &source_entry, &path)?;
        } else if metadata.is_file() {
            linkat(directory, name, &destination, name, AtFlags::empty()).map_err(|error| {
                format!(
                    "linking existing authority file {}: {error}",
                    source_entry.display()
                )
            })?;
            let linked = std::fs::symlink_metadata(&path).map_err(|error| {
                format!("checking staged authority file {}: {error}", path.display())
            })?;
            if !linked.is_file() || linked.dev() != metadata.dev() || linked.ino() != metadata.ino()
            {
                return Err(format!(
                    "existing authority file {} changed while staging; rerun af onboard --tasks",
                    source_entry.display()
                ));
            }
        } else {
            return Err(format!(
                "existing authority entry {} is not a regular file or directory; inspect it before af onboard --tasks",
                source_entry.display()
            ));
        }
    }
    std::fs::set_permissions(
        target,
        directory
            .metadata()
            .map_err(|error| error.to_string())?
            .permissions(),
    )
    .map_err(|error| {
        format!(
            "preserving authority directory permissions {}: {error}",
            target.display()
        )
    })
}

fn sync_tree_directories(root: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(root)
        .map_err(|error| format!("reading staged authority {}: {error}", root.display()))?
    {
        let entry = entry
            .map_err(|error| format!("reading staged authority {}: {error}", root.display()))?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            sync_tree_directories(&entry.path())?;
        }
    }
    std::fs::File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing staged authority {}: {error}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_gate_name_stays_one_policy_check_and_a_review_result_keeps_its_domain_type() {
        let gate = Gate::detected("project.check", "make", &["check"], "Makefile");
        let generated = files(&[gate], RunnerProfile::Codex).unwrap();
        let policy: toml::Value =
            toml::from_str(std::str::from_utf8(&generated[".af/code-policy.toml"]).unwrap())
                .unwrap();
        assert_eq!(
            policy["checks"]["project.check"]["name"].as_str(),
            Some("project.check")
        );
        let bugs =
            std::str::from_utf8(&generated[".af/task-packages/onboard/bugs/worker.toml"]).unwrap();
        assert!(bugs.contains("review.kernel/ReviewerResult@2"));
        assert!(bugs.contains("name = \"onboard/bugs\""));
    }
}
