//! Native presentation helpers layered over Waku's headless Computer Use core.

use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;

pub use waku_client::computer_use::*;

pub(crate) fn decode_preview_image_url(image_url: &str) -> anyhow::Result<Arc<gpui::Image>> {
    const PNG_PREFIX: &str = "data:image/png;base64,";
    let encoded = image_url
        .strip_prefix(PNG_PREFIX)
        .ok_or_else(|| anyhow!("Computer Use preview is not a PNG data URL"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("Computer Use preview contains invalid base64")?;
    if bytes.is_empty() {
        bail!("Computer Use preview is empty");
    }
    Ok(Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        bytes,
    )))
}
