use gpui::{
    AnyElement, App, Context, Div, ElementId, Hsla, Img, InteractiveElement, Interactivity,
    KeyDownEvent, ParentElement, PathBuilder, Pixels, RenderOnce, ScrollHandle, SharedString,
    Stateful, StyleRefinement, Styled, Svg, Window, canvas, div, img, point, prelude::*, px, rgb,
    svg,
};

pub mod menu;
pub mod motion;
pub mod scrollbar;
pub mod text_field;
pub mod tooltip;

use crate::model::{ActivityKind, ProviderKind, SessionStatus};
use crate::theme::{RADIUS_DF, RADIUS_LG, Theme};

/// Resolve Waku's semantic icon names onto one Hugeicons stroke family.
/// Provider marks and polychrome file-type logos remain authored assets.
fn icon_path(path: &'static str) -> &'static str {
    if path.starts_with("icons/provider-") || path.starts_with("icons/file-types/") {
        return path;
    }

    match path {
        "icons/alert.svg" => "hugeicons/alert-circle.svg",
        "icons/appearance.svg" => "hugeicons/paint-board.svg",
        "icons/arrow-down.svg" => "hugeicons/arrow-down-01.svg",
        "icons/arrow-left.svg" => "hugeicons/arrow-left-01.svg",
        "icons/arrow-right.svg" => "hugeicons/arrow-right-01.svg",
        "icons/arrow-up.svg" => "hugeicons/arrow-up-01.svg",
        "icons/arrow-up-right.svg" => "hugeicons/arrow-up-right-01.svg",
        "icons/block.svg" => "hugeicons/unavailable.svg",
        "icons/bot.svg" => "hugeicons/robot-01.svg",
        "icons/case-sensitive.svg" => "hugeicons/text-font.svg",
        "icons/chart-column.svg" => "hugeicons/chart-column.svg",
        "icons/check.svg" => "hugeicons/tick-02.svg",
        "icons/changes.svg" => "hugeicons/git-compare.svg",
        "icons/cloud-upload.svg" => "hugeicons/cloud-upload.svg",
        "icons/collapse.svg" => "hugeicons/arrow-shrink.svg",
        "icons/chevron-down.svg" => "hugeicons/arrow-down-01.svg",
        "icons/chevron-right.svg" => "hugeicons/arrow-right-01.svg",
        "icons/chevron-up.svg" => "hugeicons/arrow-up-01.svg",
        "icons/chevrons-up-down.svg" => "hugeicons/arrow-up-down.svg",
        "icons/command.svg" => "hugeicons/command.svg",
        "icons/compose.svg" => "hugeicons/edit-02.svg",
        "icons/copy.svg" => "hugeicons/copy-01.svg",
        "icons/corner-down-right.svg" => "hugeicons/arrow-turn-forward.svg",
        "icons/cursor-spark.svg" => "hugeicons/cursor-magic-selection-02.svg",
        "icons/download.svg" => "hugeicons/download-01.svg",
        "icons/expand.svg" => "hugeicons/arrow-expand.svg",
        "icons/ellipsis.svg" => "hugeicons/more-horizontal.svg",
        "icons/eye.svg" => "hugeicons/view.svg",
        "icons/eye-off.svg" => "hugeicons/view-off.svg",
        "icons/external-link.svg" => "hugeicons/link-square-02.svg",
        "icons/file.svg" => "hugeicons/file-01.svg",
        "icons/folder.svg" => "hugeicons/folder-01.svg",
        "icons/folder-new.svg" => "hugeicons/folder-add.svg",
        "icons/file-bottom-left-arrow.svg" => "hugeicons/file-download.svg",
        "icons/file-diff.svg" => "hugeicons/file-edit.svg",
        "icons/fork.svg" => "hugeicons/git-fork.svg",
        "icons/git-branch.svg" => "hugeicons/git-branch.svg",
        "icons/git-commit-horizontal.svg" => "hugeicons/git-commit.svg",
        "icons/history-back.svg" => "hugeicons/arrow-left-02.svg",
        "icons/history-forward.svg" => "hugeicons/arrow-right-02.svg",
        "icons/globe.svg" => "hugeicons/globe-02.svg",
        "icons/github.svg" => "hugeicons/github.svg",
        "icons/hexagon.svg" => "hugeicons/hexagon.svg",
        "icons/info.svg" => "hugeicons/information-circle.svg",
        "icons/laptop.svg" => "hugeicons/laptop.svg",
        "icons/list.svg" => "hugeicons/task-01.svg",
        "icons/loader-circle.svg" => "hugeicons/loading-03.svg",
        "icons/lock.svg" => "hugeicons/lock.svg",
        "icons/lock-open.svg" => "hugeicons/square-unlock-02.svg",
        "icons/package.svg" => "hugeicons/package.svg",
        "icons/panel-left.svg" => "hugeicons/sidebar-left.svg",
        "icons/panel-right.svg" => "hugeicons/sidebar-right.svg",
        "icons/paperclip.svg" => "hugeicons/attachment-01.svg",
        "icons/pencil.svg" => "hugeicons/edit-02.svg",
        "icons/plus.svg" => "hugeicons/add-01.svg",
        "icons/queue.svg" => "hugeicons/queue-01.svg",
        "icons/regex.svg" => "hugeicons/code.svg",
        "icons/replace.svg" | "icons/replace-all.svg" => "hugeicons/search-replace.svg",
        "icons/rewind.svg" => "hugeicons/undo-02.svg",
        "icons/rotate-cw.svg" => "hugeicons/rotate-clockwise.svg",
        "icons/search.svg" => "hugeicons/search-01.svg",
        "icons/send.svg" => "hugeicons/sent-02.svg",
        "icons/server.svg" => "hugeicons/server-stack-01.svg",
        "icons/settings.svg" => "hugeicons/settings-01.svg",
        "icons/profile.svg" => "hugeicons/user-circle.svg",
        "icons/slash.svg" => "hugeicons/code.svg",
        "icons/sparkle.svg" => "hugeicons/sparkles.svg",
        "icons/star.svg" | "icons/star-filled.svg" => "hugeicons/star.svg",
        "icons/stop.svg" | "icons/stop-filled.svg" => "hugeicons/stop.svg",
        "icons/terminal.svg" | "icons/terminal-square.svg" => "hugeicons/command-line.svg",
        "icons/trash.svg" => "hugeicons/delete-02.svg",
        "icons/whole-word.svg" => "hugeicons/text-font.svg",
        "icons/wrench.svg" => "hugeicons/wrench-01.svg",
        "icons/x.svg" => "hugeicons/cancel-01.svg",
        "icons/zap.svg" => "hugeicons/flash.svg",
        _ => panic!("unmapped native icon: {path}"),
    }
}

/// A monochrome Hugeicons glyph tinted via GPUI text color.
pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(icon_path(path))
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(color)
}

/// A polychrome file icon rendered as an image so the SVG's authored colors
/// are preserved. GPUI's `svg()` element intentionally renders an alpha mask
/// tinted with one text color.
pub fn file_icon(path: &'static str, size: f32) -> Img {
    img(path).w(px(size)).h(px(size)).flex_none()
}

/// A compact ghost icon button: the only button shape outside the composer's
/// bespoke send control.
pub fn icon_button(id: impl Into<ElementId>, path: &'static str, theme: Theme) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(22.0))
        .rounded(px(RADIUS_DF))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|element| element.bg(theme.overlay))
        .active(|element| element.bg(theme.overlay_strong))
        .child(icon(path, 13.0, theme.text_tertiary))
}

/// Keeps a wheel gesture inside a scrollable nested in another scrollable
/// (activity output inside the transcript list, command output inside the
/// background-work page), matching AppKit: while the viewport under the
/// pointer has overflow of its own, the ancestor must not scroll it away.
/// Call from an `on_scroll_wheel` listener. The viewport's own scroll
/// handler registers after user listeners, so it has already consumed the
/// delta when this stops the bubble; a viewport whose content fits keeps
/// chaining so short blocks do not dead-zone the page. Stopping propagation
/// also skips wheel listeners pushed earlier on the same element, so fold
/// any sibling wheel logic into the listener that calls this.
pub fn contain_scroll(handle: &ScrollHandle, cx: &mut App) {
    if handle.max_offset().y > px(0.5) {
        cx.stop_propagation();
    }
}

/// Add conventional mouse and keyboard activation to a focusable element.
pub trait ActivationExt: Sized {
    fn on_activation<E>(
        self,
        cx: &mut Context<E>,
        activate: impl Fn(&mut E, &mut Window, &mut Context<E>) + 'static,
    ) -> Self
    where
        E: 'static;
}

impl ActivationExt for Stateful<Div> {
    fn on_activation<E>(
        self,
        cx: &mut Context<E>,
        activate: impl Fn(&mut E, &mut Window, &mut Context<E>) + 'static,
    ) -> Self
    where
        E: 'static,
    {
        let activate = std::rc::Rc::new(activate);
        let click_activate = activate.clone();
        let key_activate = activate;
        self.on_click(cx.listener(move |this, _, window, cx| {
            click_activate(this, window, cx);
            cx.stop_propagation();
        }))
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            // Bare Enter/Space only. A modified chord belongs to whatever
            // command owns it, so a focused control must not swallow it —
            // this is the guard the hand-rolled settings toggles carried
            // before they moved onto this helper.
            if !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                key_activate(this, window, cx);
                cx.stop_propagation();
            }
        }))
    }
}

/// The shared pill switch used by settings and automation forms.
///
/// `activate` is ignored while `disabled` is true, but the control remains in
/// the tab order so a pending operation does not move focus unexpectedly.
pub fn toggle_switch<E>(
    id: impl Into<ElementId>,
    on: bool,
    disabled: bool,
    theme: Theme,
    cx: &mut Context<E>,
    activate: impl Fn(&mut E, &mut Window, &mut Context<E>) + 'static,
) -> Stateful<Div>
where
    E: 'static,
{
    let base = div()
        .id(id)
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme.ring))
        .w(px(36.0))
        .h(px(20.0))
        .p(px(2.0))
        .flex_none()
        .rounded(px(RADIUS_LG))
        .cursor_pointer()
        .when(disabled, |element| element.opacity(0.55))
        .bg(if on { theme.inverse } else { theme.inset })
        .border_1()
        .border_color(if on {
            theme.inverse
        } else {
            theme.border_strong
        })
        .flex()
        .items_center()
        .when(on, |element| element.justify_end())
        .child(
            div()
                .w(px(14.0))
                .h(px(14.0))
                .rounded(px(RADIUS_LG))
                .bg(if on {
                    theme.on_inverse
                } else {
                    theme.text_tertiary
                }),
        );

    if disabled {
        base
    } else {
        base.on_activation(cx, activate)
    }
}

/// Brand hue for each provider's official mark.
pub fn provider_color(theme: &Theme, provider: ProviderKind) -> Hsla {
    match provider {
        ProviderKind::Amp => rgb(0xF34E3F).into(),
        ProviderKind::Claude => rgb(0xD97757).into(),
        ProviderKind::DeepSeek => rgb(0x4D6BFE).into(),
        ProviderKind::Codex
        | ProviderKind::Cursor
        | ProviderKind::OpenCode
        | ProviderKind::Grok
        | ProviderKind::Pi => {
            if theme.is_dark {
                rgb(0xF3F3F3).into()
            } else {
                rgb(0x34363B).into()
            }
        }
    }
}

/// Recognizable provider marks, matching the model picker vocabulary.
pub fn provider_icon(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Amp => "icons/provider-amp.svg",
        ProviderKind::Claude => "icons/provider-claude.svg",
        ProviderKind::Codex => "icons/provider-openai.svg",
        ProviderKind::Cursor => "icons/provider-cursor.svg",
        ProviderKind::DeepSeek => "icons/provider-deepseek.svg",
        ProviderKind::OpenCode => "icons/provider-opencode.svg",
        ProviderKind::Grok => "icons/provider-grok.svg",
        ProviderKind::Pi => "icons/provider-pi.svg",
    }
}

pub fn status_color(theme: &Theme, status: SessionStatus) -> Hsla {
    match status {
        SessionStatus::Idle => theme.text_ghost,
        SessionStatus::Connecting | SessionStatus::Working => theme.warning,
        SessionStatus::Waiting => theme.warning,
        SessionStatus::Failed => theme.danger,
    }
}

pub fn activity_icon(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Reasoning => "icons/sparkle.svg",
        ActivityKind::Command => "icons/terminal.svg",
        ActivityKind::FileChange => "icons/pencil.svg",
        ActivityKind::FileRead => "icons/file.svg",
        ActivityKind::FileSearch => "icons/search.svg",
        ActivityKind::FileList => "icons/folder.svg",
        ActivityKind::Search => "icons/search.svg",
        ActivityKind::Plan => "icons/list.svg",
        ActivityKind::Tool => "icons/wrench.svg",
    }
}

pub fn activity_noun(kind: ActivityKind) -> (String, String) {
    match kind {
        ActivityKind::Reasoning => (tr!("activity.thought"), tr!("activity.thoughts")),
        ActivityKind::Command => (tr!("activity.command"), tr!("activity.commands")),
        ActivityKind::FileChange => (tr!("activity.file_edit"), tr!("activity.file_edits")),
        ActivityKind::FileRead => (tr!("activity.file_read"), tr!("activity.file_reads")),
        ActivityKind::FileSearch => (tr!("activity.file_search"), tr!("activity.file_searches")),
        ActivityKind::FileList => (tr!("activity.file_list"), tr!("activity.file_lists")),
        ActivityKind::Search => (tr!("activity.search"), tr!("activity.searches")),
        ActivityKind::Plan => (tr!("activity.plan_step"), tr!("activity.plan_steps")),
        ActivityKind::Tool => (tr!("activity.tool_call"), tr!("activity.tool_calls")),
    }
}

/// A compact chip used as a dropdown-menu trigger. `selected` is driven by the
/// menu's open state and renders as a soft fill.
#[derive(IntoElement)]
pub struct MenuChip {
    base: Stateful<Div>,
    icon: Option<(&'static str, Hsla)>,
    label: SharedString,
    caret: bool,
    outlined: bool,
    selected: bool,
    disabled: bool,
    height: Option<Pixels>,
    background: Option<Hsla>,
    full_radius: bool,
}

impl MenuChip {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id),
            icon: None,
            label: SharedString::default(),
            caret: true,
            outlined: false,
            selected: false,
            disabled: false,
            height: None,
            background: None,
            full_radius: false,
        }
    }

    /// Override the chip's fixed height, for rows whose controls share a
    /// different one.
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = Some(height);
        self
    }

    /// Fill behind an outlined chip. The default matches raised cards; a
    /// chip sitting directly on another surface passes that surface here so
    /// it doesn't read as a filled pill.
    pub fn background(mut self, background: Hsla) -> Self {
        self.background = Some(background);
        self
    }

    /// Use the pill radius for compact controls embedded in the composer.
    /// Dropdowns elsewhere keep the standard control radius.
    pub fn full_radius(mut self) -> Self {
        self.full_radius = true;
        self
    }

    pub fn icon(mut self, path: &'static str, color: Hsla) -> Self {
        self.icon = Some((path, color));
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    pub fn outlined(mut self) -> Self {
        self.outlined = true;
        self
    }

    pub fn caret(mut self, caret: bool) -> Self {
        self.caret = caret;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Soft fill marking the chip as the open menu's trigger.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Styled for MenuChip {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for MenuChip {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl ParentElement for MenuChip {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for MenuChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::current(cx);
        self.base
            .h(self
                .height
                .unwrap_or(if self.outlined { px(30.0) } else { px(24.0) }))
            .px(if self.outlined { px(10.0) } else { px(7.0) })
            .rounded(px(if self.full_radius {
                RADIUS_LG
            } else {
                RADIUS_DF
            }))
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(11.5))
            .line_height(px(14.0))
            .when(!self.disabled, |element| element.cursor_pointer())
            .focus_visible(|style| style.border_1().border_color(theme.ring))
            .when(self.outlined, |element| {
                element
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(self.background.unwrap_or(theme.raised))
            })
            .when(self.selected, |element| element.bg(theme.overlay))
            .when(!self.disabled, |element| {
                element.hover(|element| element.bg(theme.overlay))
            })
            .when(self.disabled, |element| {
                element.text_color(theme.text_ghost)
            })
            .when_some(self.icon, |element, (path, color)| {
                element.child(icon(path, 10.5, color))
            })
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_secondary)
                    .child(self.label),
            )
            .when(self.caret, |element| {
                element.child(icon("icons/chevron-down.svg", 9.0, theme.text_ghost))
            })
    }
}

/// An inline, link-like dropdown trigger used for the project name in the
/// empty-state headline.
#[derive(IntoElement)]
pub struct ProjectNameSelector {
    base: Stateful<Div>,
    label: SharedString,
    selected: bool,
}

impl ProjectNameSelector {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            base: div().id(id),
            label: label.into(),
            selected: false,
        }
    }

    /// Emphasised underline while its menu is open.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Styled for ProjectNameSelector {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for ProjectNameSelector {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl ParentElement for ProjectNameSelector {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for ProjectNameSelector {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::current(cx);
        let underline_color = if self.selected {
            theme.text_secondary
        } else {
            theme.text_tertiary
        };

        self.base
            .relative()
            .flex_none()
            .cursor_pointer()
            .focus_visible(|style| style.border_1().border_color(theme.ring))
            .child(self.label)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let y = bounds.origin.y + bounds.size.height - px(0.5);
                        let mut builder =
                            PathBuilder::stroke(px(1.0)).dash_array(&[px(1.0), px(2.0)]);
                        builder.move_to(point(bounds.origin.x, y));
                        builder.line_to(point(bounds.origin.x + bounds.size.width, y));
                        if let Ok(line) = builder.build() {
                            window.paint_path(line, underline_color);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_referenced_icon_is_embedded() {
        use crate::assets::Assets;
        use crate::model::{ActivityKind, ProviderKind};
        use gpui::AssetSource;
        use gpui_hugeicons::HugeiconsAssets;

        let mut paths = vec![
            "icons/alert.svg",
            "icons/appearance.svg",
            "icons/arrow-down.svg",
            "icons/panel-left.svg",
            "icons/paperclip.svg",
            "icons/plus.svg",
            "icons/arrow-left.svg",
            "icons/arrow-right.svg",
            "icons/arrow-up.svg",
            "icons/arrow-up-right.svg",
            "icons/block.svg",
            "icons/bot.svg",
            "icons/case-sensitive.svg",
            "icons/stop.svg",
            "icons/stop-filled.svg",
            "icons/check.svg",
            "icons/changes.svg",
            "icons/cloud-upload.svg",
            "icons/collapse.svg",
            "icons/command.svg",
            "icons/compose.svg",
            "icons/copy.svg",
            "icons/corner-down-right.svg",
            "icons/cursor-spark.svg",
            "icons/download.svg",
            "icons/expand.svg",
            "icons/ellipsis.svg",
            "icons/eye.svg",
            "icons/eye-off.svg",
            "icons/external-link.svg",
            "icons/file.svg",
            "icons/file-bottom-left-arrow.svg",
            "icons/rewind.svg",
            "icons/fork.svg",
            "icons/git-branch.svg",
            "icons/git-commit-horizontal.svg",
            "icons/history-back.svg",
            "icons/history-forward.svg",
            "icons/chart-column.svg",
            "icons/chevron-down.svg",
            "icons/chevron-right.svg",
            "icons/chevron-up.svg",
            "icons/chevrons-up-down.svg",
            "icons/folder.svg",
            "icons/folder-new.svg",
            "icons/laptop.svg",
            "icons/file-diff.svg",
            "icons/globe.svg",
            "icons/github.svg",
            "icons/hexagon.svg",
            "icons/info.svg",
            "icons/list.svg",
            "icons/loader-circle.svg",
            "icons/lock.svg",
            "icons/lock-open.svg",
            "icons/queue.svg",
            "icons/regex.svg",
            "icons/replace.svg",
            "icons/replace-all.svg",
            "icons/search.svg",
            "icons/send.svg",
            "icons/server.svg",
            "icons/settings.svg",
            "icons/slash.svg",
            "icons/star.svg",
            "icons/star-filled.svg",
            "icons/sparkle.svg",
            "icons/zap.svg",
            "icons/panel-right.svg",
            "icons/x.svg",
            "icons/bot.svg",
            "icons/rotate-cw.svg",
            "icons/package.svg",
            "icons/pencil.svg",
            "icons/terminal.svg",
            "icons/terminal-square.svg",
            "icons/trash.svg",
            "icons/whole-word.svg",
            "icons/wrench.svg",
        ];
        for provider in ProviderKind::ALL {
            paths.push(provider_icon(provider));
        }
        for kind in [
            ActivityKind::Reasoning,
            ActivityKind::Command,
            ActivityKind::FileChange,
            ActivityKind::FileRead,
            ActivityKind::FileSearch,
            ActivityKind::FileList,
            ActivityKind::Search,
            ActivityKind::Plan,
            ActivityKind::Tool,
        ] {
            paths.push(activity_icon(kind));
        }
        let assets = HugeiconsAssets::with_fallback(Assets);
        for path in paths {
            let resolved = icon_path(path);
            assert!(
                assets.load(resolved).unwrap().is_some(),
                "missing resolved icon: {path} -> {resolved}"
            );
        }
    }
}
