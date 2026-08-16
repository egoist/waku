use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ts_rs::TS;

pub const ATTACHMENT_SCHEME: &str = "waku-attachment:";
pub const MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ATTACHMENT_FILES: usize = 4_096;

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AttachmentUpload {
    File { data_base64: String },
    Directory { entries: Vec<AttachmentUploadEntry> },
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadEntry {
    #[ts(type = "string")]
    pub relative_path: PathBuf,
    pub data_base64: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StoredAttachment {
    pub reference: String,
    #[ts(type = "string")]
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}
