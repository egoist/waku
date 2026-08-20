use super::*;

fn retain_runtime_after_cancel(provider: ProviderKind) -> bool {
    // Codex's app-server owns the Computer Use process tree, and Amp offers no
    // interrupt on its stream — stopping it means ending the process. Both
    // resume their native thread on the next prompt.
    !matches!(provider, ProviderKind::Codex | ProviderKind::Amp)
}

impl Waku {
    pub(crate) fn open_task_from_notification(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        self.select_session(session_id, cx);
    }

    pub(super) fn select_project(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        self.state.selected_project = Some(project_id);
        self.create_session_for(project_id, self.state.last_provider, cx);
    }

    pub(super) fn select_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        self.request_session_activation(session_id, SessionActivationTransition::Visit, cx);
    }

    fn request_session_activation(
        &mut self,
        session_id: Uuid,
        transition: SessionActivationTransition,
        cx: &mut Context<Self>,
    ) {
        if !self
            .state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }
        let needs_hydration = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| !session.detail_loaded);
        if needs_hydration {
            self.pending_session_activation = Some(PendingSessionActivation {
                session_id,
                transition,
            });
            // Keep the current transcript visible until the daemon returns the
            // target session, but acknowledge the click immediately in the
            // sidebar instead of making the UI appear unresponsive.
            cx.notify();
            self.ensure_session_loaded(session_id, cx);
            return;
        }
        self.pending_session_activation = None;
        self.finish_session_activation(session_id, transition, cx);
    }

    fn finish_session_activation(
        &mut self,
        session_id: Uuid,
        transition: SessionActivationTransition,
        cx: &mut Context<Self>,
    ) {
        match transition {
            SessionActivationTransition::Visit => self
                .session_navigation
                .visit(self.state.selected_session, session_id),
            SessionActivationTransition::Back { from } => {
                if self.state.selected_session != Some(from)
                    || self.session_navigation.back_target() != Some(session_id)
                {
                    return;
                }
                let _ = self.session_navigation.go_back(from);
            }
            SessionActivationTransition::Forward { from } => {
                if self.state.selected_session != Some(from)
                    || self.session_navigation.forward_target() != Some(session_id)
                {
                    return;
                }
                let _ = self.session_navigation.go_forward(from);
            }
        }
        self.activate_session(session_id, cx);
    }

    /// Loads a session's transcript if startup only fetched its list columns.
    ///
    /// The SQLite query and daemon round trip both stay off the UI thread. The
    /// current selection stays rendered until the requested session is whole.
    pub(super) fn ensure_session_loaded(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let needs_hydration = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| !session.detail_loaded);
        if !needs_hydration || !self.session_hydrations.insert(session_id) {
            return;
        }
        let daemon = self.daemon.clone();
        cx.spawn(async move |waku, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match waku_client::persistence::hydrate_session(&daemon, session_id)? {
                        Some(session) => Ok(session),
                        None => {
                            anyhow::bail!("the task no longer exists")
                        }
                    }
                })
                .await;
            let _ = waku.update(cx, |waku, cx| {
                waku.session_hydrations.remove(&session_id);
                match result {
                    Ok(session) => {
                        let replaced = if let Some(existing) = waku
                            .state
                            .sessions
                            .iter_mut()
                            .find(|existing| existing.id == session_id)
                        {
                            *existing = session;
                            true
                        } else {
                            false
                        };
                        let pending = waku
                            .pending_session_activation
                            .filter(|pending| pending.session_id == session_id);
                        if pending.is_some() {
                            waku.pending_session_activation = None;
                        }
                        if replaced && let Some(pending) = pending {
                            waku.finish_session_activation(session_id, pending.transition, cx);
                        } else if waku.state.selected_session == Some(session_id) {
                            waku.reset_visible_state();
                            waku.reset_transcript_rows(waku.transcript_row_count());
                            waku.refresh_composer_sources(cx);
                        }
                    }
                    Err(error) => {
                        if waku
                            .pending_session_activation
                            .is_some_and(|pending| pending.session_id == session_id)
                        {
                            waku.pending_session_activation = None;
                        }
                        waku.show_toast(tr!("errors.open_session", error = error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn activate_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let session_changed = self.state.selected_session != Some(session_id);
        if session_changed {
            self.capture_and_save_current_composer_draft(cx);
            self.store_selected_right_panel_state();
        }
        self.state.selected_session = Some(session_id);
        if let Some((project_id, provider, model, reasoning_effort, service_tier, context_window)) =
            self.selected_session().map(|session| {
                (
                    session.project_id,
                    session.provider,
                    session.model.clone(),
                    session.reasoning_effort.clone(),
                    session.service_tier.clone(),
                    session.context_window.clone(),
                )
            })
        {
            self.state.selected_project = Some(project_id);
            self.state.last_provider = provider;
            self.state.last_model = model;
            self.state.last_reasoning_effort = reasoning_effort;
            self.state.last_service_tier = service_tier;
            self.state.last_context_window = context_window;
        }
        if self
            .selected_session()
            .is_some_and(|session| !session.has_started())
        {
            self.session_navigation.remember_new_task(session_id);
        }
        if session_changed {
            self.restore_selected_composer_draft(cx);
            self.sync_user_input_answer(cx);
            self.restore_right_panel_state(session_id, cx);
        } else {
            self.ensure_right_panel_terminals(cx);
        }
        self.reset_visible_state();
        if session_changed {
            // Each materialized worktree has its own cache entry. A task that
            // finished while another session was selected could otherwise
            // retain the clean snapshot captured before its agent made edits.
            self.refresh_selected_branch_snapshot(cx);
        }
        self.refresh_composer_sources(cx);
        self.reset_transcript_rows(self.transcript_row_count());
        self.save();
        if self
            .selected_session()
            .is_some_and(AgentSession::has_started)
        {
            self.start_runtime_attachment(session_id, cx);
        }
        cx.notify();
    }

    /// Drops cached answers about the workspace on disk.
    ///
    /// These queries cache to keep `git` and directory walks out of frames, but
    /// nothing tells us when the working tree changes underneath. Rather than
    /// expire on a timer, they are dropped at the moments the answer plausibly
    /// moved — coming back to the window, or a turn finishing.
    pub(super) fn invalidate_workspace_queries(&mut self, cx: &mut Context<Self>) {
        let Some(workspace_path) = self
            .selected_workspace_path()
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        self.branch_snapshots.invalidate(&workspace_path);
        self.refresh_workspace_surfaces(cx);
        self.invalidate_composer_sources(cx);
    }

    pub(super) fn create_session_for(
        &mut self,
        project_id: Uuid,
        provider: ProviderKind,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft_id) = self
            .state
            .sessions
            .iter()
            .find(|session| session.project_id == project_id && !session.has_started())
            .map(|session| session.id)
        {
            self.select_session(draft_id, cx);
            return;
        }
        let session = self.state.new_session(project_id, provider);
        let id = session.id;
        self.state.push_session(session);
        self.select_session(id, cx);
    }

    pub(super) fn select_workspace(&mut self, workspace: SessionWorkspace, cx: &mut Context<Self>) {
        let Some(session) = self.selected_session_mut() else {
            return;
        };
        if session.has_started() || session.is_busy() || session.workspace == workspace {
            return;
        }
        session.workspace = workspace;
        self.save();
        cx.notify();
    }

    pub(super) fn remove_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if self.response_fork_preparations.contains_key(&session_id) {
            self.show_toast(tr!("session.response_fork_in_progress"));
            cx.notify();
            return;
        }
        let Some(index) = self
            .state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return;
        };
        let project_id = self.state.sessions[index].project_id;
        let composer_draft_key =
            crate::persistence::ComposerDraftKey::for_session(&self.state.sessions[index]);
        let projectless = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .is_some_and(Project::is_projectless);
        let project_path = self
            .workspace_path_for_session(&self.state.sessions[index])
            .map(std::path::Path::to_path_buf);
        let was_selected = self.state.selected_session == Some(session_id);
        self.submission_preparations.remove(&session_id);
        self.reset_session_runtime(session_id);
        self.background_work.remove(&session_id);
        self.remove_right_panel_session_state(session_id);
        self.remove_composer_draft(composer_draft_key, cx);
        self.state.sessions.remove(index);
        if let Err(error) = self.store.remove_session(session_id) {
            self.show_toast(tr!("errors.save_local_state", error = error));
        }
        if self
            .pending_session_activation
            .is_some_and(|pending| pending.session_id == session_id)
        {
            self.pending_session_activation = None;
        }
        self.session_navigation.remove(session_id);
        let project_still_used = self
            .state
            .sessions
            .iter()
            .any(|session| session.project_id == project_id);
        if projectless && !project_still_used {
            self.remove_composer_draft(
                crate::persistence::ComposerDraftKey::NewSession(project_id),
                cx,
            );
            self.state
                .projects
                .retain(|project| project.id != project_id);
            if self.state.selected_project == Some(project_id) {
                self.state.selected_project = None;
            }
        }
        if let Some(project_path) = project_path {
            let workspace = waku_client::WorkspaceClient::new(self.daemon.client());
            cx.background_executor()
                .spawn(async move {
                    let _ = workspace.request(waku_client::WorkspaceOperation::DeleteSessionRefs {
                        cwd: project_path,
                        session_id,
                    });
                })
                .detach();
        }
        self.invalidate_checkpoint_refs();

        if was_selected {
            self.state.selected_session = None;
            let next_session = self
                .state
                .sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .max_by_key(|session| session.updated_at)
                .map(|session| session.id);
            if let Some(session_id) = next_session {
                self.select_session(session_id, cx);
            } else if projectless {
                self.create_projectless_session(cx);
            } else {
                self.create_session_for(project_id, self.state.last_provider, cx);
            }
        } else {
            self.save();
            cx.notify();
        }

        // Only now is the row gone, so the sweep can see which blobs are
        // genuinely unreferenced. It reads the database and walks the blob
        // directory, so it stays off the UI thread.
        let sweep = self.store.blob_sweep();
        cx.background_executor()
            .spawn(async move { sweep() })
            .detach();
    }

    pub(super) fn new_session_action(
        &mut self,
        _: &NewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = None;
        if let Some(session_id) = self
            .session_navigation
            .remembered_new_task(&self.state.sessions)
        {
            self.select_session(session_id, cx);
        } else if self.selected_project().is_some_and(Project::is_projectless) {
            self.create_projectless_session(cx);
        } else if let Some(project_id) = self.state.selected_project {
            self.create_session_for(project_id, self.state.last_provider, cx);
        } else {
            self.create_projectless_session(cx);
        }
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
    }

    pub(super) fn new_project_action(
        &mut self,
        _: &NewProject,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_project(cx);
    }

    pub(super) fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = Some(SettingsPage::General);
        self.settings_scroll.set_offset(gpui::Point::default());
        // Sparkle owns this value and its consent prompt can flip it outside
        // the settings UI, so re-mirror it each time settings opens.
        self.automatic_updates_enabled = cx
            .try_global::<crate::updater::UpdaterState>()
            .and_then(|updater| updater.0.as_ref())
            .is_some_and(|updater| updater.automatically_checks_for_updates());
        // Warm the Usage page's transcript scan while the user is still on
        // General, so clicking Usage lands on data instead of a spinner.
        self.ensure_usage_history(false, cx);
        window.focus(&self.settings_focus, cx);
        cx.notify();
    }

    pub(super) fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sidebar_visible(!self.sidebar_visible, cx);
    }

    pub(super) fn toggle_right_panel_action(
        &mut self,
        _: &ToggleRightPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_right_panel_visible(!self.right_panel_visible, cx);
    }

    pub(super) fn toggle_fps_counter_action(
        &mut self,
        _: &ToggleFpsCounter,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fps_counter_visible = !self.fps_counter_visible;
        cx.notify();
    }

    pub(super) fn set_sidebar_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.sidebar_visible == visible {
            return;
        }
        self.sidebar_visible = visible;
        self.sidebar_slide = self.begin_panel_slide(self.sidebar_rendered_width, cx);
        self.persist_panel_layout();
        cx.notify();
    }

    /// A toggle's slide, starting from the width the panel currently occupies
    /// so an interrupted one reverses from where its edge actually is.
    /// Reduce-motion gets `None`: the panel simply appears at its new width,
    /// and no frames are scheduled for it.
    fn begin_panel_slide(&self, from: f32, cx: &App) -> Option<motion::WidthTween> {
        (!cx.reduce_motion()).then(|| motion::WidthTween::new(from))
    }

    pub(super) fn set_right_panel_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible {
            self.request_active_terminal_focus();
        } else {
            self.right_panel_pending_terminal_focus = None;
        }
        if self.right_panel_visible == visible {
            return;
        }
        self.right_panel_visible = visible;
        self.right_panel_slide = self.begin_panel_slide(self.right_panel_rendered_width, cx);
        if visible {
            self.analytics
                .track(crate::analytics::Event::RightPanelOpened);
        }
        self.persist_panel_layout();
        cx.notify();
    }

    pub(super) fn persist_panel_layout(&mut self) {
        self.state.sidebar_visible = self.sidebar_visible;
        self.state.right_panel_visible = self.right_panel_visible;
        self.state.sidebar_width = self.sidebar_width;
        self.state.right_panel_width = self.right_panel_width;
        self.save();
    }

    /// Mirror the live window frame into persisted state; disk waits for the
    /// app-quit save (any other `save` carries the frame along for free).
    /// macOS reports a zoomed window as `Windowed` with screen-filling bounds,
    /// so while maximized (and while fullscreen) the last floating frame is
    /// kept as the restore size — and the display it was captured on — and
    /// only the flag advances.
    pub(super) fn capture_window_state(&mut self, window: &Window, cx: &App) {
        // Bounds also change when the OS relocates the window — a monitor
        // unplugged, a display asleep. Those moves are not the user's; like
        // Zed, only capture while the window is the active one.
        if !window.is_window_active() {
            return;
        }
        let previous = self.state.window_state;
        let display = window.display(cx).and_then(|display| display.uuid().ok());
        self.state.window_state = Some(match window.window_bounds() {
            WindowBounds::Fullscreen(restore) => {
                previous.unwrap_or_else(|| persisted_window_state(restore, false, display))
            }
            WindowBounds::Maximized(restore) => persisted_window_state(restore, true, display),
            WindowBounds::Windowed(bounds) if window.is_maximized() => PersistedWindowState {
                maximized: true,
                ..previous.unwrap_or_else(|| persisted_window_state(bounds, true, display))
            },
            WindowBounds::Windowed(bounds) => persisted_window_state(bounds, false, display),
        });
    }

    /// The width each panel lays its content out at. A panel mid-slide counts
    /// as on screen and keeps its full width here: the slide narrows the
    /// container that clips it, so nothing inside reflows on the way out.
    /// What the panel actually occupies this frame is
    /// [`Waku::sidebar_rendered_width`] / [`Waku::right_panel_rendered_width`].
    pub(super) fn effective_panel_widths(&self, window: &Window) -> (f32, f32) {
        fitted_panel_widths(
            f32::from(window.viewport_size().width),
            self.sidebar_visible || self.sidebar_slide.is_some(),
            self.right_panel_visible || self.right_panel_slide.is_some(),
            self.sidebar_width,
            self.right_panel_width,
        )
    }

    pub(super) fn begin_panel_resize(
        &mut self,
        target: PanelResizeTarget,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        // A drag tracks the pointer directly; whatever slide was still
        // finishing would fight it for the same edge.
        let start_width = match target {
            PanelResizeTarget::Sidebar => {
                self.sidebar_slide = None;
                self.sidebar_width = sidebar_width;
                crate::platform::set_sidebar_material_width(window, sidebar_width);
                sidebar_width
            }
            PanelResizeTarget::RightPanel => {
                self.right_panel_slide = None;
                self.right_panel_width = right_panel_width;
                right_panel_width
            }
            PanelResizeTarget::FileTree => {
                let width =
                    fitted_file_tree_width(right_panel_width, self.right_panel_file_tree_width);
                self.right_panel_file_tree_width = width;
                width
            }
        };
        self.panel_resize_drag = Some(PanelResizeDrag {
            target,
            start_mouse_x: f32::from(event.position.x),
            start_width,
        });
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn resize_panel_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.panel_resize_drag else {
            return;
        };
        let viewport_width = f32::from(window.viewport_size().width);
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        let delta = f32::from(event.position.x) - drag.start_mouse_x;
        match drag.target {
            PanelResizeTarget::Sidebar => {
                let maximum = SIDEBAR_MAX_WIDTH
                    .min(viewport_width - MAIN_PANEL_MIN_WIDTH - right_panel_width)
                    .max(SIDEBAR_MIN_WIDTH);
                let width = (drag.start_width + delta).clamp(SIDEBAR_MIN_WIDTH, maximum);
                if (self.sidebar_width - width).abs() < 0.5 {
                    return;
                }
                self.sidebar_width = width;
                crate::platform::set_sidebar_material_width(window, width);
            }
            PanelResizeTarget::RightPanel => {
                let maximum = RIGHT_PANEL_MAX_WIDTH
                    .min(viewport_width - MAIN_PANEL_MIN_WIDTH - sidebar_width)
                    .max(RIGHT_PANEL_MIN_WIDTH);
                let width = (drag.start_width - delta).clamp(RIGHT_PANEL_MIN_WIDTH, maximum);
                if (self.right_panel_width - width).abs() < 0.5 {
                    return;
                }
                self.right_panel_width = width;
            }
            PanelResizeTarget::FileTree => {
                let maximum = FILE_TREE_MAX_WIDTH
                    .min(right_panel_width - FILE_EDITOR_MIN_WIDTH)
                    .max(FILE_TREE_MIN_WIDTH);
                let width = (drag.start_width - delta).clamp(FILE_TREE_MIN_WIDTH, maximum);
                if (self.right_panel_file_tree_width - width).abs() < 0.5 {
                    return;
                }
                self.right_panel_file_tree_width = width;
            }
        }
        cx.notify();
    }

    pub(super) fn finish_panel_resize(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Left
            && let Some(drag) = self.panel_resize_drag.take()
        {
            if drag.target != PanelResizeTarget::FileTree {
                self.persist_panel_layout();
            }
            cx.notify();
        }
    }

    pub(super) fn navigate_back_action(
        &mut self,
        _: &NavigateBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.take().is_some() {
            let focus_handle = self.composer_focus(cx);
            window.focus(&focus_handle, cx);
            cx.notify();
            return;
        }

        let Some(current) = self.state.selected_session else {
            return;
        };
        if let Some(target) = self.session_navigation.back_target() {
            self.settings_page = None;
            self.request_session_activation(
                target,
                SessionActivationTransition::Back { from: current },
                cx,
            );
        }
    }

    pub(super) fn navigate_forward_action(
        &mut self,
        _: &NavigateForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.is_some() {
            return;
        }

        let Some(current) = self.state.selected_session else {
            return;
        };
        if let Some(target) = self.session_navigation.forward_target() {
            self.settings_page = None;
            self.request_session_activation(
                target,
                SessionActivationTransition::Forward { from: current },
                cx,
            );
        }
    }

    pub(super) fn navigation_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.button {
            MouseButton::Navigate(NavigationDirection::Back) => {
                cx.stop_propagation();
                self.navigate_back_action(&NavigateBack, window, cx);
            }
            MouseButton::Navigate(NavigationDirection::Forward) => {
                cx.stop_propagation();
                self.navigate_forward_action(&NavigateForward, window, cx);
            }
            _ => {}
        }
    }

    pub(super) fn focus_composer_action(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = None;
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn cancel_turn_action(
        &mut self,
        _: &CancelTurn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.take().is_some() {
            let focus_handle = self.composer_focus(cx);
            window.focus(&focus_handle, cx);
            cx.notify();
            return;
        }
        if self.message_edit.is_some() {
            self.cancel_message_edit(window, cx);
            return;
        }
        let Some(target) = self.selected_escape_stop_target() else {
            self.cancel_turn(cx);
            return;
        };
        match self.escape_stop_confirmation.press(target, Instant::now()) {
            EscapeStopPress::Stop => self.cancel_turn(cx),
            EscapeStopPress::Arm(arm) => {
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(ESCAPE_STOP_CONFIRMATION_TIMEOUT)
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.escape_stop_confirmation.expire(arm) {
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    fn selected_escape_stop_target(&self) -> Option<EscapeStopTarget> {
        let session = self.selected_session()?;
        (!self.submission_preparations.contains(&session.id) && session.status.is_busy())
            .then(|| EscapeStopTarget::for_session(session))
    }

    pub(super) fn reset_visible_state(&mut self) {
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.expanded_turns.clear();
        self.expanded_changed_files.clear();
        self.transcript_control_focuses.borrow_mut().clear();
        // Selection belongs to the session being left.
        self.transcript_selection.selection.borrow_mut().clear();
        self.transcript_selection.registry.borrow_mut().clear();
        let (streaming_messages, live_reasoning) = self.selected_session().map_or_else(
            || (Vec::new(), Vec::new()),
            |session| {
                let messages = session
                    .messages
                    .iter()
                    .filter(|message| message.role == MessageRole::Assistant && message.streaming)
                    .map(|message| message.id)
                    .collect();
                let reasoning = session
                    .transcript_blocks
                    .iter()
                    .flat_map(|block| &block.activities)
                    .filter(|activity| activity.reasoning.is_some() && !activity.complete)
                    .map(|activity| activity.id)
                    .collect();
                (messages, reasoning)
            },
        );
        // Parsed messages are keyed by message id, which is unique across
        // sessions, so they stay cached — switching back to a recent session
        // then costs no re-parse. Bounded so a long-running window cannot grow
        // without limit.
        let mut message_markdown = self.message_markdown.borrow_mut();
        let cached_bytes: usize = message_markdown
            .values()
            .map(md::render::MarkdownView::source_len)
            .sum();
        if cached_bytes > MAX_CACHED_MESSAGE_SOURCE_BYTES {
            message_markdown.clear();
        }
        for id in streaming_messages {
            message_markdown
                .entry(id)
                .or_insert_with(MarkdownView::new)
                .seed_streaming_baseline();
        }
        drop(message_markdown);
        // Block parses are keyed by position within the session, so they would
        // be read as another session's blocks.
        let mut activity_markdown = self.activity_markdown.borrow_mut();
        activity_markdown.clear();
        for id in live_reasoning {
            activity_markdown.insert(id, MarkdownView::seeded());
        }
        drop(activity_markdown);
        self.reasoning_window_starts.borrow_mut().clear();
        self.activity_scroll_viewports.borrow_mut().clear();
        self.menus.borrow_mut().clear();
        self.message_edit = None;
        self.hide_toast();
        self.navigation_rail_reset_generation
            .set(self.navigation_rail_reset_generation.get().wrapping_add(1));
        self.transcript_anchor.set(None);
        self.transcript_anchor_end_space.set(Pixels::ZERO);
        self.transcript_anchor_following.set(false);
    }

    pub(super) fn reset_session_runtime(&mut self, session_id: Uuid) {
        if let Some(runtime) = self.runtimes.remove(&session_id) {
            runtime.driver.cancel();
            runtime.driver.close();
            self.mark_background_work_lost(session_id);
        }
    }

    fn remember_selected_model_traits(&mut self) {
        let Some((provider, model, reasoning_effort, service_tier, context_window)) =
            self.selected_session().and_then(|session| {
                Some((
                    session.provider,
                    self.model_for_session(session)?.to_owned(),
                    session.reasoning_effort.clone(),
                    session.service_tier.clone(),
                    session.context_window.clone(),
                ))
            })
        else {
            return;
        };
        self.state.remember_model_traits(
            provider,
            &model,
            reasoning_effort,
            service_tier,
            context_window,
        );
    }

    pub(super) fn choose_model(
        &mut self,
        provider: ProviderKind,
        model: String,
        cx: &mut Context<Self>,
    ) {
        let Some((session_id, provider_changed)) = self
            .selected_session()
            .filter(|session| {
                session.can_choose_model(provider)
                    && (session.provider != provider
                        || session.model.as_deref() != Some(model.as_str()))
            })
            .map(|session| (session.id, session.provider != provider))
        else {
            return;
        };

        self.remember_selected_model_traits();
        let (reasoning_effort, service_tier, context_window) =
            self.state.model_traits_for(provider, &model);
        if let Some(session) = self.selected_session_mut() {
            session.provider = provider;
            session.model = Some(model.clone());
            if provider_changed {
                session.agent_preset = None;
            }
            session.reasoning_effort.clone_from(&reasoning_effort);
            session.service_tier.clone_from(&service_tier);
            session.context_window.clone_from(&context_window);
            self.state.last_provider = provider;
            self.state.last_model = Some(model);
            self.state.last_reasoning_effort = reasoning_effort;
            self.state.last_service_tier = service_tier;
            self.state.last_context_window = context_window;
            self.model_picker_tab = ModelPickerTab::Provider(provider);
            // A different provider is a different binary and protocol; only a
            // model change within one provider can be applied in session.
            if provider_changed {
                self.reset_session_runtime(session_id);
                // A different provider is also a different command registry.
                self.refresh_composer_sources(cx);
            } else {
                self.apply_session_options(session_id, cx);
            }
            self.save();
            cx.notify();
        }
    }

    /// Primary modifier + /: toggle the composer's model picker as if its chip were clicked.
    pub(super) fn toggle_model_picker_action(
        &mut self,
        _: &ToggleModelPicker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.is_some() {
            return;
        }
        if !self
            .selected_session()
            .is_some_and(|session| session.can_choose_model(session.provider))
        {
            return;
        }
        let menus = self.menus.borrow();
        let Some(handle) = menus.get(MODEL_PICKER_MENU_ID).cloned() else {
            return;
        };
        // A keyboard toggle produces no mouse-down for another open menu's
        // dismiss-on-down-out to see, so close the rest here.
        let other_open: Vec<_> = menus
            .iter()
            .filter(|(id, other)| id.as_ref() != MODEL_PICKER_MENU_ID && other.is_open())
            .map(|(_, other)| other.clone())
            .collect();
        drop(menus);
        // The picker's toggle observers update this entity, so the toggle has
        // to run after this listener releases it.
        window.defer(cx, move |window, cx| {
            for menu in other_open {
                menu.close(window, cx);
            }
            crate::ui::menu::toggle_popover(&handle, MenuAlign::AboveLeft, window, cx);
        });
    }

    /// Discovery is not requested here: launch already requested it for every
    /// installed provider, so tabs only ever switch between loaded lists.
    pub(super) fn select_model_picker_tab(&mut self, tab: ModelPickerTab, cx: &mut Context<Self>) {
        if self.model_picker_tab != tab {
            self.model_picker_tab = tab;
            if let ModelPickerTab::Provider(provider) = tab {
                // Selecting a rail re-runs that provider's catalog discovery,
                // so each tab is fresh when viewed without probing every
                // provider on open.
                self.refresh_provider_model_discovery(provider);
            }
            // A different tab renumbers the rows under the keyboard cursor,
            // and would otherwise inherit the old tab's scroll offset.
            self.model_picker_highlight = None;
            self.reveal_selected_picker_model();
            cx.notify();
        }
    }

    pub(super) fn toggle_favorite_model(
        &mut self,
        provider: ProviderKind,
        model: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .state
            .favorite_models
            .iter()
            .position(|favorite| favorite.provider == provider && favorite.model == model)
        {
            self.state.favorite_models.remove(index);
        } else {
            self.state
                .favorite_models
                .push(FavoriteModel { provider, model });
        }
        self.save();
        cx.notify();
    }

    pub(super) fn set_runtime_mode(&mut self, mode: RuntimeMode, cx: &mut Context<Self>) {
        if mode == RuntimeMode::Plan {
            return;
        }
        if let Some(session) = self.selected_session_mut()
            && session.runtime_mode != mode
        {
            let session_id = session.id;
            session.runtime_mode = mode;
            self.apply_session_options(session_id, cx);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_interaction_mode(&mut self, mode: InteractionMode, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.interaction_mode != mode
        {
            let session_id = session.id;
            session.interaction_mode = mode;
            self.apply_session_options(session_id, cx);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_reasoning_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.reasoning_effort.as_deref() != Some(effort.as_str())
        {
            let session_id = session.id;
            session.reasoning_effort = Some(effort.clone());
            self.state.last_reasoning_effort = Some(effort);
            self.remember_selected_model_traits();
            self.apply_session_options(session_id, cx);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_service_tier(&mut self, tier: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.service_tier.as_deref() != Some(tier.as_str())
        {
            let session_id = session.id;
            session.service_tier = Some(tier.clone());
            self.state.last_service_tier = Some(tier);
            self.remember_selected_model_traits();
            self.apply_session_options(session_id, cx);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_context_window(&mut self, window: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.context_window.as_deref() != Some(window.as_str())
        {
            let session_id = session.id;
            session.context_window = Some(window.clone());
            self.state.last_context_window = Some(window);
            self.remember_selected_model_traits();
            self.apply_session_options(session_id, cx);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_agent_preset(&mut self, agent_preset: String, cx: &mut Context<Self>) {
        let selectable = self
            .provider_probe(ProviderKind::DeepSeek)
            .is_some_and(|probe| {
                probe
                    .agent_presets
                    .iter()
                    .any(|preset| preset.id == agent_preset)
            });
        if !selectable {
            return;
        }
        if let Some(session) = self.selected_session_mut()
            && session.provider == ProviderKind::DeepSeek
            && !session.has_started()
            && !session.is_busy()
            && session.agent_preset.as_deref() != Some(agent_preset.as_str())
        {
            let session_id = session.id;
            if agent_preset == "minimal" {
                session.interaction_mode = InteractionMode::Build;
            }
            session.agent_preset = Some(agent_preset);
            // A provider cursor makes a session started, so this is normally a
            // no-op. It also closes the narrow race where a blank runtime was
            // prepared but had not reported its native session yet.
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        self.escape_stop_confirmation.clear();
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        // Worktree/checkpoint preparation has no safe interrupt contract. The
        // composer deliberately shows a spinner rather than Stop until the
        // provider runtime exists, and the keyboard action follows the same
        // boundary.
        if self.submission_preparations.contains(&session_id) {
            return;
        }
        let retain_runtime = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| retain_runtime_after_cancel(session.provider))
            || self.session_has_live_detached_work(session_id);
        let mut runtime = self.runtimes.remove(&session_id);
        if let Some(runtime) = runtime.as_ref() {
            runtime.driver.cancel();
            if retain_runtime {
                // A detached process keeps Codex's app-server resident, but
                // Computer Use descendants still belong to the cancelled turn.
                runtime.driver.cancel_computer_use();
            }
        }
        // Do not leave already-received text in the smoothing queue: once the
        // message is marked complete, a later delta would otherwise create a
        // second assistant bubble. Show the received portion immediately.
        // Buffered turn-completion events also must not start queued
        // follow-ups: the user asked to stop, not to continue.
        let mut keep_runtime = true;
        if let Some(runtime) = runtime.as_mut() {
            Self::collect_runtime_events(runtime);
            while let Some(event) = runtime.pending_events.pop_front() {
                keep_runtime &= self.handle_driver_event(session_id, runtime, event, false, cx);
                if !keep_runtime {
                    break;
                }
            }
        }
        self.pending_queue_drains.retain(|id| *id != session_id);
        let has_active_turn = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(AgentSession::active_turn_id)
            .is_some();
        let previous_kinds = has_active_turn
            .then(|| self.snapshot_selected_transcript_rows(session_id))
            .flatten();
        self.finish_streaming_assistant(session_id);
        self.complete_turn_blocks(session_id);
        self.settle_foreground_work(session_id, BackgroundWorkStatus::Stopped);
        if let Some(runtime) = runtime.as_mut() {
            runtime.stream_phase = None;
            runtime.pending_permission = None;
            runtime.pending_user_input = None;
            runtime.pending_computer_approval = None;
            runtime.computer_use_previews.clear();
        }
        if has_active_turn {
            let needs_fallback = !self.turn_has_assistant_message(session_id);
            if let Some(session) = self.state.session_mut(session_id) {
                session.status = SessionStatus::Idle;
                if needs_fallback {
                    session.push_message(MessageRole::Assistant, tr!("session.stopped"));
                }
            }
            self.finish_active_turn_with_analytics(
                session_id,
                TurnStatus::Interrupted,
                crate::analytics::TurnOutcome::Cancelled,
            );
        }
        if has_active_turn {
            self.capture_latest_turn_checkpoint_for(session_id);
            self.start_pending_checkpoint_captures(cx);
        }
        if let Some(previous_kinds) = previous_kinds.as_deref() {
            self.splice_active_transcript_rows_after_visibility_change(previous_kinds);
        }
        // A provider runtime owns its Waku JavaScript REPL and Computer Use
        // descendants. Normally Stop closes that process tree and the next
        // prompt resumes the same provider thread with a fresh runtime. A
        // detached process or subagent is the exception: its provider must
        // remain resident so Waku can keep observing and stopping it.
        if retain_runtime && keep_runtime {
            if let Some(runtime) = runtime.take() {
                self.runtimes.insert(session_id, runtime);
            }
        } else if let Some(runtime) = runtime {
            runtime.driver.close();
        }
        self.remeasure_transcript_tail();
        self.save();
        cx.notify();
    }

    pub(super) fn respond_permission(
        &mut self,
        request_id: String,
        option_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let provider = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.provider.id());
        let decision = if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            let decision = runtime
                .pending_permission
                .as_ref()
                .and_then(|permission| {
                    permission
                        .options
                        .iter()
                        .find(|option| option.id == option_id)
                })
                .map_or(
                    "other",
                    |option| if option.allow { "allow" } else { "deny" },
                );
            runtime.driver.respond(request_id, option_id);
            runtime.pending_permission = None;
            Some(decision)
        } else {
            None
        };
        if let (Some(provider), Some(decision)) = (provider, decision) {
            self.analytics
                .track(crate::analytics::Event::PermissionResponded {
                    provider,
                    kind: "provider",
                    decision,
                });
        }
        if let Some(session) = self.selected_session_mut() {
            session.status = SessionStatus::Working;
        }
        cx.notify();
    }

    pub(super) fn sync_user_input_answer(&mut self, cx: &mut Context<Self>) {
        let answer = self
            .selected_runtime()
            .and_then(|runtime| runtime.pending_user_input.as_ref())
            .and_then(|pending| {
                pending
                    .current_question()
                    .map(|question| (pending, question))
            })
            .and_then(|(pending, question)| pending.custom_answers.get(&question.id))
            .cloned()
            .unwrap_or_default();
        self.user_input_answer
            .update(cx, |input, cx| input.set_content(answer, cx));
    }

    pub(super) fn update_user_input_custom_answer(
        &mut self,
        answer: impl AsRef<str>,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let Some(pending) = self
            .runtimes
            .get_mut(&session_id)
            .and_then(|runtime| runtime.pending_user_input.as_mut())
        else {
            return;
        };
        let Some(question_id) = pending
            .current_question()
            .map(|question| question.id.clone())
        else {
            return;
        };
        let answer = answer.as_ref().to_owned();
        if answer.trim().is_empty() {
            pending.custom_answers.remove(&question_id);
        } else {
            pending.custom_answers.insert(question_id.clone(), answer);
            pending.selections.remove(&question_id);
        }
        cx.notify();
    }

    pub(super) fn submit_user_input_custom_answer(
        &mut self,
        answer: String,
        cx: &mut Context<Self>,
    ) {
        if answer.trim().is_empty() {
            return;
        }
        self.update_user_input_custom_answer(answer, cx);
        self.advance_user_input(cx);
    }

    pub(super) fn select_user_input_option(&mut self, label: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let Some(pending) = self
            .runtimes
            .get_mut(&session_id)
            .and_then(|runtime| runtime.pending_user_input.as_mut())
        else {
            return;
        };
        let Some((question_id, multi_select)) = pending
            .current_question()
            .map(|question| (question.id.clone(), question.multi_select))
        else {
            return;
        };
        let selected = pending.selections.entry(question_id.clone()).or_default();
        if multi_select {
            if let Some(index) = selected.iter().position(|answer| answer == &label) {
                selected.remove(index);
            } else {
                selected.push(label);
            }
        } else {
            selected.clear();
            selected.push(label);
        }
        if selected.is_empty() {
            pending.selections.remove(&question_id);
        }
        pending.custom_answers.remove(&question_id);
        self.user_input_answer
            .update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    pub(super) fn previous_user_input(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let Some(pending) = self
            .runtimes
            .get_mut(&session_id)
            .and_then(|runtime| runtime.pending_user_input.as_mut())
        else {
            return;
        };
        if pending.question_index == 0 {
            return;
        }
        pending.question_index -= 1;
        self.sync_user_input_answer(cx);
        cx.notify();
    }

    pub(super) fn advance_user_input(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let should_submit = {
            let Some(pending) = self
                .runtimes
                .get_mut(&session_id)
                .and_then(|runtime| runtime.pending_user_input.as_mut())
            else {
                return;
            };
            let Some(question) = pending.current_question() else {
                return;
            };
            let answered = pending
                .custom_answers
                .get(&question.id)
                .is_some_and(|answer| !answer.trim().is_empty())
                || pending
                    .selections
                    .get(&question.id)
                    .is_some_and(|answers| !answers.is_empty());
            if !answered {
                return;
            }
            if pending.question_index + 1 < pending.questions.len() {
                pending.question_index += 1;
                false
            } else {
                true
            }
        };

        if should_submit {
            let Some(runtime) = self.runtimes.get_mut(&session_id) else {
                return;
            };
            let Some(pending) = runtime.pending_user_input.take() else {
                return;
            };
            let answers = pending.answers();
            runtime
                .driver
                .respond_user_input(pending.request_id, answers);
            if let Some(session) = self.state.session_mut(session_id) {
                session.status = SessionStatus::Working;
            }
            self.user_input_answer
                .update(cx, |input, cx| input.clear(cx));
        } else {
            self.sync_user_input_answer(cx);
        }
        cx.notify();
    }

    pub(super) fn respond_computer_permission(
        &mut self,
        decision: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let provider = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map_or("unknown", |session| session.provider.id());
        let Some(mut runtime) = self.runtimes.remove(&session_id) else {
            return;
        };
        let Some(pending) = runtime.pending_computer_approval.take() else {
            self.runtimes.insert(session_id, runtime);
            return;
        };

        if decision == "deny" {
            runtime.driver.reject_computer_tool(
                pending.request,
                "The user denied control of this app.".into(),
            );
        } else {
            let key = pending.target.grant_key();
            runtime.computer_session_grants.insert(key);
            if decision == "always" && pending.target.persistable() {
                let grant = crate::computer_use::ComputerAppGrant {
                    bundle_id: pending.target.bundle_id.clone(),
                    app_name: pending.target.app_name.clone(),
                };
                if !self
                    .state
                    .computer_use_allowed_apps
                    .iter()
                    .any(|existing| existing.key() == grant.key())
                {
                    self.state.computer_use_allowed_apps.push(grant);
                    self.save();
                }
            }
            runtime.driver.run_computer_tool(pending.request);
        }
        if let Some(session) = self.state.session_mut(session_id) {
            session.status = SessionStatus::Working;
        }
        self.analytics
            .track(crate::analytics::Event::PermissionResponded {
                provider,
                kind: "computer_use",
                decision: match decision {
                    "deny" => "deny",
                    "always" => "allow_always",
                    "task" => "allow_task",
                    _ => "other",
                },
            });
        self.runtimes.insert(session_id, runtime);
        cx.notify();
    }

    pub(super) fn bring_computer_use_to_front(&mut self, window_id: u32, cx: &mut Context<Self>) {
        if let Some(runtime) = self
            .state
            .selected_session
            .and_then(|session_id| self.runtimes.get_mut(&session_id))
            && let Some(index) = runtime.computer_use_previews.iter().position(|preview| {
                preview
                    .target
                    .as_ref()
                    .is_some_and(|target| target.window_id == window_id)
            })
        {
            let preview = runtime.computer_use_previews.remove(index);
            runtime.computer_use_previews.push(preview);
        }
        cx.notify();
    }

    pub(super) fn dismiss_computer_use(&mut self, window_id: u32, cx: &mut Context<Self>) {
        if let Some(runtime) = self
            .state
            .selected_session
            .and_then(|session_id| self.runtimes.get_mut(&session_id))
        {
            runtime.computer_use_previews.retain(|preview| {
                preview
                    .target
                    .as_ref()
                    .is_none_or(|target| target.window_id != window_id)
            });
        }
        cx.notify();
    }

    pub(super) fn add_project(&mut self, cx: &mut Context<Self>) {
        if self.daemon.is_remote() {
            self.show_toast(tr!("errors.remote_project_picker"));
            cx.notify();
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr!("project.add_project").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    if let Some(existing) = this.state.projects.iter().find(|p| p.path == path) {
                        this.select_project(existing.id, cx);
                        return;
                    }
                    let project = Project::from_path(path);
                    let project_id = project.id;
                    this.state.projects.push(project);
                    this.analytics.track(crate::analytics::Event::ProjectAdded);
                    this.create_session_for(project_id, this.state.last_provider, cx);
                });
            }
        })
        .detach();
    }

    pub(super) fn create_projectless_session(&mut self, cx: &mut Context<Self>) {
        if let Some(draft_id) = self
            .state
            .sessions
            .iter()
            .find(|session| {
                !session.has_started()
                    && self.state.projects.iter().any(|project| {
                        project.id == session.project_id
                            && project.is_projectless()
                            && !crate::projectless::is_legacy_root_path(&project.path)
                    })
            })
            .map(|session| session.id)
        {
            self.select_session(draft_id, cx);
            return;
        }

        let workspace = waku_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |waku, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match workspace.request(
                        waku_client::WorkspaceOperation::CreateProjectlessWorkspace {
                            prompt: None,
                        },
                    )? {
                        waku_client::WorkspaceResult::ProjectlessWorkspace { cwd } => Ok(cwd),
                        _ => anyhow::bail!("the daemon returned an invalid projectless response"),
                    }
                })
                .await;
            let _ = waku.update(cx, |waku, cx| match result {
                Ok(cwd) => {
                    let mut project = Project::from_path(cwd);
                    project.name = Project::PROJECTLESS_NAME.to_owned();
                    let project_id = project.id;
                    waku.state.projects.push(project);
                    waku.create_session_for(project_id, waku.state.last_provider, cx);
                }
                Err(error) => {
                    waku.show_toast(tr!("errors.create_projectless_task", error = error));
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_task_navigation_keeps_the_selected_project_after_visiting_history() {
        let project_id = Uuid::new_v4();
        let draft = AgentSession::new(project_id, ProviderKind::Codex);
        let mut started = AgentSession::new(Uuid::new_v4(), ProviderKind::Claude);
        started.begin_turn("Existing task");
        let mut navigation = SessionNavigation::default();

        navigation.remember_new_task(draft.id);
        navigation.visit(Some(draft.id), started.id);

        assert_eq!(
            navigation.remembered_new_task(&[draft.clone(), started]),
            Some(draft.id)
        );
    }

    #[test]
    fn new_task_navigation_does_not_reopen_a_started_or_removed_draft() {
        let project_id = Uuid::new_v4();
        let mut draft = AgentSession::new(project_id, ProviderKind::Codex);
        let mut navigation = SessionNavigation::default();
        navigation.remember_new_task(draft.id);

        draft.begin_turn("Start it");
        assert_eq!(navigation.remembered_new_task(&[draft.clone()]), None);

        navigation.remove(draft.id);
        assert_eq!(navigation.new_task, None);
    }

    #[test]
    fn stopping_releases_the_runtimes_that_cannot_be_interrupted_in_place() {
        // Codex owns a Computer Use process tree; Amp has no stream interrupt.
        assert!(!retain_runtime_after_cancel(ProviderKind::Codex));
        assert!(!retain_runtime_after_cancel(ProviderKind::Amp));
        for provider in ProviderKind::ALL {
            if !matches!(provider, ProviderKind::Codex | ProviderKind::Amp) {
                assert!(retain_runtime_after_cancel(provider));
            }
        }
    }
}
