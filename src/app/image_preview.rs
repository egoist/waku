//! Window-modal preview for image attachments.
//!
//! Daemon-owned image bytes are retained as GPUI images only while the desktop
//! needs them. Opening a preview therefore changes only in-memory UI state;
//! the frame path never probes the filesystem or performs RPC.

use gpui::{KeyBinding, actions};

use super::*;

actions!(waku_image_preview, [DismissImagePreview]);

const IMAGE_PREVIEW_CONTEXT: &str = "ImagePreview";
const IMAGE_PREVIEW_ANIMATION_DURATION: Duration = Duration::from_millis(140);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "escape",
        DismissImagePreview,
        Some(IMAGE_PREVIEW_CONTEXT),
    )]);
}

pub(super) struct ImagePreviewState {
    image: Arc<gpui::Image>,
    name: SharedString,
    focus: FocusHandle,
    close_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    generation: u64,
}

pub(super) fn attachment_menu_items(path: PathBuf, can_reveal: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::new(tr!("common.reveal_in_finder"), move |_, cx| {
            crate::platform::reveal_in_file_manager(&path, cx);
        })
        .icon("icons/folder.svg")
        .disabled(!can_reveal),
    ]
}

impl Waku {
    /// Resolve one daemon-owned image for a visible row. Frames consult only
    /// in-memory state; the first miss starts a deduplicated background RPC and
    /// a later notification lets GPUI render the returned bytes from memory.
    pub(super) fn image_for_reference(
        &self,
        reference: &str,
        daemon_path: Option<&Path>,
        name: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<gpui::Image>> {
        let attachment_reference =
            reference.starts_with(waku_protocol::attachments::ATTACHMENT_SCHEME);
        if !waku_protocol::blob::is_reference(reference) && !attachment_reference {
            return None;
        }
        if let Some(state) = self.remote_images.borrow().get(reference) {
            return match state {
                RemoteImageState::Ready(image) => Some(image.clone()),
                RemoteImageState::Loading | RemoteImageState::Unavailable => None,
            };
        }

        let Some(format) = name
            .and_then(image_format_for_name)
            .or_else(|| image_format_for_name(reference))
        else {
            self.remote_images
                .borrow_mut()
                .insert(reference.to_owned(), RemoteImageState::Unavailable);
            return None;
        };

        self.remote_images
            .borrow_mut()
            .insert(reference.to_owned(), RemoteImageState::Loading);
        let cache_key = reference.to_owned();
        let fetch_reference = cache_key.clone();
        let daemon_path = daemon_path.map(Path::to_path_buf);
        let daemon = self.daemon.clone();
        cx.spawn(async move |waku, cx| {
            let image = cx
                .background_executor()
                .spawn(async move {
                    waku_client::persistence::read_remote_reference(
                        &fetch_reference,
                        daemon_path.as_deref(),
                        &daemon,
                    )
                    .map(|bytes| Arc::new(gpui::Image::from_bytes(format, bytes)))
                })
                .await;
            let _ = waku.update(cx, |waku, cx| {
                waku.remote_images.borrow_mut().insert(
                    cache_key,
                    image.map_or(RemoteImageState::Unavailable, RemoteImageState::Ready),
                );
                cx.notify();
            });
        })
        .detach();
        None
    }

    pub(super) fn open_image_preview(
        &mut self,
        image: Arc<gpui::Image>,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
        let generation = self.image_preview_generation;
        let focus = cx.focus_handle();
        self.image_preview = Some(ImagePreviewState {
            image,
            name,
            focus: focus.clone(),
            close_focus: cx.focus_handle(),
            previous_focus: window.focused(cx),
            generation,
        });

        // The preview is deferred onto GPUI's overlay plane. Wait until that
        // subtree has joined the dispatch tree before focusing it, so Escape
        // is reliable on the first key press.
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let mut should_focus = false;
                let _ = weak.update(cx, |this, _| {
                    should_focus = this
                        .image_preview
                        .as_ref()
                        .is_some_and(|preview| preview.generation == generation);
                });
                if should_focus {
                    window.focus(&focus, cx);
                }
            });
        });
        cx.notify();
    }

    fn close_image_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.image_preview.take() else {
            return;
        };
        self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
        if let Some(previous_focus) = preview.previous_focus {
            window.focus(&previous_focus, cx);
        } else {
            let focus = self.composer_focus(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub(super) fn render_image_preview(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let preview = self.image_preview.as_ref()?;
        let theme = Theme::current(cx);
        let image_source = preview.image.clone();
        let name = preview.name.clone();
        let focus = preview.focus.clone();
        let close_focus = preview.close_focus.clone();
        let generation = preview.generation;

        let close = div()
            .id("image-preview-close")
            .absolute()
            .top(px(14.0))
            .right(px(14.0))
            .track_focus(&close_focus)
            .tab_index(0)
            .size(px(32.0))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.48))
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(gpui::hsla(0.0, 0.0, 0.0, 0.66)))
            .active(|style| style.opacity(0.8))
            .tooltip(Tooltip::text(tr!("attachments.close_preview")))
            .child(icon("icons/x.svg", 13.0, gpui::white()))
            .on_click(cx.listener(|this, _, window, cx| {
                this.close_image_preview(window, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.close_image_preview(window, cx);
                    cx.stop_propagation();
                }
            }));

        let unavailable_color = gpui::white().opacity(0.78);
        let image = div()
            .size_full()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .child(
                img(image_source)
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .with_fallback(move || {
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(sp(12.5))
                            .text_color(unavailable_color)
                            .child(icon("icons/alert.svg", 18.0, unavailable_color))
                            .child(tr_cow!("attachments.preview_unavailable"))
                            .into_any_element()
                    }),
            );
        let content = div()
            .relative()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(div().w_full().flex_1().min_h_0().child(image))
            .child(
                div()
                    .max_w(px(560.0))
                    .px(px(11.0))
                    .py(px(5.0))
                    .rounded_full()
                    .bg(gpui::hsla(0.0, 0.0, 0.0, 0.48))
                    .text_size(sp(12.5))
                    .text_color(gpui::white().opacity(0.9))
                    .truncate()
                    .child(name),
            )
            .child(close);

        let layer = div()
            .id(SharedString::from(format!(
                "image-preview-layer-{generation}"
            )))
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&focus)
            .key_context(IMAGE_PREVIEW_CONTEXT)
            .on_action(cx.listener(|this, _: &DismissImagePreview, window, cx| {
                this.close_image_preview(window, cx);
            }))
            .tab_group()
            .tab_stop(false)
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.82))
            .p(px(36.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_image_preview(window, cx);
                }),
            )
            .child(content)
            .with_animation(
                SharedString::from(format!("image-preview-enter-{generation}")),
                Animation::new(IMAGE_PREVIEW_ANIMATION_DURATION).with_easing(ease_out_quint()),
                |element, delta| element.opacity(delta),
            );

        Some(gpui::deferred(layer).with_priority(5).into_any_element())
    }
}

pub(super) fn image_format_for_name(name: &str) -> Option<gpui::ImageFormat> {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(gpui::ImageFormat::Png),
        "jpg" | "jpeg" => Some(gpui::ImageFormat::Jpeg),
        "webp" => Some(gpui::ImageFormat::Webp),
        "gif" => Some(gpui::ImageFormat::Gif),
        "svg" => Some(gpui::ImageFormat::Svg),
        "bmp" => Some(gpui::ImageFormat::Bmp),
        "tif" | "tiff" => Some(gpui::ImageFormat::Tiff),
        "ico" => Some(gpui::ImageFormat::Ico),
        "pnm" | "pbm" | "pgm" | "ppm" => Some(gpui::ImageFormat::Pnm),
        _ => None,
    }
}
