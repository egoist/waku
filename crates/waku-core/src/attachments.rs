//! Daemon-owned materialization of client-uploaded attachments.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

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
        let name = safe_name(name)?;
        let id = Uuid::new_v4();
        let reference = format!("{ATTACHMENT_SCHEME}{id}");
        let staging = self.root.join(format!(".{id}.tmp"));
        let destination = self.root.join(id.to_string());
        fs::create_dir_all(&staging)?;
        let target = staging.join(&name);
        let materialized = (|| -> io::Result<bool> {
            match upload {
                AttachmentUpload::File { data_base64 } => {
                    let bytes = decode(&data_base64)?;
                    ensure_size(bytes.len())?;
                    fs::write(&target, bytes)?;
                    Ok(false)
                }
                AttachmentUpload::Directory { entries } => {
                    if entries.len() > MAX_ATTACHMENT_FILES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "attachment directory contains too many files",
                        ));
                    }
                    fs::create_dir_all(&target)?;
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
            }
        })();
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
            reclaimed += directory_size(&entry.path()).unwrap_or_default();
            fs::remove_dir_all(entry.path())?;
        }
        Ok(reclaimed)
    }
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
}
