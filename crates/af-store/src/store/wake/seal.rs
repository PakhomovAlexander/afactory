//! Independent Store-private provenance for retained continuation, including prior facts/clocks.
//! SQLite checksums cannot mint this attestation. The trusted kernel writes it only with a
//! checked projector page, under the same transaction as cursor/outbox publication.
use super::*;
use hmac::{Hmac, Mac};
use sha2_seal::Sha256;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

type Authenticator = Hmac<Sha256>;
const KEY: &str = "wake-checkpoint-key-v1";
const PENDING: &str = "wake-checkpoint-key-v1.pending";

impl EventStore {
    // Called with the writer lock. Before incarnation admission, an interrupted partial
    // staging file is disposable; after admission a missing/damaged key is always terminal.
    pub(super) fn checkpoint_key(&self, create: bool) -> Result<[u8; 32], StoreError> {
        let database = self
            .database_path()
            .ok_or_else(|| refusal("Missing wake Store"))?;
        let parent = database
            .parent()
            .ok_or_else(|| refusal("Missing wake Store root"))?;
        let path = parent.join(KEY);
        let absent = match std::fs::symlink_metadata(&path) {
            Ok(_) => false,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(refusal(&e.to_string())),
        };
        if create && absent {
            let pending = parent.join(PENDING);
            match std::fs::remove_file(&pending) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(refusal(&e.to_string())),
            }
            let mut key = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut key))
                .map_err(|e| refusal(&e.to_string()))?;
            let mut file = crate::local_fs::private_file_options()
                .write(true)
                .create_new(true)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
                .open(&pending)
                .map_err(|e| refusal(&e.to_string()))?;
            file.write_all(&key).map_err(|e| refusal(&e.to_string()))?;
            nix::unistd::fsync(&file).map_err(|e| refusal(&e.to_string()))?;
            std::fs::rename(&pending, &path).map_err(|e| refusal(&e.to_string()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| refusal(&e.to_string()))?;
        let info = file.metadata().map_err(|e| refusal(&e.to_string()))?;
        if !info.is_file()
            || info.nlink() != 1
            || info.uid() != nix::unistd::geteuid().as_raw()
            || info.mode() & 0o077 != 0
            || info.len() != 32
        {
            return Err(refusal(
                "Invalid wake checkpoint key ownership, type or size",
            ));
        }
        let mut key = [0u8; 32];
        file.read_exact(&mut key)
            .map_err(|e| refusal(&e.to_string()))?;
        // Recover publication durability even when the key predated this process. Never
        // acknowledge an incarnation/usable proof with an unsynced key or parent entry.
        nix::unistd::fsync(&file).map_err(|e| refusal(&e.to_string()))?;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(parent)
            .map_err(|e| refusal(&e.to_string()))?;
        nix::unistd::fsync(&directory).map_err(|e| refusal(&e.to_string()))?;
        Ok(key)
    }

    fn checkpoint_authenticator(
        &self,
        registration: &str,
        text: &str,
        through: u64,
        source: u64,
    ) -> Result<Authenticator, StoreError> {
        // Validate the closed shape before signing/verifying. Authenticate exact serialized
        // bytes, full registration and both progress columns, not merely a derived summary.
        checkpoint_digest(text)?;
        let material = serde_json::to_vec(&json!({
            "domain":"af/wake-checkpoint-seal/1", "store":self.wake_incarnation()?,
            "registration":registration, "checkpoint":text,
            "through_sequence":through, "source_sequence":source
        }))?;
        let mut mac = Authenticator::new_from_slice(&self.checkpoint_key(false)?)
            .map_err(|_| refusal("Invalid wake seal key"))?;
        mac.update(&material);
        Ok(mac)
    }

    pub(super) fn seal_checkpoint(
        &self,
        id: &str,
        text: &str,
        through: u64,
        source: u64,
    ) -> Result<String, StoreError> {
        let registration: String = self.conn.query_row(
            "SELECT registration FROM wake_subscriptions_v1 WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        let tag = self
            .checkpoint_authenticator(&registration, text, through, source)?
            .finalize()
            .into_bytes();
        Ok(serde_json::to_string(&WakeCheckpointSealV1 {
            schema: "af/task-wake-checkpoint-seal@1".into(),
            tag: format!("sha256:{}", af_contracts::hex::encode(&tag)),
        })?)
    }

    pub(super) fn verify_checkpoint_seal_for_registration(
        &self,
        registration: &str,
        text: &str,
        seal: &str,
    ) -> Result<(), StoreError> {
        let parsed: WakeRegistration = serde_json::from_str(registration)?;
        self.verify_checkpoint_seal(&parsed.subscription.subscription_id, text, seal)
    }

    pub(super) fn verify_checkpoint_seal(
        &self,
        id: &str,
        text: &str,
        seal: &str,
    ) -> Result<(), StoreError> {
        let seal: WakeCheckpointSealV1 = serde_json::from_str(seal)?;
        seal.validate().map_err(|e| refusal(&e))?;
        let (registration, through, source): (String, u64, u64) = self.conn.query_row(
            "SELECT registration,through_sequence,source_sequence FROM wake_subscriptions_v1 WHERE id=?1",
            [id], |r| Ok((r.get(0)?, u64_column(r, 1)?, u64_column(r, 2)?)))?;
        let tag = (7..71)
            .step_by(2)
            .map(|at| {
                u8::from_str_radix(&seal.tag[at..at + 2], 16)
                    .map_err(|_| refusal("Invalid wake seal tag"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.checkpoint_authenticator(&registration, text, through, source)?
            .verify_slice(&tag)
            .map_err(|_| refusal("Wake checkpoint provenance seal mismatch"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg(target_os = "linux")]
    fn wake_seal_key_is_private_durable_and_never_recreated_after_admission() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("events.sqlite");
        let store = EventStore::open(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let pending = directory.path().join(PENDING);
        std::fs::write(&pending, b"partial").unwrap();
        let identity = store.admit_wake_incarnation().unwrap();
        let key = store.checkpoint_key(false).unwrap();
        assert!(!pending.exists());
        drop(store);
        let store = EventStore::open(&path).unwrap();
        assert_eq!(store.admit_wake_incarnation().unwrap(), identity);
        assert_eq!(store.checkpoint_key(false).unwrap(), key);
        let file = directory.path().join(KEY);
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.admit_wake_incarnation().is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let foreign = directory.path().join("foreign");
        std::fs::hard_link(&file, &foreign).unwrap();
        assert!(store.admit_wake_incarnation().is_err());
        std::fs::remove_file(&foreign).unwrap();
        std::fs::write(&file, b"short").unwrap();
        assert!(store.admit_wake_incarnation().is_err());
        std::fs::remove_file(&file).unwrap();
        assert!(store.admit_wake_incarnation().is_err());
        assert!(
            !file.exists(),
            "key loss cannot mint replacement provenance"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn wake_seal_unsupported_platform_refuses_without_creating_provenance() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("events.sqlite");
        let store = EventStore::open(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        for _ in 0..2 {
            let error = store.admit_wake_incarnation().unwrap_err().to_string();
            assert!(
                error.contains(
                    "Wake Store incarnation admission currently requires Linux machine identity"
                ),
                "{error}"
            );
            assert!(!directory.path().join(KEY).exists());
            assert!(!directory.path().join(PENDING).exists());
            let count: i64 = store
                .conn
                .query_row("SELECT COUNT(*) FROM wake_incarnation_v1", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "unsupported admission cannot create provenance");
        }
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn wake_seal_admission_refuses_unsupported_machine_identity_without_creating_authority() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("events.sqlite");
        for _ in 0..2 {
            let store = EventStore::open(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            for result in [store.admit_wake_incarnation(), store.wake_incarnation()] {
                assert!(result.unwrap_err().to_string().contains(
                    "Wake Store incarnation admission currently requires Linux machine identity"
                ));
            }
            let admitted: i64 = store
                .conn
                .query_row("SELECT count(*) FROM wake_incarnation_v1", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(admitted, 0);
            assert!(!root.join(KEY).exists());
            assert!(!root.join(PENDING).exists());
        }
    }

    #[test]
    fn wake_seal_authenticator_matches_rfc4231_and_refuses_recomputed_digest() {
        let mut mac = Authenticator::new_from_slice(&[0x0b; 20]).unwrap();
        mac.update(b"Hi There");
        let bytes = mac.clone().finalize().into_bytes();
        assert_eq!(
            af_contracts::hex::encode(&bytes),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        use sha2_seal::Digest;
        assert!(mac.verify_slice(&Sha256::digest(b"Hi There")).is_err());
    }
}
