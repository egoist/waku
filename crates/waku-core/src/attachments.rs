//! Daemon-owned materialization of client-uploaded attachments.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use base64::Engine as _;
use uuid::Uuid;

pub use waku_protocol::attachments::{
    ATTACHMENT_SCHEME, AttachmentUpload, AttachmentUploadEntry, MAX_ATTACHMENT_BYTES,
    MAX_ATTACHMENT_FILES, StoredAttachment,
};

pub struct AttachmentStore {
    root: PathBuf,
}

impl AttachmentStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn import(&self, name: &str, upload: AttachmentUpload) -> io::Result<StoredAttachment> {
        self.materialize(name, |target| match upload {
            AttachmentUpload::File { data_base64 } => {
                let bytes = decode(&data_base64)?;
                ensure_size(bytes.len())?;
                fs::write(target, bytes)?;
                Ok(false)
            }
            AttachmentUpload::Directory { entries } => {
                if entries.len() > MAX_ATTACHMENT_FILES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "attachment directory contains too many files",
                    ));
                }
                fs::create_dir_all(target)?;
                let mut total_bytes = 0usize;
                for entry in entries {
                    let relative = safe_relative_path(&entry.relative_path)?;
                    let bytes = decode(&entry.data_base64)?;
                    total_bytes = total_bytes.checked_add(bytes.len()).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "attachment directory is too large",
                        )
                    })?;
                    ensure_size(total_bytes)?;
                    let path = target.join(relative);
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(path, bytes)?;
                }
                Ok(true)
            }
        })
    }

    /// Copies an absolute path on the daemon host into the managed attachment
    /// store. The client sends only the path; file bytes never cross the RPC.
    pub fn import_path(&self, path: &Path) -> io::Result<StoredAttachment> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "daemon attachment path must be absolute",
            ));
        }
        let source = fs::canonicalize(path)?;
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid attachment name")
            })?;
        self.materialize(name, |target| copy_source(&source, target))
    }

    fn materialize(
        &self,
        name: &str,
        populate: impl FnOnce(&Path) -> io::Result<bool>,
    ) -> io::Result<StoredAttachment> {
        let name = safe_name(name)?;
        let id = Uuid::new_v4();
        let reference = format!("{ATTACHMENT_SCHEME}{id}");
        let staging = self.root.join(format!(".{id}.tmp"));
        let destination = self.root.join(id.to_string());
        fs::create_dir_all(&staging)?;
        let target = staging.join(&name);
        let materialized = populate(&target);
        let is_dir = match materialized {
            Ok(is_dir) => is_dir,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Err(error) = fs::rename(&staging, &destination) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        Ok(StoredAttachment {
            reference,
            path: destination.join(&name),
            name,
            is_dir,
        })
    }

    pub fn path_for(&self, reference: &str) -> Option<PathBuf> {
        let id = reference.strip_prefix(ATTACHMENT_SCHEME)?;
        let id = Uuid::parse_str(id).ok()?;
        Some(self.root.join(id.to_string()))
    }

    pub fn read_file(&self, reference: &str, path: &Path) -> io::Result<Vec<u8>> {
        let root = self.path_for(reference).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid attachment reference")
        })?;
        let relative = path.strip_prefix(&root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "attachment path does not belong to its reference",
            )
        })?;
        safe_relative_path(relative)?;
        fs::read(path)
    }

    /// Removes daemon-owned attachment trees that are no longer referenced by
    /// persisted messages or composer drafts. In-progress staging directories
    /// are deliberately ignored so an upload cannot race a cleanup pass.
    pub fn retain(&self, live: &HashSet<String>) -> io::Result<u64> {
        self.retain_with_cutoff(live, None)
    }

    /// Reclaims only attachment trees old enough that no upload-to-draft or
    /// upload-to-session handoff can still be committing their references.
    pub fn retain_unreferenced_older_than(
        &self,
        live: &HashSet<String>,
        cutoff: SystemTime,
    ) -> io::Result<u64> {
        self.retain_with_cutoff(live, Some(cutoff))
    }

    fn retain_with_cutoff(
        &self,
        live: &HashSet<String>,
        cutoff: Option<SystemTime>,
    ) -> io::Result<u64> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(0);
        };
        let mut reclaimed = 0;
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(id) = Uuid::parse_str(&name) else {
                continue;
            };
            if live.contains(&format!("{ATTACHMENT_SCHEME}{id}")) {
                continue;
            }
            if cutoff.is_some_and(|cutoff| {
                entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .map_or(true, |modified| modified >= cutoff)
            }) {
                continue;
            }
            reclaimed += directory_size(&entry.path()).unwrap_or_default();
            fs::remove_dir_all(entry.path())?;
        }
        Ok(reclaimed)
    }
}

fn copy_source(source: &Path, target: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symbolic-link attachments are not supported",
        ));
    }
    if metadata.is_file() {
        if metadata.len() > MAX_ATTACHMENT_BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment is larger than 32 MB",
            ));
        }
        let copied = fs::copy(source, target)?;
        if copied > MAX_ATTACHMENT_BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment is larger than 32 MB",
            ));
        }
        return Ok(false);
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attachment is not a file or directory",
        ));
    }

    fs::create_dir_all(target)?;
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    let mut file_count = 0usize;
    let mut total_bytes = 0u64;
    while let Some((source_directory, target_directory)) = pending.pop() {
        for entry in fs::read_dir(source_directory)? {
            let entry = entry?;
            let source_path = entry.path();
            let target_path = target_directory.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                fs::create_dir_all(&target_path)?;
                pending.push((source_path, target_path));
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            file_count += 1;
            if file_count > MAX_ATTACHMENT_FILES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attachment directory contains too many files",
                ));
            }
            total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attachment directory is too large",
                )
            })?;
            if total_bytes > MAX_ATTACHMENT_BYTES as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attachment directory is too large",
                ));
            }
            let copied = fs::copy(source_path, target_path)?;
            if copied > metadata.len() {
                total_bytes = total_bytes
                    .checked_add(copied - metadata.len())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "attachment directory is too large",
                        )
                    })?;
                if total_bytes > MAX_ATTACHMENT_BYTES as u64 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "attachment directory is too large",
                    ));
                }
            }
        }
    }
    Ok(true)
}

fn directory_size(root: &Path) -> io::Result<u64> {
    let mut bytes = 0u64;
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(bytes)
}

fn decode(data: &str) -> io::Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn ensure_size(bytes: usize) -> io::Result<()> {
    if bytes > MAX_ATTACHMENT_BYTES {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attachment is larger than 32 MB",
        ))
    } else {
        Ok(())
    }
}

fn safe_name(name: &str) -> io::Result<String> {
    let path = Path::new(name);
    let mut components = path.components();
    let name = components
        .next()
        .filter(|_| components.next().is_none())
        .and_then(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid attachment name"))?;
    Ok(name.to_owned())
}

fn safe_relative_path(path: &Path) -> io::Result<&Path> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attachment entry escapes its root",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_components() {
        assert!(safe_relative_path(Path::new("../secret")).is_err());
        assert!(safe_relative_path(Path::new("nested/file.txt")).is_ok());
        assert!(safe_name("../secret.txt").is_err());
        assert_eq!(safe_name("secret.txt").unwrap(), "secret.txt");
        assert!(ensure_size(MAX_ATTACHMENT_BYTES).is_ok());
        assert!(ensure_size(MAX_ATTACHMENT_BYTES + 1).is_err());
    }

    #[test]
    fn imports_any_daemon_file_without_routing_bytes_through_the_client() {
        let source_directory =
            std::env::temp_dir().join(format!("waku-file-source-{}", Uuid::new_v4()));
        let root = std::env::temp_dir().join(format!("waku-attachments-{}", Uuid::new_v4()));
        fs::create_dir_all(&source_directory).unwrap();
        let source = source_directory.join("design.md");
        fs::write(&source, "daemon-owned").unwrap();
        let store = AttachmentStore::new(root.clone());

        let stored = store.import_path(&source).unwrap();

        assert_eq!(stored.name, "design.md");
        assert!(!stored.is_dir);
        assert_eq!(fs::read_to_string(&stored.path).unwrap(), "daemon-owned");
        assert!(store.import_path(Path::new("relative.txt")).is_err());

        fs::remove_dir_all(source_directory).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn daemon_path_import_follows_symbolic_links() {
        use std::os::unix::fs::symlink;

        let outside = std::env::temp_dir().join(format!("waku-outside-{}", Uuid::new_v4()));
        let root = std::env::temp_dir().join(format!("waku-attachments-{}", Uuid::new_v4()));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        let linked = outside.join("linked.txt");
        symlink(outside.join("secret.txt"), &linked).unwrap();
        let store = AttachmentStore::new(root.clone());

        let stored = store.import_path(&linked).unwrap();

        assert_eq!(stored.name, "secret.txt");
        assert_eq!(fs::read_to_string(stored.path).unwrap(), "secret");
        fs::remove_dir_all(outside).unwrap();
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn retain_removes_only_unreferenced_materializations() {
        let root = std::env::temp_dir().join(format!("waku-attachments-{}", Uuid::new_v4()));
        let store = AttachmentStore::new(root.clone());
        let keep = store
            .import(
                "keep.txt",
                AttachmentUpload::File {
                    data_base64: "a2VlcA==".into(),
                },
            )
            .unwrap();
        let remove = store
            .import(
                "remove.txt",
                AttachmentUpload::File {
                    data_base64: "cmVtb3Zl".into(),
                },
            )
            .unwrap();
        let live = HashSet::from([keep.reference.clone()]);

        assert_eq!(store.retain(&live).unwrap(), 6);
        assert!(keep.path.is_file());
        assert!(!remove.path.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recent_unreferenced_attachments_survive_a_grace_period_sweep() {
        let root = std::env::temp_dir().join(format!("waku-attachments-{}", Uuid::new_v4()));
        let store = AttachmentStore::new(root.clone());
        let recent = store
            .import(
                "recent.txt",
                AttachmentUpload::File {
                    data_base64: "cmVjZW50".into(),
                },
            )
            .unwrap();
        let cutoff = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            store
                .retain_unreferenced_older_than(&HashSet::new(), cutoff)
                .unwrap(),
            0
        );
        assert!(recent.path.exists());

        fs::remove_dir_all(root).unwrap();
    }
}
