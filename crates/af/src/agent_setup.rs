//! Repository-local agent entry point. Preview and preflight are read-only.

use std::path::{Path, PathBuf};

const START: &str = "<!-- af-agent-help:start -->";
const END: &str = "<!-- af-agent-help:end -->";
const SKILLS: [&str; 2] = [".claude/skills/af/SKILL.md", ".agents/skills/af/SKILL.md"];

fn agents_content(existing: &str) -> Result<String, String> {
    let snippet = crate::topics::agent_snippet();
    match (existing.find(START), existing.find(END)) {
        (None, None) => {
            let mut result = existing.to_string();
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(snippet);
            Ok(result)
        }
        (Some(start), Some(end)) if end > start && existing.matches(START).count() == 1 && existing.matches(END).count() == 1 => {
            let after = end + END.len();
            let suffix = &existing[after..];
            let suffix = suffix.strip_prefix('\n').unwrap_or(suffix);
            Ok(format!("{}{}{}", &existing[..start], snippet, suffix))
        }
        _ => Err("AGENTS.md has incomplete or duplicate af agent markers; fix the marked block and retry af setup agents".into()),
    }
}

fn safe_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for segment in relative.split('/') {
        path.push(segment);
        if path.symlink_metadata().is_ok() {
            let metadata = path.symlink_metadata().map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "{} is a symlink; choose a repository without a linked agent path",
                    path.display()
                ));
            }
        }
    }
    Ok(path)
}

pub(crate) fn run(repo: &Path, write: bool) -> Result<i32, String> {
    let root = std::fs::canonicalize(repo).map_err(|error| {
        format!(
            "opening --repo {}: {error}; choose an existing Git repository",
            repo.display()
        )
    })?;
    let root = crate::config::git_toplevel(&root).ok_or_else(|| {
        format!(
            "--repo {} is not a Git repository; choose a Git repository and retry af setup agents",
            root.display()
        )
    })?;
    let agents = safe_path(&root, "AGENTS.md")?;
    let old_agents = match std::fs::read_to_string(&agents) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "reading {}: {error}; fix the file and retry af setup agents",
                agents.display()
            ));
        }
    };
    let new_agents = agents_content(&old_agents)?;
    let skill = crate::topics::agent_skill();
    let mut planned = vec![(agents, old_agents, new_agents)];
    for relative in SKILLS {
        let path = safe_path(&root, relative)?;
        let present = path.exists();
        let old = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(format!(
                    "reading {}: {error}; fix the file and retry af setup agents",
                    path.display()
                ));
            }
        };
        if present && old != skill && !crate::topics::is_generated_agent_skill(&old) {
            return Err(format!(
                "{} contains a foreign SKILL.md; move it aside and retry af setup agents",
                path.display()
            ));
        }
        planned.push((path, old, skill.clone()));
    }
    for (path, old, new) in planned {
        if write && old != new {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("creating {}: {error}", parent.display()))?;
            }
            std::fs::write(&path, &new)
                .map_err(|error| format!("writing {}: {error}", path.display()))?;
        }
        println!(
            "{} {}\n{}",
            if old == new {
                "unchanged"
            } else if write {
                "wrote"
            } else {
                "would write"
            },
            path.display(),
            new
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_block_preserves_foreign_text_and_is_idempotent() {
        let first = agents_content("foreign text\n").unwrap();
        assert!(first.starts_with("foreign text\n\n"));
        assert_eq!(agents_content(&first).unwrap(), first);
        assert!(agents_content("foreign text\n<!-- af-agent-help:start -->").is_err());
    }
}
