//! Daemon-owned content-addressed storage for binary task payloads.
//!
//! Screenshots arrive from providers as `data:` URLs. Keeping them inline in
//! the transcript is what made the old single-file state unusable: one
//! computer-use session reached 8 MB, base64 inflates the payload by a third,
//! and every render decoded the string again. Blobs live in files instead, and
//! the transcript keeps only a `waku-blob:` reference.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use base64::Engine as _;

pub use waku_protocol::blob::SCHEME as BLOB_SCHEME;

/// Payloads below this stay inline: a reference plus a file is not worth it for
/// a favicon-sized image, and tests use tiny fixtures.
const MIN_EXTERNALIZED_BYTES: usize = 8 * 1024;

/// Blob file names are `<hash>.<ext>`; the hash is FNV-1a over the bytes, which
/// is enough to deduplicate identical screenshots without pulling in a crypto
/// dependency. A collision would only ever alias two identical-length payloads,
/// and the length is folded into the digest below.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

fn extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "image/tiff" | "image/tif" => "tiff",
        "image/ico" => "ico",
        "image/x-portable-anymap" => "pnm",
        _ => "bin",
    }
}

/// Splits a `data:` URL into its MIME type and decoded bytes.
fn decode_data_url(url: &str) -> Option<(&str, Vec<u8>)> {
    let (header, encoded) = url.split_once(',')?;
    let header = header.strip_prefix("data:")?;
    if !header.contains(";base64") {
        return None;
    }
    let mime_type = header.split(';').next()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    (!bytes.is_empty()).then_some((mime_type, bytes))
}

pub fn is_blob_reference(value: &str) -> bool {
    waku_protocol::blob::is_reference(value)
}

fn resolve_path(root: &Path, reference: &str) -> Option<PathBuf> {
    let name = reference.strip_prefix(BLOB_SCHEME)?;
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return None;
    }
    let shard = name.get(..2)?;
    Some(root.join(shard).join(name))
}

pub struct BlobStore {
    root: PathBuf,
    /// Bytes written since launch, for the storage figure in settings.
    written_bytes: AtomicU64,
}

impl BlobStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            written_bytes: AtomicU64::new(0),
        }
    }

    /// Resolves a `waku-blob:` reference against this daemon store's root.
    pub fn path_for(&self, reference: &str) -> Option<PathBuf> {
        resolve_path(&self.root, reference)
    }

    /// Stores raw binary image bytes and returns a durable blob reference.
    /// Clipboard paste calls this off the UI thread, then resolves the
    /// reference to the real path that providers can read.
    pub fn store_image_bytes(&self, mime_type: &str, bytes: &[u8]) -> io::Result<String> {
        let name = format!(
            "{:016x}.{}",
            fingerprint(bytes),
            extension_for_mime(mime_type)
        );
        self.write_blob(&name, bytes)?;
        Ok(format!("{BLOB_SCHEME}{name}"))
    }

    /// Stores a `data:` URL and returns its reference. Values that are already
    /// references, are not data URLs, or are too small to be worth a file are
    /// returned unchanged.
    pub fn store_data_url(&self, url: &str) -> String {
        if is_blob_reference(url) {
            return url.to_owned();
        }
        let Some((mime_type, bytes)) = decode_data_url(url) else {
            return url.to_owned();
        };
        if bytes.len() < MIN_EXTERNALIZED_BYTES {
            return url.to_owned();
        }
        match self.store_image_bytes(mime_type, &bytes) {
            Ok(reference) => reference,
            Err(_) => url.to_owned(),
        }
    }

    fn write_blob(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let directory = self.root.join(&name[..2]);
        let path = directory.join(name);
        // Content-addressed: an existing file with this name already holds
        // these exact bytes, so identical screenshots cost nothing.
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() as usize == bytes.len()) {
            // A newly-issued reference is a lease even when the content was
            // already present from an abandoned upload. Refresh its timestamp
            // so the grace-period sweep below cannot reclaim it before the
            // client commits the reference to a draft or session row.
            if fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(SystemTime::now()))
                .is_ok()
            {
                return Ok(());
            }
        }
        fs::create_dir_all(&directory)?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, &path)?;
        self.written_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Deletes blobs no longer named by any live reference.
    pub fn retain(&self, live: &std::collections::HashSet<String>) -> io::Result<u64> {
        self.retain_with_cutoff(live, None)
    }

    /// Deletes unreferenced blobs only when their last write predates
    /// `cutoff`. Store and commit are separate RPC/SQLite operations, so a
    /// recent unreferenced file may still be in flight.
    pub fn retain_unreferenced_older_than(
        &self,
        live: &std::collections::HashSet<String>,
        cutoff: SystemTime,
    ) -> io::Result<u64> {
        self.retain_with_cutoff(live, Some(cutoff))
    }

    fn retain_with_cutoff(
        &self,
        live: &std::collections::HashSet<String>,
        cutoff: Option<SystemTime>,
    ) -> io::Result<u64> {
        let mut reclaimed = 0;
        let Ok(shards) = fs::read_dir(&self.root) else {
            return Ok(0);
        };
        for shard in shards.flatten() {
            if !shard.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            for entry in fs::read_dir(shard.path())?.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if live.contains(&format!("{BLOB_SCHEME}{name}")) {
                    continue;
                }
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    // Unknown age is not permission to destroy a payload
                    // during a grace-period sweep. The immediate `retain`
                    // variant preserves its previous best-effort behavior.
                    Err(_) if cutoff.is_some() => continue,
                    Err(_) => {
                        let _ = fs::remove_file(entry.path());
                        continue;
                    }
                };
                if cutoff.is_some_and(|cutoff| {
                    metadata
                        .modified()
                        .map_or(true, |modified| modified >= cutoff)
                }) {
                    continue;
                }
                let size = metadata.len();
                if fs::remove_file(entry.path()).is_ok() {
                    reclaimed += size;
                }
            }
        }
        Ok(reclaimed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn temporary_root() -> PathBuf {
        std::env::temp_dir().join(format!("waku-blobs-{}", uuid::Uuid::new_v4()))
    }

    fn data_url(mime_type: &str, bytes: &[u8]) -> String {
        format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn large_images_move_to_disk_and_resolve_back() {
        let root = temporary_root();
        let store = BlobStore::new(root.clone());
        let bytes = vec![7u8; 64 * 1024];
        let reference = store.store_data_url(&data_url("image/png", &bytes));

        assert!(is_blob_reference(&reference));
        assert!(reference.ends_with(".png"));
        let path = store.path_for(&reference).unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn identical_payloads_share_one_file() {
        let root = temporary_root();
        let store = BlobStore::new(root.clone());
        let url = data_url("image/png", &vec![3u8; 32 * 1024]);

        let first = store.store_data_url(&url);
        let second = store.store_data_url(&url);

        assert_eq!(first, second);
        let shard_count = fs::read_dir(store.path_for(&first).unwrap().parent().unwrap())
            .unwrap()
            .count();
        assert_eq!(shard_count, 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn small_and_non_data_values_stay_inline() {
        let store = BlobStore::new(temporary_root());
        let small = data_url("image/png", b"hello");
        assert_eq!(store.store_data_url(&small), small);
        assert_eq!(
            store.store_data_url("https://example.com/a.png"),
            "https://example.com/a.png"
        );
    }

    #[test]
    fn unreferenced_blobs_are_reclaimed() {
        let root = temporary_root();
        let store = BlobStore::new(root.clone());
        let kept = store.store_data_url(&data_url("image/png", &vec![1u8; 16 * 1024]));
        let dropped = store.store_data_url(&data_url("image/png", &vec![2u8; 16 * 1024]));

        let live = HashSet::from([kept.clone()]);
        let reclaimed = store.retain(&live).unwrap();

        assert_eq!(reclaimed, 16 * 1024);
        assert!(store.path_for(&kept).unwrap().exists());
        assert!(!store.path_for(&dropped).unwrap().exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recent_unreferenced_blobs_survive_a_grace_period_sweep() {
        let root = temporary_root();
        let store = BlobStore::new(root.clone());
        let reference = store.store_data_url(&data_url("image/png", &vec![2u8; 16 * 1024]));
        let cutoff = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            store
                .retain_unreferenced_older_than(&HashSet::new(), cutoff)
                .unwrap(),
            0
        );
        assert!(store.path_for(&reference).unwrap().exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn traversal_references_are_rejected() {
        let store = BlobStore::new(temporary_root());
        assert!(store.path_for("waku-blob:../../etc/passwd").is_none());
        assert!(store.path_for("data:image/png;base64,AAAA").is_none());
    }
}
