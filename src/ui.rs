use gpui::{
    App, Div, ElementId, Hsla, InteractiveElement, Interactivity, PathBuilder, RenderOnce,
    SharedString, Stateful, StyleRefinement, Styled, Svg, Window, canvas, div, point, prelude::*,
    px, rgb, svg,
};
use gpui_component::{Selectable, menu::DropdownMenu};

use crate::model::{ActivityKind, ProviderKind, SessionStatus};
use crate::theme::Theme;

/// A monochrome icon from the embedded set, tinted via text color.
pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(color)
}

/// Brand hue for each provider's official mark.
pub fn provider_color(theme: &Theme, provider: ProviderKind) -> Hsla {
    match provider {
        ProviderKind::Amp => rgb(0xF34E3F).into(),
        ProviderKind::Claude => rgb(0xD97757).into(),
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
        ProviderKind::OpenCode => "icons/provider-opencode.svg",
        ProviderKind::Grok => "icons/provider-grok.svg",
        ProviderKind::Pi => "icons/provider-pi.svg",
    }
}

pub fn status_color(theme: &Theme, status: SessionStatus) -> Hsla {
    match status {
        SessionStatus::Idle => theme.text_ghost,
        SessionStatus::Connecting | SessionStatus::Working => theme.accent,
        SessionStatus::Waiting => theme.warning,
        SessionStatus::Failed => theme.danger,
    }
}

pub fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Ready",
        SessionStatus::Connecting => "Connecting",
        SessionStatus::Working => "Working",
        SessionStatus::Waiting => "Needs input",
        SessionStatus::Failed => "Failed",
    }
}

pub fn activity_icon(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Reasoning => "icons/sparkle.svg",
        ActivityKind::Command => "icons/terminal.svg",
        ActivityKind::FileChange => "icons/pencil.svg",
        ActivityKind::Search => "icons/search.svg",
        ActivityKind::Plan => "icons/list.svg",
        ActivityKind::Tool => "icons/wrench.svg",
    }
}

pub fn activity_noun(kind: ActivityKind) -> (&'static str, &'static str) {
    match kind {
        ActivityKind::Reasoning => ("thought", "thoughts"),
        ActivityKind::Command => ("command", "commands"),
        ActivityKind::FileChange => ("file edit", "file edits"),
        ActivityKind::Search => ("search", "searches"),
        ActivityKind::Plan => ("plan step", "plan steps"),
        ActivityKind::Tool => ("tool call", "tool calls"),
    }
}

/// A compact composer chip that can act as a `gpui-component` dropdown-menu
/// trigger while keeping Waku's own visual language. The library only asks
/// its triggers to be styled, selectable, interactive elements — `selected`
/// is driven by the menu's open state, which we render as a soft fill.
#[derive(IntoElement)]
pub struct MenuChip {
    base: Stateful<Div>,
    icon: Option<(&'static str, Hsla)>,
    label: SharedString,
    caret: bool,
    outlined: bool,
    selected: bool,
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
        }
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

impl Selectable for MenuChip {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl DropdownMenu for MenuChip {}

impl RenderOnce for MenuChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::current(cx);
        self.base
            .h(if self.outlined { px(30.0) } else { px(24.0) })
            .px(if self.outlined { px(10.0) } else { px(7.0) })
            .rounded(if self.outlined { px(7.0) } else { px(6.0) })
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(11.5))
            .line_height(px(14.0))
            .cursor_default()
            .when(self.outlined, |element| {
                element
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
            })
            .when(self.selected, |element| element.bg(theme.overlay))
            .hover(|element| element.bg(theme.overlay))
            .when_some(self.icon, |element, (path, color)| {
                element.child(icon(path, 10.5, color))
            })
            .child(div().text_color(theme.text_secondary).child(self.label))
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

impl Selectable for ProjectNameSelector {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl DropdownMenu for ProjectNameSelector {}

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
            .cursor_default()
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

        let mut paths = vec![
            "icons/panel-left.svg",
            "icons/plus.svg",
            "icons/arrow-left.svg",
            "icons/arrow-right.svg",
            "icons/arrow-up.svg",
            "icons/stop.svg",
            "icons/check.svg",
            "icons/copy.svg",
            "icons/rewind.svg",
            "icons/fork.svg",
            "icons/git-branch.svg",
            "icons/chevron-down.svg",
            "icons/chevron-right.svg",
            "icons/folder.svg",
            "icons/folder-new.svg",
            "icons/file-diff.svg",
            "icons/globe.svg",
            "icons/alert.svg",
            "icons/lock.svg",
            "icons/lock-open.svg",
            "icons/star.svg",
            "icons/star-filled.svg",
            "icons/sparkle.svg",
            "icons/zap.svg",
            "icons/panel-right.svg",
            "icons/x.svg",
        ];
        for provider in ProviderKind::ALL {
            paths.push(provider_icon(provider));
        }
        for kind in [
            ActivityKind::Reasoning,
            ActivityKind::Command,
            ActivityKind::FileChange,
            ActivityKind::Search,
            ActivityKind::Plan,
            ActivityKind::Tool,
        ] {
            paths.push(activity_icon(kind));
        }
        for path in paths {
            assert!(
                Assets.load(path).unwrap().is_some(),
                "missing embedded icon: {path}"
            );
        }
    }

    #[test]
    fn failed_sessions_are_not_labeled_as_user_stops() {
        assert_eq!(status_label(SessionStatus::Failed), "Failed");
    }
}
