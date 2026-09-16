//! Native presentation helpers layered over Waku's headless Computer Use core.

use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;

pub use waku_client::computer_use::*;

pub(crate) fn decode_preview_image_url(
    image_url: &str,
    renderer: gpui::SvgRenderer,
    current_source: Option<u64>,
) -> anyhow::Result<Option<(u64, Arc<gpui::RenderImage>)>> {
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
    let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes);
    let source_id = image.id();
    if current_source == Some(source_id) {
        return Ok(None);
    }
    Ok(Some((source_id, image.to_image_data(renderer)?)))
}

/// A ready-to-paint frame. Retire atlas entries when replaced or dismissed.
pub(crate) struct PreviewImage {
    pub source_id: u64,
    pub image: Arc<gpui::RenderImage>,
    app: gpui::AsyncApp,
}

impl PreviewImage {
    pub fn new(source_id: u64, image: Arc<gpui::RenderImage>, cx: &gpui::App) -> Self {
        Self {
            source_id,
            image,
            app: cx.to_async(),
        }
    }
}

impl Drop for PreviewImage {
    fn drop(&mut self) {
        let image = self.image.clone();
        self.app
            .spawn(async move |cx| {
                cx.update(|cx| cx.defer(move |cx| cx.drop_image(image, None)));
            })
            .detach();
    }
}

pub(crate) struct PreviewFrames<T> {
    generation: u64,
    pub current: Option<T>,
}

impl<T> Default for PreviewFrames<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            current: None,
        }
    }
}

impl<T> PreviewFrames<T> {
    pub fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub fn complete(&mut self, generation: u64, image: Option<T>) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(image) = image else {
            return false;
        };
        self.current = Some(image);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_keeps_the_ready_frame_until_the_latest_decode_succeeds() {
        let mut frames = PreviewFrames::default();
        let first = frames.begin();
        assert!(frames.complete(first, Some("first")));
        let stale = frames.begin();
        let latest = frames.begin();
        assert_eq!(frames.current, Some("first"));
        assert!(!frames.complete(stale, Some("stale")));
        assert!(!frames.complete(latest, None));
        assert_eq!(frames.current, Some("first"));
        let next = frames.begin();
        assert!(frames.complete(next, Some("next")));
        assert!(!frames.complete(latest, Some("late")));
        assert_eq!(frames.current, Some("next"));
    }
}
