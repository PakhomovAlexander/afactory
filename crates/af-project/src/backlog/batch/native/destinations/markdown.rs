use super::*;
use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, Stat};
use rustix::io::Errno;
use std::fs::File;
use std::io::{Seek as _, SeekFrom};
use std::path::{Component, PathBuf};

const CONFIG: &str = "backlog-markdown-destination-v2.json";
const RESERVED: &str = "<!-- af-backlog-";
const BEGIN: &str = "<!-- af-backlog-entry-v1:";
const END: &str = "<!-- af-backlog-end-v1 -->\n";
const MAX_DOCUMENT: usize = 8 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    UnversionedFile,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    schema: String,
    project_id: String,
    scope: Scope,
    path: PathBuf,
}

impl Config {
    pub(super) fn load(store: &ProjectStore, destination: &str) -> Result<Self, String> {
        let bytes = owned_file(&store.directory.join(CONFIG), 8192)?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Markdown destination")?;
        if config.schema != "af/backlog-markdown-destination@2"
            || config.project_id != store.project_id
            || !config.path.is_absolute()
            || config.path.components().collect::<PathBuf>().as_os_str() != config.path.as_os_str()
            || config.path.extension().is_none_or(|ext| ext != "md")
            || value_id(&config)? != destination
        {
            return Err("Markdown destination differs from pinned Project authority".into());
        }
        Ok(config)
    }

    pub(super) fn deliver(
        self,
        store: &ProjectStore,
        delivery: Delivery<'_>,
        mapped_items: &BTreeMap<String, String>,
    ) -> Result<Observation, Pending> {
        let Delivery {
            publication,
            item,
            operation,
            cancellation,
            deadline,
            ..
        } = delivery;
        let creating = matches!(operation, Operation::Create);
        let conflict = if creating {
            Pending::PrewriteConflict
        } else {
            Pending::Conflict
        };
        let (parent, mut file) = open_document(&self.path).map_err(|_| conflict)?;
        fs::flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|_| conflict)?;
        let (original, stamp) = read_document(&mut file).map_err(|_| conflict)?;
        let document = std::str::from_utf8(&original).map_err(|_| conflict)?;
        let content = format!("## {}\n\n{}\n", item.public.title, item.public.body);
        if content.contains(RESERVED) {
            return Err(conflict);
        }
        let digest = af_store::canonical::blob_content_id(content.as_bytes());
        let intent = &publication.intent_id;
        let entries = entries(document, &self.project_id).ok_or(conflict)?;
        for (id, entry) in &entries {
            if mapped_items.get(*id) != Some(&entry.remote_id)
                && (creating
                    || *id != item.item_id
                    || entry.intent != intent
                    || entry.digest != digest)
            {
                return Err(conflict);
            }
        }
        if mapped_items
            .keys()
            .any(|id| !entries.contains_key(id.as_str()))
        {
            return Err(conflict);
        }

        let (remote_id, observed_digest) = if creating {
            let append = format!(
                "\n\n{BEGIN}{}|{}|{intent}|{digest} -->\n{content}{END}",
                self.project_id, item.item_id
            );
            if original.len() + append.len() > MAX_DOCUMENT {
                return Err(conflict);
            }
            Self::load(store, &publication.destination_id)
                .map_err(|_| Pending::AuthorityUnavailable)?;
            append_checked(
                &self.path,
                &parent,
                &mut file,
                (&original, stamp),
                append.as_bytes(),
                cancellation,
                deadline,
            )?;
            (remote_id(&item.item_id, intent, &digest), digest.as_str())
        } else {
            let entry = entries
                .get(item.item_id.as_str())
                .ok_or(Pending::NotObserved)?;
            if !still_at_path(&self.path, &parent, stamp)
                || read_document(&mut file).map_err(|_| conflict)?.0 != original
            {
                return Err(conflict);
            }
            file.sync_all()
                .and_then(|()| parent.sync_all())
                .map_err(|_| Pending::AdapterUnavailable)?;
            (entry.remote_id.clone(), entry.digest)
        };
        let fields_id = if observed_digest == digest {
            value_id(&(&item.public.title, &item.public.body)).map_err(|_| Pending::Conflict)?
        } else {
            observed_digest.to_owned()
        };
        Ok(Observation {
            remote_id,
            issue_key: None,
            closed: false,
            fields_id,
        })
    }
}

fn open_parent(path: &Path) -> rustix::io::Result<File> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(fs::open("/", flags, Mode::empty())?);
    let parent = path.parent().ok_or(Errno::INVAL)?;
    for component in parent.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = File::from(fs::openat(&directory, name, flags, Mode::empty())?);
            }
            _ => return Err(Errno::INVAL),
        }
        let absent = |name| {
            matches!(
                fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW),
                Err(Errno::NOENT)
            )
        };
        // A .git file also denotes linked worktrees and submodules. Fail closed on
        // inspection errors and on bare repository metadata.
        if !absent(".git") || (!absent("HEAD") && !absent("objects")) {
            return Err(Errno::PERM);
        }
    }
    let owner = fs::fstat(&directory)?;
    if owner.st_uid != nix::unistd::geteuid().as_raw() || owner.st_mode & 0o022 != 0 {
        return Err(Errno::PERM);
    }
    Ok(directory)
}

fn checked_stat(parent: &File, path: &Path) -> rustix::io::Result<Stat> {
    let stat = fs::statat(
        parent,
        path.file_name().ok_or(Errno::INVAL)?,
        AtFlags::SYMLINK_NOFOLLOW,
    )?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != nix::unistd::geteuid().as_raw()
        || stat.st_mode & 0o022 != 0
        || stat.st_nlink != 1
    {
        return Err(Errno::PERM);
    }
    Ok(stat)
}

fn open_document(path: &Path) -> rustix::io::Result<(File, File)> {
    let parent = open_parent(path)?;
    let before = checked_stat(&parent, path)?;
    let flags =
        OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = File::from(fs::openat(
        &parent,
        path.file_name().ok_or(Errno::INVAL)?,
        flags,
        Mode::empty(),
    )?);
    if stamp(&before) != stamp(&fs::fstat(&file)?) {
        return Err(Errno::AGAIN);
    }
    Ok((parent, file))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    identity: (i128, u64),
    change: (i64, i64),
    modified: (i64, i64),
    size: i64,
}

fn stamp(stat: &Stat) -> Stamp {
    Stamp {
        identity: (i128::from(stat.st_dev), stat.st_ino),
        change: (stat.st_ctime, stat.st_ctime_nsec),
        modified: (stat.st_mtime, stat.st_mtime_nsec),
        size: stat.st_size,
    }
}

fn read_document(file: &mut File) -> Result<(Vec<u8>, Stamp), ()> {
    let before = stamp(&fs::fstat(&*file).map_err(|_| ())?);
    file.seek(SeekFrom::Start(0)).map_err(|_| ())?;
    let mut bytes = Vec::new();
    file.take((MAX_DOCUMENT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_DOCUMENT || before != stamp(&fs::fstat(&*file).map_err(|_| ())?) {
        return Err(());
    }
    Ok((bytes, before))
}

fn still_at_path(path: &Path, parent: &File, expected: Stamp) -> bool {
    let matches = || {
        let current = open_parent(path)?;
        Ok::<_, Errno>(
            stamp(&fs::fstat(parent)?).identity == stamp(&fs::fstat(&current)?).identity
                && stamp(&checked_stat(&current, path)?) == expected,
        )
    };
    matches().unwrap_or(false)
}

fn append_checked(
    path: &Path,
    parent: &File,
    file: &mut File,
    snapshot: (&[u8], Stamp),
    append: &[u8],
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<(), Pending> {
    let (original, expected) = snapshot;
    if !matches!(read_document(file), Ok((bytes, stamp)) if bytes == original && stamp == expected)
        || !still_at_path(path, parent, expected)
    {
        return Err(Pending::PrewriteConflict);
    }
    running(cancellation, deadline).map_err(|_| Pending::AuthorityUnavailable)?;
    // One O_APPEND write. Never overwrite human bytes, retry a short write or roll back
    // an ambiguous write. Replay must observe the exact complete managed entry.
    match file.write(append) {
        Ok(count) if count == append.len() => {}
        _ => return Err(Pending::AdapterUnavailable),
    }
    file.sync_all()
        .and_then(|()| parent.sync_all())
        .map_err(|_| Pending::AdapterUnavailable)?;
    let (observed, after) = read_document(file).map_err(|_| Pending::Conflict)?;
    if observed.strip_prefix(original) != Some(append) || !still_at_path(path, parent, after) {
        return Err(Pending::Conflict);
    }
    Ok(())
}

struct Entry<'a> {
    intent: &'a str,
    digest: &'a str,
    remote_id: String,
}

fn entries<'a>(mut text: &'a str, project: &str) -> Option<BTreeMap<&'a str, Entry<'a>>> {
    let mut entries = BTreeMap::new();
    while let Some(index) = text.find(RESERVED) {
        if index != 0 && !text[..index].ends_with('\n') {
            return None;
        }
        let (header, remainder) = text[index..].split_once('\n')?;
        let header = header.strip_prefix(BEGIN)?.strip_suffix(" -->")?;
        let mut fields = header.split('|');
        if fields.next()? != project {
            return None;
        }
        let (item, intent, digest) = (fields.next()?, fields.next()?, fields.next()?);
        if fields.next().is_some() {
            return None;
        }
        if ![item, intent, digest]
            .into_iter()
            .all(af_contracts::is_digest)
        {
            return None;
        }
        let end = remainder.find(RESERVED)?;
        let content = &remainder[..end];
        text = remainder[end..].strip_prefix(END)?;
        if !content.ends_with('\n')
            || af_store::canonical::blob_content_id(content.as_bytes()) != digest
        {
            return None;
        }
        if entries
            .insert(
                item,
                Entry {
                    intent,
                    digest,
                    remote_id: remote_id(item, intent, digest),
                },
            )
            .is_some()
        {
            return None;
        }
    }
    Some(entries)
}

fn remote_id(item: &str, intent: &str, digest: &str) -> String {
    format!("markdown:{}:{}:{}", &item[7..], &intent[7..], &digest[7..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    #[test]
    fn markdown_refuses_checkout_worktree_bare_repository_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        for (name, metadata) in [
            ("checkout", ".git"),
            ("worktree", ".git"),
            ("bare", "objects"),
        ] {
            let repository = root.join(name);
            std_fs::create_dir(&repository).unwrap();
            if name == "worktree" {
                std_fs::write(repository.join(metadata), "gitdir: elsewhere\n").unwrap();
            } else {
                std_fs::create_dir(repository.join(metadata)).unwrap();
            }
            if name == "bare" {
                std_fs::write(repository.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            }
            let path = repository.join("Backlog.md");
            std_fs::write(&path, "Version-controlled prose").unwrap();
            assert!(open_document(&path).is_err());
            assert_eq!(
                std_fs::read_to_string(path).unwrap(),
                "Version-controlled prose"
            );
        }

        let real = root.join("real");
        std_fs::create_dir(&real).unwrap();
        std_fs::write(real.join("Backlog.md"), "Human notes").unwrap();
        symlink(&real, root.join("alias")).unwrap();
        symlink(real.join("Backlog.md"), root.join("Link.md")).unwrap();
        assert!(open_document(&root.join("alias/Backlog.md")).is_err());
        assert!(open_document(&root.join("Link.md")).is_err());
        std_fs::hard_link(real.join("Backlog.md"), root.join("Hardlink.md")).unwrap();
        assert!(open_document(&root.join("Hardlink.md")).is_err());
        assert_eq!(
            std_fs::read_to_string(real.join("Backlog.md")).unwrap(),
            "Human notes"
        );
    }

    #[test]
    fn markdown_preserves_edits_replacements_and_cancelled_writes() {
        for change in ["edit", "replace", "symlink", "cancel", "expire"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().canonicalize().unwrap().join("Backlog.md");
            std_fs::write(&path, "Before").unwrap();
            let (parent, mut file) = open_document(&path).unwrap();
            let (original, stamp) = read_document(&mut file).unwrap();
            let displaced = path.with_file_name("Displaced.md");
            match change {
                "edit" => std_fs::write(&path, "Edited by a human").unwrap(),
                "replace" | "symlink" => {
                    std_fs::rename(&path, &displaced).unwrap();
                    if change == "replace" {
                        std_fs::write(&path, "Replacement").unwrap();
                    } else {
                        symlink(&displaced, &path).unwrap();
                    }
                }
                _ => {}
            }
            let unchanged = std_fs::read(&path).unwrap();
            let result = append_checked(
                &path,
                &parent,
                &mut file,
                (&original, stamp),
                b"AF entry",
                &AtomicBool::new(change == "cancel"),
                Instant::now()
                    + if change == "expire" {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(5)
                    },
            );
            if matches!(change, "cancel" | "expire") {
                assert!(matches!(result, Err(Pending::AuthorityUnavailable)));
            } else {
                assert!(matches!(result, Err(Pending::PrewriteConflict)));
            }
            assert_eq!(std_fs::read(&path).unwrap(), unchanged);
            if displaced.exists() {
                assert_eq!(std_fs::read_to_string(displaced).unwrap(), "Before");
            }
        }
    }
}
