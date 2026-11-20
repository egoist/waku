//! In-app directory browser for projects that live on a remote daemon host.
//!
//! The local "add project" flow asks the operating system for a folder, which
//! only means something when the daemon shares the client's filesystem. When
//! the daemon runs on another machine the picker browses the daemon's own
//! directories over `WorkspaceOperation::BrowseDirectory` and turns the
//! confirmed folder into the same project the local flow creates.

use gpui::{KeyBinding, actions};

use super::*;

actions!(
    waku_remote_directory_picker,
    [
        ConfirmRemoteDirectory,
        DismissRemoteDirectory,
        MoveRemoteDirectoryCursorUp,
        MoveRemoteDirectoryCursorDown,
        MoveRemoteDirectoryCursorToFirst,
        MoveRemoteDirectoryCursorToLast
    ]
);

const DIALOG_CONTEXT: &str = "RemoteDirectoryPicker";
const ROW_HEIGHT: f32 = 28.0;
const VISIBLE_ROWS: f32 = 10.0;
const CARD_WIDTH: f32 = 460.0;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", MoveRemoteDirectoryCursorUp, Some(DIALOG_CONTEXT)),
        KeyBinding::new("down", MoveRemoteDirectoryCursorDown, Some(DIALOG_CONTEXT)),
        KeyBinding::new(
            "home",
            MoveRemoteDirectoryCursorToFirst,
            Some(DIALOG_CONTEXT),
        ),
        KeyBinding::new("end", MoveRemoteDirectoryCursorToLast, Some(DIALOG_CONTEXT)),
        KeyBinding::new(
            "secondary-enter",
            ConfirmRemoteDirectory,
            Some(DIALOG_CONTEXT),
        ),
        KeyBinding::new("escape", DismissRemoteDirectory, Some(DIALOG_CONTEXT)),
    ]);
}

/// One row of the browser: the shortcut to the parent folder, or a
/// subdirectory of the folder being shown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RemoteDirectoryRow {
    Parent { path: PathBuf },
    Directory { name: String, path: PathBuf },
}

/// The browser's own state. The daemon owns the tree; this holds the last
/// listing it sent, the keyboard cursor, and the focus handles the dialog
/// needs.
pub(super) struct RemoteDirectoryPickerState {
    /// The folder being shown, on the daemon host.
    path: Option<PathBuf>,
    home: Option<PathBuf>,
    filesystem_root: Option<PathBuf>,
    /// Shared with the row builder so a frame clones a pointer, not every
    /// path in the listing.
    rows: Rc<Vec<RemoteDirectoryRow>>,
    cursor: usize,
    loading: bool,
    error: Option<String>,
    /// Bumped for every listing request so an answer for a folder the user
    /// already left cannot land.
    generation: u64,
    focus: FocusHandle,
    focus_pending: bool,
    use_focus: FocusHandle,
    home_focus: FocusHandle,
    root_focus: FocusHandle,
    list_state: ListState,
    scrollbar: Rc<ScrollbarState>,
}

/// Which way the keyboard cursor moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteDirectoryCursor {
    Up,
    Down,
    First,
    Last,
}

/// The rows for one listing: the parent shortcut first, then the
/// subdirectories in the order the daemon sent them.
fn remote_directory_rows(
    parent: Option<&Path>,
    directories: Vec<(String, PathBuf)>,
) -> Vec<RemoteDirectoryRow> {
    let mut rows = Vec::with_capacity(directories.len() + 1);
    if let Some(parent) = parent {
        rows.push(RemoteDirectoryRow::Parent {
            path: parent.to_path_buf(),
        });
    }
    rows.extend(
        directories
            .into_iter()
            .map(|(name, path)| RemoteDirectoryRow::Directory { name, path }),
    );
    rows
}

/// Where the cursor lands for one movement. `None` means there is nothing to
/// move to.
fn next_remote_directory_cursor(
    cursor: usize,
    len: usize,
    movement: RemoteDirectoryCursor,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    let cursor = cursor.min(last);
    Some(match movement {
        RemoteDirectoryCursor::Up => {
            if cursor == 0 {
                last
            } else {
                cursor - 1
            }
        }
        RemoteDirectoryCursor::Down => {
            if cursor == last {
                0
            } else {
                cursor + 1
            }
        }
        RemoteDirectoryCursor::First => 0,
        RemoteDirectoryCursor::Last => last,
    })
}

impl Waku {
    /// Open the browser for a project on the daemon host. The first listing
    /// starts at the daemon's home directory.
    pub(super) fn open_remote_directory_picker(&mut self, cx: &mut Context<Self>) {
        if self.remote_directory_picker.is_some() {
            return;
        }
        self.remote_directory_picker = Some(RemoteDirectoryPickerState {
            path: None,
            home: None,
            filesystem_root: None,
            rows: Rc::new(Vec::new()),
            cursor: 0,
            loading: true,
            error: None,
            generation: 0,
            focus: cx.focus_handle(),
            focus_pending: true,
            use_focus: cx.focus_handle(),
            home_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            list_state: ListState::new(0, ListAlignment::Top, px(ROW_HEIGHT * VISIBLE_ROWS)),
            scrollbar: ScrollbarState::new(),
        });
        self.browse_remote_directory(None, cx);
    }

    /// Ask the daemon for one directory. The request runs on the background
    /// executor; the generation counter drops answers that a newer request has
    /// already superseded.
    fn browse_remote_directory(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(picker) = self.remote_directory_picker.as_mut() else {
            return;
        };
        picker.generation += 1;
        let generation = picker.generation;
        picker.loading = true;
        picker.error = None;
        let workspace_client = waku_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |waku, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let operation = waku_client::WorkspaceOperation::BrowseDirectory { path };
                    match workspace_client.request(operation) {
                        Ok(waku_client::WorkspaceResult::Directory {
                            path,
                            parent,
                            home,
                            filesystem_root,
                            entries,
                        }) => {
                            let directories = entries
                                .into_iter()
                                .filter(|entry| entry.is_dir)
                                .map(|entry| (entry.name, entry.absolute_path))
                                .collect();
                            Ok((path, parent, home, filesystem_root, directories))
                        }
                        Ok(_) => {
                            Err("the daemon returned an invalid directory response".to_owned())
                        }
                        Err(error) => Err(error.to_string()),
                    }
                })
                .await;
            let _ = waku.update(cx, |waku, cx| {
                let Some(picker) = waku.remote_directory_picker.as_mut() else {
                    return;
                };
                if picker.generation != generation {
                    return;
                }
                picker.loading = false;
                match result {
                    Ok((path, parent, home, filesystem_root, directories)) => {
                        picker.rows =
                            Rc::new(remote_directory_rows(parent.as_deref(), directories));
                        picker.path = Some(path);
                        picker.home = Some(home);
                        picker.filesystem_root = Some(filesystem_root);
                        picker.cursor = 0;
                        picker
                            .list_state
                            .reset_with_uniform_height(picker.rows.len(), px(ROW_HEIGHT));
                    }
                    Err(error) => picker.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Move the keyboard cursor. The rows are the only navigable content, so
    /// the cursor wraps around them.
    fn move_remote_directory_cursor(
        &mut self,
        movement: RemoteDirectoryCursor,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.remote_directory_picker.as_mut() else {
            return;
        };
        let Some(next) = next_remote_directory_cursor(picker.cursor, picker.rows.len(), movement)
        else {
            return;
        };
        picker.cursor = next;
        picker.list_state.scroll_to_reveal_item(next);
        cx.notify();
    }

    /// Open the highlighted row. Both rows name a folder to list: the parent
    /// shortcut goes up one level, a directory replaces the listing.
    fn open_remote_directory_entry(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.remote_directory_picker.as_ref() else {
            return;
        };
        let Some(path) = picker.rows.get(picker.cursor).map(|row| match row {
            RemoteDirectoryRow::Parent { path } => path.clone(),
            RemoteDirectoryRow::Directory { path, .. } => path.clone(),
        }) else {
            return;
        };
        self.browse_remote_directory(Some(path), cx);
    }

    /// List the daemon host's home directory.
    fn browse_remote_home(&mut self, cx: &mut Context<Self>) {
        let Some(home) = self
            .remote_directory_picker
            .as_ref()
            .and_then(|picker| picker.home.clone())
        else {
            return;
        };
        self.browse_remote_directory(Some(home), cx);
    }

    /// List the daemon host's filesystem root.
    fn browse_remote_root(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .remote_directory_picker
            .as_ref()
            .and_then(|picker| picker.filesystem_root.clone())
        else {
            return;
        };
        self.browse_remote_directory(Some(root), cx);
    }

    /// Confirm the folder being shown and add it as a project. This mirrors
    /// the local flow: an existing project is selected, a new one is added and
    /// opened.
    fn confirm_remote_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .remote_directory_picker
            .as_ref()
            .and_then(|picker| picker.path.clone())
        else {
            return;
        };
        self.remote_directory_picker = None;
        let existing = self
            .state
            .projects
            .iter()
            .find(|project| project.path == path)
            .map(|project| project.id);
        if let Some(project_id) = existing {
            self.select_project(project_id, cx);
        } else {
            let project = Project::from_path(path);
            let project_id = project.id;
            self.state.projects.push(project);
            self.analytics.track(crate::analytics::Event::ProjectAdded);
            self.create_session_for(project_id, self.state.last_provider, cx);
            // The daemon owns the project list, so the new project is handed
            // over right away rather than at the next autosave.
            self.save();
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn dismiss_remote_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.remote_directory_picker.take().is_none() {
            return;
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Draw the browser as a modal over the app. The dialog joins the
    /// dispatch tree only after it has drawn, so the focus it takes is
    /// deferred by two frames.
    pub(super) fn render_remote_directory_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self
            .remote_directory_picker
            .as_ref()
            .is_some_and(|picker| picker.focus_pending)
        {
            let focus = self
                .remote_directory_picker
                .as_ref()
                .map(|picker| picker.focus.clone());
            if let Some(picker) = self.remote_directory_picker.as_mut() {
                picker.focus_pending = false;
            }
            if let Some(focus) = focus {
                window.on_next_frame(move |window, _| {
                    window.on_next_frame(move |window, cx| window.focus(&focus, cx));
                });
            }
        }
        let picker = self.remote_directory_picker.as_ref()?;
        let theme = Theme::current(cx);
        let weak = cx.entity().downgrade();
        let rows = picker.rows.clone();
        let cursor = picker.cursor;
        let loading = picker.loading;
        let error = picker.error.clone();
        let list_state = picker.list_state.clone();
        let scrollbar = picker.scrollbar.clone();
        let list_focus = picker.focus.clone();
        let use_focus = picker.use_focus.clone();
        let home_focus = picker.home_focus.clone();
        let root_focus = picker.root_focus.clone();
        let path_label = picker.path.as_ref().map(|path| path.display().to_string());
        let use_enabled = picker.path.is_some();
        let home_enabled = picker.home.is_some();
        let root_enabled = picker.filesystem_root.is_some();
        let has_error = picker.error.is_some();

        let body: AnyElement = if rows.is_empty() {
            div()
                .h(px(VISIBLE_ROWS * ROW_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .text_size(sp(13.0))
                .text_color(theme.text_secondary)
                .child(if loading {
                    tr!("remote_picker.loading")
                } else if has_error {
                    // The error line below the list carries the message, so
                    // the empty folder text would only contradict it.
                    String::new()
                } else {
                    tr!("remote_picker.empty")
                })
                .into_any_element()
        } else {
            let row_weak = weak.clone();
            div()
                .h(px(VISIBLE_ROWS * ROW_HEIGHT))
                .flex_none()
                .relative()
                .child(
                    div().size_full().child(
                        list(list_state.clone(), move |index, _window, _cx| {
                            let Some(row) = rows.get(index) else {
                                return div().into_any_element();
                            };
                            remote_directory_row(
                                index,
                                row,
                                index == cursor,
                                &theme,
                                row_weak.clone(),
                            )
                        })
                        .size_full(),
                    ),
                )
                .child(scrollbar::vertical(&list_state, &scrollbar))
                .into_any_element()
        };

        let use_folder = remote_directory_button(
            "remote-directory-picker-use",
            &use_focus,
            tr!("remote_picker.use_folder"),
            true,
            use_enabled,
            RemoteDirectoryButton::Use,
            weak.clone(),
            &theme,
        );
        let home = remote_directory_button(
            "remote-directory-picker-home",
            &home_focus,
            tr!("remote_picker.home"),
            false,
            home_enabled,
            RemoteDirectoryButton::Home,
            weak.clone(),
            &theme,
        );
        let root = remote_directory_button(
            "remote-directory-picker-root",
            &root_focus,
            tr!("remote_picker.root"),
            false,
            root_enabled,
            RemoteDirectoryButton::Root,
            weak.clone(),
            &theme,
        );

        let key_weak = weak.clone();
        let card = div()
            .id("remote-directory-picker-card")
            .key_context(DIALOG_CONTEXT)
            .on_action(cx.listener(|waku, _: &ConfirmRemoteDirectory, window, cx| {
                waku.confirm_remote_directory(window, cx)
            }))
            .on_action(cx.listener(|waku, _: &DismissRemoteDirectory, window, cx| {
                waku.dismiss_remote_directory(window, cx)
            }))
            .on_action(cx.listener(|waku, _: &MoveRemoteDirectoryCursorUp, _, cx| {
                waku.move_remote_directory_cursor(RemoteDirectoryCursor::Up, cx)
            }))
            .on_action(
                cx.listener(|waku, _: &MoveRemoteDirectoryCursorDown, _, cx| {
                    waku.move_remote_directory_cursor(RemoteDirectoryCursor::Down, cx)
                }),
            )
            .on_action(
                cx.listener(|waku, _: &MoveRemoteDirectoryCursorToFirst, _, cx| {
                    waku.move_remote_directory_cursor(RemoteDirectoryCursor::First, cx)
                }),
            )
            .on_action(
                cx.listener(|waku, _: &MoveRemoteDirectoryCursorToLast, _, cx| {
                    waku.move_remote_directory_cursor(RemoteDirectoryCursor::Last, cx)
                }),
            )
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                // `enter` opens the highlighted folder. A focused button stops
                // the event before it reaches the card, so activating a button
                // never also opens a folder.
                if !event.keystroke.modifiers.modified() && event.keystroke.key.as_str() == "enter"
                {
                    cx.stop_propagation();
                    let _ = key_weak.update(cx, |waku, cx| waku.open_remote_directory_entry(cx));
                }
            })
            .tab_group()
            .tab_stop(false)
            .w_full()
            .max_w(px(CARD_WIDTH))
            .overflow_hidden()
            .rounded(px(18.0))
            .bg(theme.composer)
            .shadow_xl()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(48.0))
                    .px(px(16.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .text_size(sp(14.0))
                    .text_color(theme.text)
                    .child(icon("icons/folder.svg", 15.0, theme.text))
                    .child(div().min_w_0().truncate().child(tr!("remote_picker.title"))),
            )
            .child(
                div()
                    .px(px(16.0))
                    .pb(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(sp(12.5))
                    .text_color(theme.text_secondary)
                    .when(loading, |row| {
                        row.child(motion::spin(icon(
                            "icons/loader-circle.svg",
                            13.0,
                            theme.text_secondary,
                        )))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(path_label.unwrap_or_default()),
                    ),
            )
            .child(
                div()
                    .id("remote-directory-picker-list")
                    .track_focus(&list_focus)
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .rounded(px(8.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .child(body),
            )
            .when_some(error, |card, error| {
                card.child(
                    div()
                        .px(px(16.0))
                        .py(px(8.0))
                        .text_size(sp(12.5))
                        .line_height(sp(16.0))
                        .text_color(theme.danger)
                        .child(tr!("remote_picker.failed", error = error)),
                )
            })
            .child(
                div()
                    .p(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(use_folder)
                    .child(home)
                    .child(root),
            );

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("remote-directory-picker-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|waku, _, window, cx| waku.dismiss_remote_directory(window, cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
    }
}

/// The buttons under the browser. Each one is reachable by mouse and by
/// keyboard, and each pairs its label with an icon so the meaning does not
/// rest on color alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteDirectoryButton {
    Use,
    Home,
    Root,
}

#[allow(clippy::too_many_arguments)]
fn remote_directory_button(
    id: &'static str,
    focus: &FocusHandle,
    label: String,
    primary: bool,
    enabled: bool,
    action: RemoteDirectoryButton,
    weak: WeakEntity<Waku>,
    theme: &Theme,
) -> Stateful<Div> {
    let foreground = if !enabled {
        theme.text_ghost
    } else if primary {
        theme.text
    } else {
        theme.text_secondary
    };
    let click_weak = weak.clone();
    let key_weak = weak;
    div()
        .id(id)
        .track_focus(focus)
        .when(enabled, |button| button.tab_index(0))
        .h(px(30.0))
        .px(px(10.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .cursor_default()
        .text_size(sp(13.0))
        .text_color(foreground)
        .focus_visible(|style| style.border_1().border_color(theme.accent))
        .when(primary && enabled, |button| button.bg(theme.overlay_strong))
        .when(enabled, |button| {
            button.hover(|style| style.bg(theme.overlay))
        })
        .child(icon(
            match action {
                RemoteDirectoryButton::Use => "icons/check.svg",
                RemoteDirectoryButton::Home => "icons/folder-open.svg",
                RemoteDirectoryButton::Root => "icons/server.svg",
            },
            13.0,
            foreground,
        ))
        .child(div().min_w_0().truncate().child(label))
        .when(enabled, |button| {
            button
                .on_click(move |_, window, cx| {
                    let _ = click_weak.update(cx, |waku, cx| match action {
                        RemoteDirectoryButton::Use => waku.confirm_remote_directory(window, cx),
                        RemoteDirectoryButton::Home => waku.browse_remote_home(cx),
                        RemoteDirectoryButton::Root => waku.browse_remote_root(cx),
                    });
                })
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        cx.stop_propagation();
                        let _ = key_weak.update(cx, |waku, cx| match action {
                            RemoteDirectoryButton::Use => waku.confirm_remote_directory(window, cx),
                            RemoteDirectoryButton::Home => waku.browse_remote_home(cx),
                            RemoteDirectoryButton::Root => waku.browse_remote_root(cx),
                        });
                    }
                })
        })
}

/// One row of the browser: the parent shortcut, or a subdirectory. The row is
/// clickable, and the highlighted row is what the keyboard opens.
fn remote_directory_row(
    index: usize,
    row: &RemoteDirectoryRow,
    highlighted: bool,
    theme: &Theme,
    weak: WeakEntity<Waku>,
) -> AnyElement {
    let (icon_path, name, path) = match row {
        RemoteDirectoryRow::Parent { path } => (
            "icons/arrow-up.svg",
            tr!("remote_picker.parent"),
            path.clone(),
        ),
        RemoteDirectoryRow::Directory { name, path } => {
            ("icons/folder.svg", name.clone(), path.clone())
        }
    };
    let click_weak = weak;
    div()
        .id(("remote-directory-row", index))
        .h(px(ROW_HEIGHT))
        .w_full()
        .px(px(8.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .cursor_default()
        .text_size(sp(13.5))
        .text_color(theme.text)
        .when(highlighted, |row| row.bg(theme.overlay_strong))
        .child(icon(
            icon_path,
            14.0,
            if highlighted {
                theme.text
            } else {
                theme.text_secondary
            },
        ))
        .child(div().min_w_0().flex_1().truncate().child(name))
        .on_click(move |_, _, cx| {
            let _ = click_weak.update(cx, |waku, cx| {
                waku.browse_remote_directory(Some(path.clone()), cx)
            });
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_start_with_the_parent_shortcut() {
        let rows = remote_directory_rows(
            Some(Path::new("/home/user")),
            vec![("src".to_owned(), PathBuf::from("/home/user/src"))],
        );
        assert_eq!(
            rows,
            vec![
                RemoteDirectoryRow::Parent {
                    path: PathBuf::from("/home/user"),
                },
                RemoteDirectoryRow::Directory {
                    name: "src".to_owned(),
                    path: PathBuf::from("/home/user/src"),
                },
            ]
        );
    }

    #[test]
    fn rows_omit_the_parent_shortcut_at_the_filesystem_root() {
        assert!(remote_directory_rows(None, vec![]).is_empty());
    }

    #[test]
    fn the_cursor_wraps_around_the_rows() {
        assert_eq!(
            next_remote_directory_cursor(0, 3, RemoteDirectoryCursor::Up),
            Some(2)
        );
        assert_eq!(
            next_remote_directory_cursor(2, 3, RemoteDirectoryCursor::Down),
            Some(0)
        );
        assert_eq!(
            next_remote_directory_cursor(1, 3, RemoteDirectoryCursor::First),
            Some(0)
        );
        assert_eq!(
            next_remote_directory_cursor(1, 3, RemoteDirectoryCursor::Last),
            Some(2)
        );
    }

    #[test]
    fn the_cursor_stays_put_when_there_is_nothing_to_move_to() {
        assert_eq!(
            next_remote_directory_cursor(0, 0, RemoteDirectoryCursor::Down),
            None
        );
    }
}
