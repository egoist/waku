//! Modal Git commit/push flow opened from the Environment summary.
//!
//! Git inspection, mutation, and one-shot agent CLI generation all run on the
//! background executor. UI surfaces only paint the cached state below.

use gpui::{KeyBinding, actions};

use super::*;

actions!(
    waku_commit_dialog,
    [ConfirmCommitDialog, DismissCommitDialog]
);

const DIALOG_CONTEXT: &str = "CommitDialog";
const DIALOG_INPUT_CONTEXT: &str = "CommitDialog > TextInput";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "secondary-enter",
            ConfirmCommitDialog,
            Some(DIALOG_INPUT_CONTEXT),
        ),
        KeyBinding::new("secondary-enter", ConfirmCommitDialog, Some(DIALOG_CONTEXT)),
        KeyBinding::new("escape", DismissCommitDialog, Some(DIALOG_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitAction {
    Commit,
    CommitAndPush,
    Push,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitPending {
    Generating(CommitAction),
    Git(CommitAction),
}

pub(super) struct CommitOperationState {
    id: Uuid,
    workspace: PathBuf,
    pending: CommitPending,
}

impl CommitOperationState {
    fn status_label(&self) -> String {
        commit_pending_status_label(self.pending)
    }
}

fn commit_pending_status_label(pending: CommitPending) -> String {
    match pending {
        CommitPending::Generating(_) => tr!("commit.generating_message"),
        CommitPending::Git(CommitAction::Commit) => tr!("commit.committing"),
        CommitPending::Git(CommitAction::CommitAndPush) => {
            tr!("commit.committing_and_pushing")
        }
        CommitPending::Git(CommitAction::Push) => tr!("commit.pushing"),
    }
}

pub(super) struct CommitDialogState {
    id: Uuid,
    workspace: PathBuf,
    invocation: Option<crate::git_commit::AgentInvocation>,
    message: Entity<TextInput>,
    include_unstaged: bool,
    snapshot: crate::git_commit::Snapshot,
    snapshot_loading: bool,
    error: Option<String>,
    include_focus: FocusHandle,
    commit_focus: FocusHandle,
    commit_push_focus: FocusHandle,
    push_focus: FocusHandle,
}

impl CommitDialogState {
    fn can_commit(&self) -> bool {
        !self.snapshot_loading
            && (self.snapshot.has_staged || (self.include_unstaged && self.snapshot.has_unstaged))
    }

    fn can_push(&self) -> bool {
        !self.snapshot_loading && self.snapshot.can_push
    }

    fn displayed_counts(&self) -> (u64, u64) {
        if self.include_unstaged {
            (self.snapshot.additions, self.snapshot.deletions)
        } else {
            (
                self.snapshot.staged_additions,
                self.snapshot.staged_deletions,
            )
        }
    }
}

impl Waku {
    pub(super) fn commit_operation_status_label(&self) -> Option<String> {
        self.commit_operation
            .as_ref()
            .map(CommitOperationState::status_label)
    }

    pub(super) fn open_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.commit_operation.is_some() {
            return;
        }
        let Some((workspace, provider, model, reasoning_effort)) =
            self.selected_session().and_then(|session| {
                Some((
                    self.workspace_path_for_session(session)?.to_path_buf(),
                    session.provider,
                    self.model_for_session(session).map(str::to_owned),
                    session.reasoning_effort.clone(),
                ))
            })
        else {
            self.show_toast(tr!("commit.no_task"));
            cx.notify();
            return;
        };

        let invocation = self
            .provider_probe(provider)
            .and_then(|probe| probe.path.clone())
            .map(|binary| crate::git_commit::AgentInvocation {
                provider,
                binary,
                model,
                reasoning_effort,
            });
        let cached_branch = self
            .visible_branch_snapshot
            .as_ref()
            .filter(|(path, _)| path == &workspace)
            .map(|(_, snapshot)| snapshot);
        let snapshot = crate::git_commit::Snapshot {
            branch: cached_branch
                .and_then(|snapshot| snapshot.display_branch())
                .unwrap_or("HEAD")
                .to_owned(),
            additions: cached_branch.map_or(0, |snapshot| snapshot.additions),
            deletions: cached_branch.map_or(0, |snapshot| snapshot.deletions),
            ..Default::default()
        };
        let message = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line()
                .placeholder(tr!("commit.message_placeholder"))
        });
        let message_focus = message.read(cx).focus();
        let id = Uuid::new_v4();
        self.commit_dialog = Some(CommitDialogState {
            id,
            workspace: workspace.clone(),
            invocation,
            message,
            include_unstaged: true,
            snapshot,
            snapshot_loading: true,
            error: None,
            include_focus: cx.focus_handle(),
            commit_focus: cx.focus_handle(),
            commit_push_focus: cx.focus_handle(),
            push_focus: cx.focus_handle(),
        });
        // Like Waku's other deferred surfaces, the modal joins the dispatch
        // tree only after it has drawn. Focus it two frames later so typing
        // cannot fall through to the composer beneath it.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&message_focus, cx));
        });
        cx.notify();

        let workspace_client = waku_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |waku, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match workspace_client.request(waku_client::WorkspaceOperation::InspectCommit {
                        cwd: workspace.clone(),
                    }) {
                        Ok(waku_client::WorkspaceResult::CommitSnapshot { snapshot }) => {
                            Ok(snapshot)
                        }
                        Ok(_) => Err("the daemon returned an invalid Git response".to_owned()),
                        Err(error) => Err(error.to_string()),
                    }
                })
                .await;
            let _ = waku.update(cx, |waku, cx| {
                let Some(dialog) = waku.commit_dialog.as_mut().filter(|dialog| dialog.id == id)
                else {
                    return;
                };
                dialog.snapshot_loading = false;
                match result {
                    Ok(snapshot) => dialog.snapshot = snapshot,
                    Err(error) => dialog.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.commit_dialog.take().is_none() {
            return;
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn toggle_include_unstaged(&mut self, cx: &mut Context<Self>) {
        if self.commit_operation.is_some() {
            return;
        }
        let Some(dialog) = self.commit_dialog.as_mut() else {
            return;
        };
        dialog.include_unstaged = !dialog.include_unstaged;
        dialog.error = None;
        cx.notify();
    }

    fn request_commit_action(
        &mut self,
        action: CommitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.commit_operation.is_some() {
            return;
        }
        let Some(dialog) = self.commit_dialog.as_ref() else {
            return;
        };
        let enabled = match action {
            CommitAction::Commit | CommitAction::CommitAndPush => dialog.can_commit(),
            CommitAction::Push => dialog.can_push(),
        };
        if !enabled {
            return;
        }

        let id = dialog.id;
        let workspace = dialog.workspace.clone();
        let include_unstaged = dialog.include_unstaged;
        let invocation = dialog.invocation.clone();
        let message = dialog.message.read(cx).content().trim().to_owned();
        let window_handle = window.window_handle();

        if action != CommitAction::Push && message.is_empty() {
            let Some(invocation) = invocation else {
                if let Some(dialog) = self.commit_dialog.as_mut() {
                    dialog.error = Some(tr!("commit.agent_unavailable"));
                }
                cx.notify();
                return;
            };
            if let Some(dialog) = self.commit_dialog.as_mut() {
                dialog.error = None;
                dialog
                    .message
                    .update(cx, |message, _| message.set_read_only(true));
            }
            self.commit_operation = Some(CommitOperationState {
                id,
                workspace: workspace.clone(),
                pending: CommitPending::Generating(action),
            });
            self.spawn_commit_message_generation(
                id,
                action,
                workspace,
                include_unstaged,
                invocation,
                window_handle,
                cx,
            );
            cx.notify();
            return;
        }

        if let Some(dialog) = self.commit_dialog.as_mut() {
            dialog.error = None;
            dialog
                .message
                .update(cx, |message, _| message.set_read_only(true));
        }
        self.commit_operation = Some(CommitOperationState {
            id,
            workspace: workspace.clone(),
            pending: CommitPending::Git(action),
        });
        self.spawn_git_action(
            id,
            action,
            workspace,
            message,
            include_unstaged,
            window_handle,
            cx,
        );
        cx.notify();
    }

    fn spawn_commit_message_generation(
        &mut self,
        id: Uuid,
        action: CommitAction,
        workspace: PathBuf,
        include_unstaged: bool,
        invocation: crate::git_commit::AgentInvocation,
        window_handle: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let workspace_client = waku_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |waku, cx| {
            let generation_workspace = workspace.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    match workspace_client.request(
                        waku_client::WorkspaceOperation::GenerateCommitMessage {
                            cwd: generation_workspace,
                            include_unstaged,
                            invocation,
                        },
                    ) {
                        Ok(waku_client::WorkspaceResult::CommitMessage { message }) => Ok(message),
                        Ok(_) => {
                            Err("the daemon returned an invalid commit message response".into())
                        }
                        Err(error) => Err(error.to_string()),
                    }
                })
                .await;
            let _ = waku.update(cx, |waku, cx| {
                let current = waku.commit_operation.as_ref().is_some_and(|operation| {
                    operation.id == id
                        && operation.workspace == workspace
                        && operation.pending == CommitPending::Generating(action)
                });
                if !current {
                    return;
                }
                match result {
                    Ok(message) => {
                        if let Some(operation) = waku.commit_operation.as_mut() {
                            operation.pending = CommitPending::Git(action);
                        }
                        if let Some(dialog) =
                            waku.commit_dialog.as_mut().filter(|dialog| dialog.id == id)
                        {
                            dialog
                                .message
                                .update(cx, |input, cx| input.set_content(message.clone(), cx));
                        }
                        waku.spawn_git_action(
                            id,
                            action,
                            workspace,
                            message,
                            include_unstaged,
                            window_handle,
                            cx,
                        );
                    }
                    Err(error) => {
                        waku.commit_operation = None;
                        if let Some(dialog) =
                            waku.commit_dialog.as_mut().filter(|dialog| dialog.id == id)
                        {
                            dialog.error = Some(error);
                            dialog
                                .message
                                .update(cx, |message, _| message.set_read_only(false));
                        } else {
                            waku.show_toast(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn spawn_git_action(
        &mut self,
        id: Uuid,
        action: CommitAction,
        workspace: PathBuf,
        message: String,
        include_unstaged: bool,
        window_handle: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let workspace_client = waku_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |waku, cx| {
            let operation_workspace = workspace.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let operation = match action {
                        CommitAction::Commit => waku_client::WorkspaceOperation::Commit {
                            cwd: operation_workspace.clone(),
                            message,
                            include_unstaged,
                            push: false,
                        },
                        CommitAction::CommitAndPush => waku_client::WorkspaceOperation::Commit {
                            cwd: operation_workspace.clone(),
                            message,
                            include_unstaged,
                            push: true,
                        },
                        CommitAction::Push => waku_client::WorkspaceOperation::Push {
                            cwd: operation_workspace.clone(),
                        },
                    };
                    let result = match workspace_client.request(operation) {
                        Ok(waku_client::WorkspaceResult::Ack) => Ok(()),
                        Ok(_) => Err(anyhow::anyhow!(
                            "the daemon returned an invalid Git response"
                        )),
                        Err(error) => Err(error),
                    };
                    let snapshot = result.as_ref().err().and_then(|_| {
                        match workspace_client.request(
                            waku_client::WorkspaceOperation::InspectCommit {
                                cwd: operation_workspace.clone(),
                            },
                        ) {
                            Ok(waku_client::WorkspaceResult::CommitSnapshot { snapshot }) => {
                                Some(snapshot)
                            }
                            _ => None,
                        }
                    });
                    (result.map_err(|error| error.to_string()), snapshot)
                })
                .await;
            let focus = waku.update(cx, |waku, cx| {
                let current = waku.commit_operation.as_ref().is_some_and(|operation| {
                    operation.id == id
                        && operation.workspace == workspace
                        && operation.pending == CommitPending::Git(action)
                });
                if !current {
                    return None;
                }
                let (result, refreshed_snapshot) = result;
                waku.commit_operation = None;
                if waku
                    .selected_workspace_path()
                    .is_some_and(|path| path == workspace)
                {
                    waku.invalidate_workspace_queries(cx);
                } else {
                    waku.branch_snapshots.invalidate(&workspace);
                }
                let focus = match result {
                    Ok(()) => {
                        let dialog_was_open = waku
                            .commit_dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.id == id);
                        if dialog_was_open {
                            waku.commit_dialog = None;
                        }
                        waku.show_success_toast(match action {
                            CommitAction::Commit => tr!("commit.committed"),
                            CommitAction::CommitAndPush => tr!("commit.committed_and_pushed"),
                            CommitAction::Push => tr!("commit.pushed"),
                        });
                        dialog_was_open.then(|| waku.composer_focus(cx))
                    }
                    Err(error) => {
                        if let Some(dialog) =
                            waku.commit_dialog.as_mut().filter(|dialog| dialog.id == id)
                        {
                            dialog.error = Some(error);
                            if let Some(snapshot) = refreshed_snapshot {
                                dialog.snapshot = snapshot;
                                dialog.snapshot_loading = false;
                            }
                            dialog
                                .message
                                .update(cx, |message, _| message.set_read_only(false));
                        } else {
                            waku.show_toast(error);
                        }
                        None
                    }
                };
                cx.notify();
                focus
            });
            if let Ok(Some(focus)) = focus {
                let _ = window_handle.update(cx, |_, window, cx| window.focus(&focus, cx));
            }
        })
        .detach();
    }

    pub(super) fn render_commit_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.commit_dialog.as_ref()?;
        let theme = Theme::current(cx);
        let branch = dialog.snapshot.branch.clone();
        let message = dialog.message.clone();
        let include_unstaged = dialog.include_unstaged;
        let pending = self
            .commit_operation
            .as_ref()
            .filter(|operation| operation.id == dialog.id)
            .map(|operation| operation.pending);
        let include_enabled = pending.is_none();
        let (additions, deletions) = dialog.displayed_counts();
        let can_commit = pending.is_none() && dialog.can_commit();
        let can_push = pending.is_none() && dialog.can_push();
        let pending_status = pending.map(commit_pending_status_label);
        let error = dialog.error.clone();
        let weak = cx.entity().downgrade();

        let include = {
            let click_weak = weak.clone();
            let key_weak = weak.clone();
            div()
                .id("commit-dialog-include-unstaged")
                .track_focus(&dialog.include_focus)
                .when(include_enabled, |row| row.tab_index(0))
                .h(px(44.0))
                .w_full()
                .px(px(12.0))
                .rounded(px(9.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .cursor_default()
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .when(include_enabled, |row| {
                    row.hover(|style| style.bg(theme.overlay))
                })
                .child(
                    div()
                        .size(px(15.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(if include_unstaged {
                            theme.border_strong
                        } else {
                            theme.text_ghost
                        })
                        .bg(if include_unstaged {
                            theme.composer
                        } else {
                            gpui::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(include_unstaged, |checkbox| {
                            checkbox.child(icon("icons/check.svg", 12.0, theme.text))
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_size(sp(14.0))
                        .text_color(if include_enabled {
                            theme.text
                        } else {
                            theme.text_ghost
                        })
                        .child(tr!("commit.include_unstaged")),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(sp(13.5))
                        .font_weight(FontWeight::MEDIUM)
                        .child(
                            div()
                                .text_color(theme.success)
                                .child(format!("+{}", grouped_number(additions))),
                        )
                        .child(
                            div()
                                .text_color(theme.danger)
                                .child(format!("-{}", grouped_number(deletions))),
                        ),
                )
                .when(include_enabled, |row| {
                    row.on_click(move |_, _, cx| {
                        let _ = click_weak.update(cx, |waku, cx| waku.toggle_include_unstaged(cx));
                    })
                    .on_key_down(move |event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            let _ =
                                key_weak.update(cx, |waku, cx| waku.toggle_include_unstaged(cx));
                            cx.stop_propagation();
                        }
                    })
                })
        };

        let commit_active = pending.is_some_and(|pending| match pending {
            CommitPending::Generating(action) | CommitPending::Git(action) => {
                action == CommitAction::Commit
            }
        });
        let commit = render_commit_action_row(
            "commit-dialog-commit",
            &dialog.commit_focus,
            "icons/git-commit-horizontal.svg",
            if commit_active {
                pending_status
                    .clone()
                    .unwrap_or_else(|| tr!("commit.commit"))
            } else {
                tr!("commit.commit")
            },
            can_commit,
            commit_active,
            Some(crate::platform::primary_shortcut("⌘↩", "Ctrl+Enter")),
            CommitAction::Commit,
            weak.clone(),
            &theme,
        );
        let commit_and_push_active = pending.is_some_and(|pending| match pending {
            CommitPending::Generating(action) | CommitPending::Git(action) => {
                action == CommitAction::CommitAndPush
            }
        });
        let commit_and_push = render_commit_action_row(
            "commit-dialog-commit-and-push",
            &dialog.commit_push_focus,
            "icons/cloud-upload.svg",
            if commit_and_push_active {
                pending_status
                    .clone()
                    .unwrap_or_else(|| tr!("commit.commit_and_push"))
            } else {
                tr!("commit.commit_and_push")
            },
            can_commit,
            commit_and_push_active,
            None,
            CommitAction::CommitAndPush,
            weak.clone(),
            &theme,
        );
        let push_active = pending == Some(CommitPending::Git(CommitAction::Push));
        let push = render_commit_action_row(
            "commit-dialog-push",
            &dialog.push_focus,
            "icons/cloud-upload.svg",
            if push_active {
                pending_status.unwrap_or_else(|| tr!("commit.push"))
            } else {
                tr!("commit.push")
            },
            can_push,
            push_active,
            None,
            CommitAction::Push,
            weak,
            &theme,
        );

        let card = div()
            .id("commit-dialog-card")
            .key_context(DIALOG_CONTEXT)
            .on_action(cx.listener(|waku, _: &ConfirmCommitDialog, window, cx| {
                waku.request_commit_action(CommitAction::Commit, window, cx)
            }))
            .on_action(cx.listener(|waku, _: &DismissCommitDialog, window, cx| {
                waku.close_commit_dialog(window, cx)
            }))
            .tab_group()
            .tab_stop(false)
            .w_full()
            .max_w(px(420.0))
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
                    .child(icon("icons/git-branch.svg", 15.0, theme.text))
                    .child(div().min_w_0().truncate().child(branch)),
            )
            .child(
                div()
                    .h(px(112.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .text_size(sp(14.0))
                    .line_height(sp(21.0))
                    .text_color(theme.text)
                    .child(message),
            )
            .child(div().px(px(8.0)).child(include))
            .when_some(error, |card, error| {
                card.child(
                    div()
                        .px(px(20.0))
                        .pb(px(10.0))
                        .text_size(sp(12.5))
                        .line_height(sp(16.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(div().mx(px(8.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .p(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(commit)
                    .child(commit_and_push)
                    .child(push),
            );

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("commit-dialog-layer")
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
                cx.listener(|waku, _, window, cx| waku.close_commit_dialog(window, cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
    }
}

#[allow(clippy::too_many_arguments)]
fn render_commit_action_row(
    id: &'static str,
    focus: &FocusHandle,
    icon_path: &'static str,
    label: String,
    enabled: bool,
    active: bool,
    shortcut: Option<&'static str>,
    action: CommitAction,
    weak: WeakEntity<Waku>,
    theme: &Theme,
) -> Stateful<Div> {
    let foreground = if enabled {
        theme.text
    } else if active {
        theme.text_secondary
    } else {
        theme.text_ghost
    };
    let indicator = if active {
        motion::spin(icon("icons/loader-circle.svg", 15.0, theme.text_secondary))
    } else {
        icon(icon_path, 15.0, foreground).into_any_element()
    };
    let click_weak = weak.clone();
    let key_weak = weak;
    div()
        .id(id)
        .track_focus(focus)
        .when(enabled, |row| row.tab_index(0))
        .h(px(38.0))
        .w_full()
        .px(px(10.0))
        .rounded(px(9.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .cursor_default()
        .text_size(sp(14.0))
        .text_color(foreground)
        .focus_visible(|style| style.border_1().border_color(theme.accent))
        .when(enabled, |row| {
            row.hover(|style| style.bg(theme.overlay_strong))
        })
        .child(indicator)
        .child(div().min_w_0().flex_1().truncate().child(label))
        .when_some(shortcut, |row, shortcut| {
            row.child(
                div()
                    .h(px(22.0))
                    .min_w(px(34.0))
                    .px(px(7.0))
                    .rounded(px(11.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.overlay_strong)
                    .text_size(sp(12.5))
                    .text_color(if enabled {
                        theme.text_secondary
                    } else {
                        theme.text_ghost
                    })
                    .child(shortcut),
            )
        })
        .when(enabled, |row| {
            row.on_click(move |_, window, cx| {
                let _ = click_weak.update(cx, |waku, cx| {
                    waku.request_commit_action(action, window, cx)
                });
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    let _ = key_weak.update(cx, |waku, cx| {
                        waku.request_commit_action(action, window, cx)
                    });
                    cx.stop_propagation();
                }
            })
        })
}

fn grouped_number(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_change_counts() {
        assert_eq!(grouped_number(0), "0");
        assert_eq!(grouped_number(2_849), "2,849");
        assert_eq!(grouped_number(1_234_567), "1,234,567");
    }
}
