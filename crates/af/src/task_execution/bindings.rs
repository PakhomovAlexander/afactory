//! Explicit local settings are captured once. Resume never follows their filesystem paths.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LocalBindings {
    schema: String,
    #[serde(default)]
    pub packages: BTreeMap<String, TaskPackagePin>,
    #[serde(default)]
    pub slots: BTreeMap<String, String>,
    #[serde(default)]
    pub providers: BTreeMap<String, String>,
}

pub(super) struct CapturedLocalBindings {
    pub definition: LocalBindings,
    pub files: BTreeMap<String, Vec<u8>>,
}

pub(super) fn read(path: &Path) -> Result<CapturedLocalBindings, String> {
    let bytes = read_file(path, 1024 * 1024)?;
    let definition: LocalBindings = parse(path, &bytes)?;
    if definition.schema != "af.task-bindings/1"
        || definition.packages.len() > 128
        || definition.slots.len() > 64
        || definition.providers.len() > 128
        || definition
            .packages
            .keys()
            .any(|name| !name.starts_with("local/") || !is_package_name(name))
        || definition
            .providers
            .iter()
            .any(|(worker, alias)| !is_package_name(worker) || !is_name(alias))
    {
        return Err(
            "Local bindings require af.task-bindings/1 and unambiguous local/* packages".into(),
        );
    }
    let base = path.parent().unwrap_or(Path::new("."));
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for pin in definition.packages.values() {
        if Path::new(&pin.path).is_absolute()
            || pin.path.contains('\\')
            || pin
                .path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(
                "Local package paths must remain beneath the bindings file directory".into(),
            );
        }
        let mut ancestor = base.to_path_buf();
        for part in pin.path.split('/') {
            ancestor.push(part);
            if std::fs::symlink_metadata(&ancestor)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Local package path cannot follow a symlink".into());
            }
        }
        capture_tree(base, &pin.path, &mut files, &mut total, 0)?;
    }
    Ok(CapturedLocalBindings { definition, files })
}

fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("Local authority requires bounded regular files".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Local authority file exceeds capture limit".into());
    }
    Ok(bytes)
}

fn capture_tree(
    base: &Path,
    relative: &str,
    files: &mut BTreeMap<String, Vec<u8>>,
    total: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 || files.len() >= 4096 {
        return Err("Local package tree exceeds capture bounds".into());
    }
    let path = base.join(relative);
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Local package filenames must be UTF-8")?;
            capture_tree(base, &format!("{relative}/{name}"), files, total, depth + 1)?;
        }
    } else {
        if files.contains_key(relative) {
            return Ok(());
        }
        let bytes = read_file(&path, 16 * 1024 * 1024)?;
        *total += bytes.len();
        if *total > 64 * 1024 * 1024 {
            return Err("Local packages exceed the captured closure bound".into());
        }
        files.insert(relative.into(), bytes);
    }
    Ok(())
}
