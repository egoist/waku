use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, ParentElement, RenderOnce,
    Stateful, StyleRefinement, Styled, Window, div, prelude::*, px,
};

use crate::input::TextInput;
use crate::theme::{Theme, sp};

use super::icon;

/// The one-line text box shell: a fixed-height bordered field around a
/// [`TextInput`], with an optional leading icon and an accent border
/// while focused.
///
/// The embedded input must stay single-line (the default mode) — that is
/// what keeps the text from wrapping and slides overlong content under the
/// clipped viewport instead of growing out of the fixed-height shell.
/// Construction stays at the call site because the entity needs the window;
/// this component owns everything visual.
#[derive(IntoElement)]
pub struct TextField {
    base: Stateful<Div>,
    input: gpui::Entity<TextInput>,
    icon: Option<(&'static str, f32)>,
}

impl TextField {
    pub fn new(id: impl Into<ElementId>, input: gpui::Entity<TextInput>) -> Self {
        Self {
            base: div().id(id),
            input,
            icon: None,
        }
    }

    /// Leading icon, tinted tertiary like the address bar's lock.
    pub fn icon(mut self, path: &'static str, size: f32) -> Self {
        self.icon = Some((path, size));
        self
    }
}

impl Styled for TextField {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for TextField {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl ParentElement for TextField {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for TextField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::current(cx);
        let focused = self.input.read(cx).is_visually_focused(window);
        self.base
            .h(px(28.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(if focused {
                theme.accent
            } else {
                theme.border_strong
            })
            .bg(theme.inset)
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(sp(12.5))
            .line_height(sp(16.0))
            .when_some(self.icon, |element, (path, size)| {
                element.child(icon(path, size, theme.text_tertiary))
            })
            .child(div().min_w_0().flex_1().child(self.input))
    }
}
