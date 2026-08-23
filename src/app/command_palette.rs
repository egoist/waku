//! The window-wide command palette opened with the platform's primary shortcut + K.
//!
//! The search field keeps real focus while the list cursor is drawn, matching
//! native pickers and Zed's command palette. Metadata results rebuild when the
//! query changes; persisted transcript matches join from a debounced background
//! SQLite scan. Caret blinks therefore repaint one in-memory snapshot instead
//! of re-fuzzy-matching history or touching storage every frame.

use gpui::{KeyBinding, StyledText, TextRun, actions};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use super::*;

actions!(
    waku_command_palette,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        SelectPageDown,
        SelectPageUp,
        Confirm,
        Dismiss
    ]
);

const SEARCH_CONTEXT: &str = "CommandPalette > TextInput";
const MAX_TASK_RESULTS: usize = 12;
const MESSAGE_SEARCH_LIMIT: usize = 50;
const MESSAGE_SEARCH_CACHE_CAPACITY: usize = 24;
const PAGE_STEP: isize = 7;
const MESSAGE_SEARCH_DEBOUNCE: Duration = Duration::from_millis(90);
const SEARCH_ROW_HEIGHT: f32 = 60.0;
const SECTION_HEADER_HEIGHT: f32 = 30.0;
const RESULT_ROW_HEIGHT: f32 = 44.0;
const CONTENT_RESULT_ROW_HEIGHT: f32 = 60.0;
const EMPTY_RESULTS_HEIGHT: f32 = 180.0;
const RESULTS_BOTTOM_PADDING: f32 = 8.0;
const MAX_CARD_HEIGHT: f32 = 480.0;

/// Bind list navigation beneath the focused one-line input. This is registered
/// after the input's bindings, although the more-specific key context would
/// win either way.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(SEARCH_CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(SEARCH_CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(SEARCH_CONTEXT)),
        KeyBinding::new("ctrl-p", SelectPrevious, Some(SEARCH_CONTEXT)),
        KeyBinding::new("tab", SelectNext, Some(SEARCH_CONTEXT)),
        KeyBinding::new("shift-tab", SelectPrevious, Some(SEARCH_CONTEXT)),
        KeyBinding::new("home", SelectFirst, Some(SEARCH_CONTEXT)),
        KeyBinding::new("end", SelectLast, Some(SEARCH_CONTEXT)),
        KeyBinding::new("pagedown", SelectPageDown, Some(SEARCH_CONTEXT)),
        KeyBinding::new("pageup", SelectPageUp, Some(SEARCH_CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(SEARCH_CONTEXT)),
        // Bound at the palette, not the field: the query field's own
        // clear-on-escape outranks this (deeper context) while it has text,
        // and an empty field propagates the keystroke down to it.
        KeyBinding::new("escape", Dismiss, Some("CommandPalette")),
    ]);
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PaletteSection {
    Suggested,
    Tasks,
    Commands,
    Settings,
}

impl PaletteSection {
    fn label(self) -> String {
        crate::i18n::translate(match self {
            Self::Suggested => "command_palette.suggested",
            Self::Tasks => "command_palette.tasks",
            Self::Commands => "command_palette.commands",
            Self::Settings => "command_palette.settings",
        })
    }

    fn query_rank(self) -> usize {
        match self {
            Self::Commands | Self::Suggested => 0,
            Self::Tasks => 1,
            Self::Settings => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaletteIcon {
    Asset(&'static str),
    Provider(ProviderKind),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PaletteAction {
    NewTask,
    OpenProject,
    FocusComposer,
    ChooseModel,
    ToggleUsage,
    CollapseSidebarGroups,
    ToggleSidebar,
    ToggleRightPanel,
    OpenSettings(SettingsPage),
    SelectTask(Uuid),
}

#[derive(Clone, Debug)]
pub(super) struct CommandPaletteItem {
    section: PaletteSection,
    label: String,
    detail: Option<String>,
    icon: PaletteIcon,
    shortcut: Option<&'static str>,
    action: PaletteAction,
    content_match: Option<crate::persistence::SessionMessageMatch>,
    search_text: String,
    order: usize,
    recency: u64,
}

impl CommandPaletteItem {
    fn command(
        section: PaletteSection,
        label: String,
        icon: &'static str,
        shortcut: Option<&'static str>,
        action: PaletteAction,
        keywords: &'static str,
        order: usize,
    ) -> Self {
        let search_text = format!("{label} {keywords}");
        Self {
            section,
            label,
            detail: None,
            icon: PaletteIcon::Asset(icon),
            shortcut,
            action,
            content_match: None,
            search_text,
            order,
            recency: 0,
        }
    }
}

struct ScoredPaletteItem {
    score: u32,
    item: CommandPaletteItem,
}

fn next_selection_index(selected: usize, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let selected = selected.min(len - 1);
    Some(if delta == isize::MIN {
        0
    } else if delta == isize::MAX {
        len - 1
    } else if delta.unsigned_abs() > 1 {
        (selected as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize
    } else {
        (selected as isize + delta).rem_euclid(len as isize) as usize
    })
}

fn palette_content_match_text(
    matched: &crate::persistence::SessionMessageMatch,
    query: &str,
    window: &Window,
    theme: Theme,
) -> StyledText {
    let (source_label, source_color) = match matched.source {
        MessageRole::User => (tr!("command_palette.you"), theme.gauge),
        MessageRole::Assistant | MessageRole::System => {
            (tr!("command_palette.agent"), theme.success)
        }
    };
    let label = format!("{source_label}: ");
    let mut text = String::with_capacity(label.len() + matched.snippet.len());
    text.push_str(&label);
    text.push_str(&matched.snippet);

    let mut normal_font = window.text_style().font();
    normal_font.weight = FontWeight::NORMAL;
    let mut emphasized_font = normal_font.clone();
    emphasized_font.weight = FontWeight::SEMIBOLD;
    let mut runs = vec![TextRun {
        len: label.len(),
        font: emphasized_font.clone(),
        color: source_color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];

    let query = query.trim();
    let match_range = (!query.is_empty())
        .then(|| {
            matched
                .snippet
                .to_ascii_lowercase()
                .find(&query.to_ascii_lowercase())
                .map(|start| start..start + query.len())
        })
        .flatten();
    let mut push = |len: usize, font, color| {
        if len > 0 {
            runs.push(TextRun {
                len,
                font,
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    };
    if let Some(range) = match_range {
        push(range.start, normal_font.clone(), theme.text_tertiary);
        push(range.len(), emphasized_font, theme.text_secondary);
        push(
            matched.snippet.len().saturating_sub(range.end),
            normal_font,
            theme.text_tertiary,
        );
    } else {
        push(matched.snippet.len(), normal_font, theme.text_tertiary);
    }
    StyledText::new(text).with_runs(runs)
}

fn command_palette_row_height(item: &CommandPaletteItem) -> f32 {
    if item.content_match.is_some() {
        CONTENT_RESULT_ROW_HEIGHT
    } else {
        RESULT_ROW_HEIGHT
    }
}

fn should_show_command_palette_empty_state(result_count: usize, search_pending: bool) -> bool {
    result_count == 0 && !search_pending
}

fn should_keep_previous_command_palette_results(
    next_result_count: usize,
    search_pending: bool,
    previous_result_count: usize,
) -> bool {
    next_result_count == 0 && search_pending && previous_result_count > 0
}

fn command_palette_results_height(results: &[CommandPaletteItem], show_empty_state: bool) -> f32 {
    let content_height = if show_empty_state {
        EMPTY_RESULTS_HEIGHT
    } else {
        let mut previous_section = None;
        results
            .iter()
            .map(|item| {
                let header_height = if previous_section == Some(item.section) {
                    0.0
                } else {
                    previous_section = Some(item.section);
                    SECTION_HEADER_HEIGHT
                };
                header_height + command_palette_row_height(item)
            })
            .sum()
    };
    content_height + RESULTS_BOTTOM_PADDING
}

pub(super) struct CommandPaletteUi {
    search: Entity<TextInput>,
    open: bool,
    focus_generation: u64,
    previous_focus: Option<FocusHandle>,
    results: Vec<CommandPaletteItem>,
    message_searches: QueryCache<String, Vec<crate::persistence::SessionMessageMatch>>,
    active_message_query: Option<String>,
    message_matches_query: Option<String>,
    message_matches: HashMap<Uuid, crate::persistence::SessionMessageMatch>,
    message_search_pending: bool,
    selected: usize,
    scroll: ScrollHandle,
    matcher: Matcher,
}

impl CommandPaletteUi {
    pub(super) fn new(search: Entity<TextInput>) -> Self {
        Self {
            search,
            open: false,
            focus_generation: 0,
            previous_focus: None,
            results: Vec::new(),
            message_searches: QueryCache::new(MESSAGE_SEARCH_CACHE_CAPACITY),
            active_message_query: None,
            message_matches_query: None,
            message_matches: HashMap::new(),
            message_search_pending: false,
            selected: 0,
            scroll: ScrollHandle::new(),
            matcher: crate::composer_complete::matcher(),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }
}

impl Waku {
    pub(super) fn toggle_command_palette_action(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.open {
            self.close_command_palette(window, cx);
        } else {
            self.open_command_palette(window, cx);
        }
    }

    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open_menus = self
            .menus
            .borrow()
            .values()
            .filter(|menu| menu.is_open())
            .cloned()
            .collect::<Vec<_>>();
        // A focused picker input disappears when its menu closes. Restore to a
        // durable surface instead of remembering that soon-detached handle.
        self.command_palette.previous_focus = if open_menus.is_empty() {
            window.focused(cx)
        } else if self.settings_page.is_some() {
            Some(self.settings_focus.clone())
        } else {
            Some(self.composer_focus(cx))
        };

        self.command_palette.open = true;
        self.command_palette.focus_generation =
            self.command_palette.focus_generation.wrapping_add(1);
        self.command_palette.message_searches.clear();
        self.command_palette.active_message_query = None;
        self.command_palette.message_matches_query = None;
        self.command_palette.message_matches.clear();
        self.command_palette.message_search_pending = false;
        let focus_generation = self.command_palette.focus_generation;
        self.command_palette
            .search
            .update(cx, |input, cx| input.clear(cx));
        self.refresh_command_palette_results("", false, cx);

        // Closing an open GPUI menu can call its toggle observers back into
        // this entity, so release this action listener's mutable borrow first.
        if !open_menus.is_empty() {
            window.defer(cx, move |window, cx| {
                for menu in open_menus {
                    menu.close(window, cx);
                }
            });
        }

        // The palette is deferred onto GPUI's overlay plane. Wait for that
        // subtree to join the dispatch tree before handing focus to its input.
        let focus = self.command_palette.search.read(cx).focus();
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let mut should_focus = false;
                let _ = weak.update(cx, |this, _| {
                    should_focus = this.command_palette.open
                        && this.command_palette.focus_generation == focus_generation;
                });
                if should_focus {
                    window.focus(&focus, cx);
                }
            });
        });
        cx.notify();
    }

    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.command_palette.open {
            return;
        }
        self.command_palette.open = false;
        self.command_palette.focus_generation =
            self.command_palette.focus_generation.wrapping_add(1);
        self.command_palette.active_message_query = None;
        self.command_palette.message_search_pending = false;
        if let Some(previous_focus) = self.command_palette.previous_focus.take() {
            window.focus(&previous_focus, cx);
        }
        cx.notify();
    }

    pub(super) fn refresh_command_palette_localized_text(&mut self, cx: &mut Context<Self>) {
        self.command_palette.search.update(cx, |input, cx| {
            input.set_placeholder(tr!("command_palette.placeholder"), cx)
        });
        if self.command_palette.open {
            let query = self.command_palette.search.read(cx).content().to_owned();
            self.refresh_command_palette_results(&query, true, cx);
        }
    }

    pub(super) fn command_palette_query_edited(&mut self, query: &str, cx: &mut Context<Self>) {
        if !self.command_palette.open {
            return;
        }
        let query = query.trim().to_owned();
        self.command_palette.active_message_query = (!query.is_empty()).then(|| query.clone());
        let fetch = if query.is_empty() {
            self.command_palette.message_matches_query = None;
            self.command_palette.message_matches.clear();
            self.command_palette.message_search_pending = false;
            None
        } else {
            match self.command_palette.message_searches.read(&query) {
                Query::Ready(matches) => {
                    self.command_palette.message_matches_query = Some(query.clone());
                    self.command_palette.message_matches = matches
                        .iter()
                        .cloned()
                        .map(|matched| (matched.session_id, matched))
                        .collect();
                    self.command_palette.message_search_pending = false;
                    None
                }
                Query::Pending => {
                    self.command_palette.message_search_pending = true;
                    None
                }
                Query::Missing(token) => {
                    self.command_palette.message_search_pending = true;
                    Some(token)
                }
            }
        };
        self.refresh_command_palette_results(&query, false, cx);
        cx.notify();

        let Some(token) = fetch else {
            return;
        };
        let search = self
            .store
            .session_message_search(query.clone(), MESSAGE_SEARCH_LIMIT);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(MESSAGE_SEARCH_DEBOUNCE)
                .await;
            let current = this
                .update(cx, |this, _| {
                    this.command_palette.open
                        && this.command_palette.active_message_query.as_deref()
                            == Some(query.as_str())
                })
                .unwrap_or(false);
            if !current {
                let _ = this.update(cx, |this, _| {
                    this.command_palette.message_searches.abandon(token)
                });
                return;
            }

            let matches = cx
                .background_executor()
                .spawn(async move { search().unwrap_or_default() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this
                    .command_palette
                    .message_searches
                    .fulfill(token, matches)
                {
                    return;
                }
                if !this.command_palette.open
                    || this.command_palette.active_message_query.as_deref() != Some(query.as_str())
                {
                    return;
                }
                let Query::Ready(matches) = this.command_palette.message_searches.read(&query)
                else {
                    return;
                };
                this.command_palette.message_matches_query = Some(query.clone());
                this.command_palette.message_matches = matches
                    .iter()
                    .cloned()
                    .map(|matched| (matched.session_id, matched))
                    .collect();
                this.command_palette.message_search_pending = false;
                this.refresh_command_palette_results(&query, false, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn command_palette_commands(&self, searching: bool) -> Vec<CommandPaletteItem> {
        let display_section = |suggested| {
            if searching {
                PaletteSection::Commands
            } else {
                suggested
            }
        };
        let mut order = 0usize;
        let mut next = || {
            let current = order;
            order += 1;
            current
        };
        let mut commands = vec![
            CommandPaletteItem::command(
                display_section(PaletteSection::Suggested),
                tr!("command_palette.new_task"),
                "icons/pencil.svg",
                Some(crate::platform::primary_shortcut("⌘N", "Ctrl+N")),
                PaletteAction::NewTask,
                "new task session chat conversation start",
                next(),
            ),
            CommandPaletteItem::command(
                display_section(PaletteSection::Suggested),
                tr!("command_palette.open_project"),
                "icons/folder.svg",
                Some(crate::platform::primary_shortcut("⌘O", "Ctrl+O")),
                PaletteAction::OpenProject,
                "open add folder project workspace repository repo",
                next(),
            ),
        ];

        let can_choose_model = self
            .selected_session()
            .is_some_and(|session| session.can_choose_model(session.provider));
        if can_choose_model {
            commands.push(CommandPaletteItem::command(
                display_section(PaletteSection::Suggested),
                tr!("command_palette.choose_model"),
                "icons/bot.svg",
                Some(crate::platform::primary_shortcut("⌘/", "Ctrl+/")),
                PaletteAction::ChooseModel,
                "choose change select model provider agent",
                next(),
            ));
        }

        commands.push(CommandPaletteItem::command(
            PaletteSection::Commands,
            tr!("menu.focus_composer"),
            "icons/pencil.svg",
            Some(crate::platform::primary_shortcut("⌘L", "Ctrl+L")),
            PaletteAction::FocusComposer,
            "focus composer prompt input message",
            next(),
        ));
        if self.usage_meter_available() {
            commands.push(CommandPaletteItem::command(
                PaletteSection::Commands,
                tr!("menu.toggle_usage_panel"),
                "icons/command.svg",
                Some(crate::platform::primary_shortcut("⌘U", "Ctrl+U")),
                PaletteAction::ToggleUsage,
                "toggle usage limits rate quota panel",
                next(),
            ));
        }
        commands.push(CommandPaletteItem::command(
            PaletteSection::Commands,
            tr!("command_palette.collapse_sidebar_groups"),
            "icons/command.svg",
            None,
            PaletteAction::CollapseSidebarGroups,
            "collapse close fold all sidebar groups projects dates history",
            next(),
        ));
        commands.extend([
            CommandPaletteItem::command(
                PaletteSection::Commands,
                tr!(if self.sidebar_visible {
                    "command_palette.hide_sidebar"
                } else {
                    "command_palette.show_sidebar"
                }),
                "icons/panel-left.svg",
                Some(crate::platform::primary_shortcut("⌘B", "Ctrl+B")),
                PaletteAction::ToggleSidebar,
                "toggle show hide left sidebar history tasks",
                next(),
            ),
            CommandPaletteItem::command(
                PaletteSection::Commands,
                tr!(if self.right_panel_visible {
                    "command_palette.hide_right_panel"
                } else {
                    "command_palette.show_right_panel"
                }),
                "icons/panel-right.svg",
                Some(crate::platform::primary_shortcut("⇧⌘B", "Ctrl+Shift+B")),
                PaletteAction::ToggleRightPanel,
                "toggle show hide right panel files diff terminal browser",
                next(),
            ),
        ]);

        for (page, label_key, icon, keywords) in [
            (
                SettingsPage::General,
                "settings.general",
                "icons/settings.svg",
                "settings preferences general local privacy updates",
            ),
            (
                SettingsPage::Appearance,
                "settings.appearance",
                "icons/appearance.svg",
                "settings preferences appearance theme language light dark",
            ),
            (
                SettingsPage::Providers,
                "settings.providers",
                "icons/bot.svg",
                "settings preferences providers agents models cli",
            ),
            (
                SettingsPage::Skills,
                "settings.skills",
                "icons/package.svg",
                "settings preferences skills library create disable agent skill",
            ),
            (
                SettingsPage::Usage,
                "settings.usage",
                "icons/chart-column.svg",
                "settings preferences usage tokens cost history",
            ),
            (
                SettingsPage::Daemon,
                "settings.daemon",
                "icons/server.svg",
                "settings preferences daemon server remote web network origin token port",
            ),
            (
                SettingsPage::ComputerUse,
                "settings.computer_use",
                "icons/cursor-spark.svg",
                "settings preferences computer use accessibility screen recording",
            ),
        ] {
            if !page.is_visible_in_navigation() {
                continue;
            }
            commands.push(CommandPaletteItem::command(
                PaletteSection::Settings,
                crate::i18n::translate(label_key),
                icon,
                (page == SettingsPage::General)
                    .then_some(crate::platform::primary_shortcut("⌘,", "Ctrl+,")),
                PaletteAction::OpenSettings(page),
                keywords,
                next(),
            ));
        }
        commands
    }

    fn command_palette_task_candidates(&self) -> Vec<CommandPaletteItem> {
        let projects = self
            .state
            .projects
            .iter()
            .map(|project| {
                (
                    project.id,
                    (
                        project.display_name(),
                        project.path.to_string_lossy().into_owned(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        self.state
            .sessions
            .iter()
            .filter(|session| session.has_started())
            .enumerate()
            .map(|(order, session)| {
                let (project, project_path) = projects
                    .get(&session.project_id)
                    .cloned()
                    .unwrap_or_else(|| (tr!("project.no_project_name"), String::new()));
                let (workspace_path, branch) = match &session.workspace {
                    SessionWorkspace::Local => (String::new(), None),
                    SessionWorkspace::NewWorktree { base_branch } => {
                        (String::new(), base_branch.as_deref())
                    }
                    SessionWorkspace::Worktree { path, branch } => {
                        (path.to_string_lossy().into_owned(), Some(branch.as_str()))
                    }
                };
                let mut details = vec![project.clone()];
                if let Some(branch) = branch {
                    details.push(format!("#{branch}"));
                }
                if Some(session.id) == self.state.selected_session {
                    details.push(tr!("command_palette.current"));
                }
                let detail = details.join(" · ");
                let label = session.display_title().to_owned();
                let content_match = self
                    .command_palette
                    .message_matches
                    .get(&session.id)
                    .cloned();
                CommandPaletteItem {
                    section: PaletteSection::Tasks,
                    search_text: format!(
                        "{label} {project} {project_path} {workspace_path} {} {} {} {} task session chat conversation",
                        branch.unwrap_or_default(),
                        session.provider.short_name(),
                        session.provider.display_name(),
                        session.model.as_deref().unwrap_or_default(),
                    ),
                    label,
                    detail: Some(detail),
                    icon: PaletteIcon::Provider(session.provider),
                    shortcut: None,
                    action: PaletteAction::SelectTask(session.id),
                    content_match,
                    order,
                    recency: session.updated_at,
                }
            })
            .collect()
    }

    fn refresh_command_palette_results(
        &mut self,
        query: &str,
        preserve_selection: bool,
        _cx: &App,
    ) {
        let query = query.trim();
        if query.is_empty() {
            self.command_palette.results = self.command_palette_commands(false);
            self.command_palette.selected = 0;
            self.command_palette.scroll.scroll_to_item(0);
            return;
        }

        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let message_matches_are_current =
            self.command_palette.message_matches_query.as_deref() == Some(query);
        let mut utf32 = Vec::new();
        let mut tasks = self
            .command_palette_task_candidates()
            .into_iter()
            .filter_map(|mut item| {
                let metadata_score = pattern.score(
                    Utf32Str::new(&item.search_text, &mut utf32),
                    &mut self.command_palette.matcher,
                );
                let content_score = item.content_match.as_ref().and_then(|matched| {
                    pattern.score(
                        Utf32Str::new(&matched.snippet, &mut utf32),
                        &mut self.command_palette.matcher,
                    )
                });
                let current_content_match =
                    message_matches_are_current && item.content_match.is_some();
                if content_score.is_none() && !current_content_match {
                    // Pending queries retain the last resolved transcript
                    // snapshot, but an unrelated old excerpt should neither
                    // keep a row alive nor be displayed under the new query.
                    item.content_match = None;
                }
                metadata_score
                    .into_iter()
                    .chain(content_score)
                    .max()
                    .or_else(|| current_content_match.then_some(0))
                    .map(|score| ScoredPaletteItem { score, item })
            })
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(b.item.recency.cmp(&a.item.recency))
                .then(a.item.order.cmp(&b.item.order))
        });
        tasks.truncate(MAX_TASK_RESULTS);

        let mut commands = self
            .command_palette_commands(true)
            .into_iter()
            .filter_map(|item| {
                pattern
                    .score(
                        Utf32Str::new(&item.search_text, &mut utf32),
                        &mut self.command_palette.matcher,
                    )
                    .map(|score| ScoredPaletteItem { score, item })
            })
            .collect::<Vec<_>>();
        commands.sort_by(|a, b| {
            a.item
                .section
                .query_rank()
                .cmp(&b.item.section.query_rank())
                .then(b.score.cmp(&a.score))
                .then(a.item.order.cmp(&b.item.order))
        });

        let selected_action = preserve_selection.then(|| {
            self.command_palette
                .results
                .get(self.command_palette.selected)
                .map(|item| item.action.clone())
        });
        let mut scored_results = tasks;
        scored_results.extend(commands);
        // Stable sorting keeps each section's existing score/recency order.
        scored_results.sort_by_key(|scored| scored.item.section.query_rank());
        let next_results = scored_results
            .into_iter()
            .map(|scored| scored.item)
            .collect::<Vec<_>>();
        // This is the palette equivalent of TanStack Query's
        // `keepPreviousData`: never replace useful rows with a transient blank
        // frame while the transcript query is still in flight.
        if should_keep_previous_command_palette_results(
            next_results.len(),
            self.command_palette.message_search_pending,
            self.command_palette.results.len(),
        ) {
            self.command_palette.selected = 0;
            self.command_palette.scroll.scroll_to_item(0);
            return;
        }
        self.command_palette.results = next_results;
        self.command_palette.selected = selected_action
            .flatten()
            .and_then(|action| {
                self.command_palette
                    .results
                    .iter()
                    .position(|item| item.action == action)
            })
            .unwrap_or(0);
        let scroll_index = self.command_palette_scroll_index(self.command_palette.selected);
        self.command_palette.scroll.scroll_to_item(scroll_index);
    }

    fn command_palette_scroll_index(&self, selected: usize) -> usize {
        let mut headers = 0;
        let mut previous = None;
        for item in self.command_palette.results.iter().take(selected + 1) {
            if previous != Some(item.section) {
                headers += 1;
                previous = Some(item.section);
            }
        }
        selected + headers
    }

    fn move_command_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.command_palette.results.len();
        let Some(next) = next_selection_index(self.command_palette.selected, len, delta) else {
            return;
        };
        self.command_palette.selected = next;
        self.command_palette
            .scroll
            .scroll_to_item(self.command_palette_scroll_index(next));
        cx.notify();
    }

    fn set_command_palette_selection(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.command_palette.results.len() && self.command_palette.selected != index {
            self.command_palette.selected = index;
            cx.notify();
        }
    }

    fn execute_command_palette_selection(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = index.unwrap_or(self.command_palette.selected);
        let Some(action) = self
            .command_palette
            .results
            .get(index)
            .map(|item| item.action.clone())
        else {
            return;
        };

        self.close_command_palette(window, cx);
        match action {
            PaletteAction::NewTask => self.new_session_action(&NewSession, window, cx),
            PaletteAction::OpenProject => self.new_project_action(&NewProject, window, cx),
            PaletteAction::FocusComposer => self.focus_composer_action(&FocusComposer, window, cx),
            PaletteAction::CollapseSidebarGroups => self.collapse_all_sidebar_groups(cx),
            PaletteAction::ToggleSidebar => self.toggle_sidebar_action(&ToggleSidebar, window, cx),
            PaletteAction::ToggleRightPanel => {
                self.toggle_right_panel_action(&ToggleRightPanel, window, cx)
            }
            PaletteAction::OpenSettings(page) => {
                self.open_settings_action(&OpenSettings, window, cx);
                self.open_settings_page(page, cx);
            }
            PaletteAction::SelectTask(session_id) => {
                self.settings_page = None;
                self.select_session(session_id, cx);
                let focus = self.composer_focus(cx);
                window.focus(&focus, cx);
            }
            PaletteAction::ChooseModel | PaletteAction::ToggleUsage => {
                // These popovers are rendered by the composer. If the command
                // came from Settings, reveal one normal app frame first so its
                // persistent menu handle and anchor bounds are current.
                self.settings_page = None;
                let focus = self.composer_focus(cx);
                window.focus(&focus, cx);
                let weak = cx.entity().downgrade();
                let choose_model = matches!(action, PaletteAction::ChooseModel);
                window.on_next_frame(move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if choose_model {
                            this.toggle_model_picker_action(&ToggleModelPicker, window, cx)
                        } else {
                            this.toggle_usage_panel_action(&ToggleUsagePanel, window, cx)
                        }
                    });
                });
            }
        }
    }

    pub(super) fn render_command_palette(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.command_palette.open {
            return None;
        }
        let theme = Theme::current(cx);
        let viewport_height = f32::from(window.viewport_size().height);
        let top = (viewport_height * 0.09).clamp(48.0, 72.0);
        let card_max_height = (viewport_height - top - 36.0)
            .max(SEARCH_ROW_HEIGHT)
            .min(MAX_CARD_HEIGHT);
        let selected = self
            .command_palette
            .selected
            .min(self.command_palette.results.len().saturating_sub(1));
        let search_query = self.command_palette.search.read(cx).content().to_owned();
        let show_empty_state = should_show_command_palette_empty_state(
            self.command_palette.results.len(),
            self.command_palette.message_search_pending,
        );
        let results_height =
            command_palette_results_height(&self.command_palette.results, show_empty_state)
                .min((card_max_height - SEARCH_ROW_HEIGHT).max(0.0));
        let card_height = SEARCH_ROW_HEIGHT + results_height;

        let mut results = div()
            .id("command-palette-results")
            .h(px(results_height))
            .flex_none()
            .overflow_y_scroll()
            .track_scroll(&self.command_palette.scroll)
            .px(px(8.0))
            .pb(px(8.0));

        if show_empty_state {
            results = results.child(
                div()
                    .h(px(EMPTY_RESULTS_HEIGHT))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(icon("icons/search.svg", 18.0, theme.text_ghost))
                    .child(
                        div()
                            .mt(px(12.0))
                            .text_size(sp(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_secondary)
                            .child(tr!("command_palette.no_results")),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(sp(12.5))
                            .text_color(theme.text_tertiary)
                            .child(tr!("command_palette.no_results_hint")),
                    ),
            );
        } else {
            let mut previous_section = None;
            for (index, item) in self.command_palette.results.iter().enumerate() {
                if previous_section != Some(item.section) {
                    results = results.child(
                        div()
                            .h(px(SECTION_HEADER_HEIGHT))
                            .px(px(9.0))
                            .pt(px(10.0))
                            .flex()
                            .items_center()
                            .text_size(sp(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_tertiary)
                            .child(item.section.label()),
                    );
                    previous_section = Some(item.section);
                }

                let highlighted = index == selected;
                let icon_color = match item.icon {
                    PaletteIcon::Asset(_) => theme.text_secondary,
                    PaletteIcon::Provider(provider) => provider_color(&theme, provider),
                };
                let icon_path = match item.icon {
                    PaletteIcon::Asset(path) => path,
                    PaletteIcon::Provider(provider) => provider_icon(provider),
                };
                let detail = item.detail.clone();
                let content_match = item.content_match.clone();
                let shortcut = item.shortcut;
                results = results.child(
                    div()
                        .id(SharedString::from(format!("command-palette-row-{index}")))
                        .h(px(command_palette_row_height(item)))
                        .px(px(11.0))
                        .rounded(px(9.0))
                        .border_1()
                        .border_color(if highlighted {
                            theme.border_strong
                        } else {
                            gpui::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .cursor_default()
                        .when(highlighted, |row| row.bg(theme.overlay_strong))
                        .hover(|row| row.bg(theme.overlay))
                        .active(|row| row.opacity(0.82))
                        .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                            if *hovering {
                                this.set_command_palette_selection(index, cx);
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.execute_command_palette_selection(Some(index), window, cx);
                            cx.stop_propagation();
                        }))
                        .child(
                            div()
                                .size(px(20.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(icon_path, 16.0, icon_color)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .justify_center()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .items_baseline()
                                        .gap(px(7.0))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(sp(14.0))
                                                .font_weight(if highlighted {
                                                    FontWeight::MEDIUM
                                                } else {
                                                    FontWeight::NORMAL
                                                })
                                                .text_color(if highlighted {
                                                    theme.text
                                                } else {
                                                    theme.text_secondary
                                                })
                                                .child(item.label.clone()),
                                        )
                                        .when_some(detail, |row, detail| {
                                            row.child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(sp(12.5))
                                                    .text_color(theme.text_tertiary)
                                                    .child(detail),
                                            )
                                        }),
                                )
                                .when_some(content_match, |column, matched| {
                                    column.child(
                                        div()
                                            .min_w_0()
                                            .w_full()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_size(sp(12.5))
                                            .child(palette_content_match_text(
                                                &matched,
                                                &search_query,
                                                window,
                                                theme,
                                            )),
                                    )
                                }),
                        )
                        .when_some(shortcut, |row, shortcut| {
                            row.child(
                                div()
                                    .h(px(22.0))
                                    .min_w(px(28.0))
                                    .px(px(7.0))
                                    .rounded(px(7.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(theme.overlay_strong)
                                    .text_size(sp(12.5))
                                    .text_color(theme.text_tertiary)
                                    .child(shortcut),
                            )
                        }),
                );
            }
        }

        let card =
            div()
                .id("command-palette-card")
                .key_context("CommandPalette")
                .on_action(cx.listener(Self::toggle_command_palette_action))
                .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                    this.move_command_palette_selection(1, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                    this.move_command_palette_selection(-1, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                    this.move_command_palette_selection(isize::MIN, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                    this.move_command_palette_selection(isize::MAX, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| {
                    this.move_command_palette_selection(PAGE_STEP, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| {
                    this.move_command_palette_selection(-PAGE_STEP, cx)
                }))
                .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                    this.execute_command_palette_selection(None, window, cx)
                }))
                .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                    this.close_command_palette(window, cx)
                }))
                .w_full()
                .max_w(px(680.0))
                .h(px(card_height))
                .overflow_hidden()
                .rounded(px(15.0))
                .bg(theme.raised)
                .shadow_xl()
                .relative()
                .flex()
                .flex_col()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .h(px(SEARCH_ROW_HEIGHT))
                        .px(px(19.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(sp(15.5))
                        .text_color(theme.text)
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .child(self.command_palette.search.clone()),
                        ),
                )
                .child(results);

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.26)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.14)
        };
        let layer = div()
            .id("command-palette-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .px(px(24.0))
            .pt(px(top))
            .flex()
            .items_start()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_command_palette(window, cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(3).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, candidate: &str) -> Option<u32> {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = crate::composer_complete::matcher();
        let mut buf = Vec::new();
        pattern.score(Utf32Str::new(candidate, &mut buf), &mut matcher)
    }

    #[test]
    fn fuzzy_search_matches_command_words_and_initials() {
        assert!(score("open proj", "Open project folder repository").is_some());
        assert!(score("cmptr use", "Computer Use settings accessibility").is_some());
        assert!(score("totally absent", "Open project folder repository").is_none());
    }

    #[test]
    fn scroll_indexes_include_section_headers() {
        let sections = [
            PaletteSection::Tasks,
            PaletteSection::Tasks,
            PaletteSection::Commands,
            PaletteSection::Settings,
        ];
        let scroll_index = |selected: usize| {
            let mut headers = 0;
            let mut previous = None;
            for section in sections.iter().take(selected + 1) {
                if previous != Some(*section) {
                    headers += 1;
                    previous = Some(*section);
                }
            }
            selected + headers
        };
        assert_eq!(scroll_index(0), 1);
        assert_eq!(scroll_index(1), 2);
        assert_eq!(scroll_index(2), 4);
        assert_eq!(scroll_index(3), 6);
    }

    #[test]
    fn arrows_wrap_while_page_and_boundary_keys_clamp() {
        assert_eq!(next_selection_index(0, 5, -1), Some(4));
        assert_eq!(next_selection_index(4, 5, 1), Some(0));
        assert_eq!(next_selection_index(3, 5, PAGE_STEP), Some(4));
        assert_eq!(next_selection_index(1, 5, -PAGE_STEP), Some(0));
        assert_eq!(next_selection_index(2, 5, isize::MIN), Some(0));
        assert_eq!(next_selection_index(2, 5, isize::MAX), Some(4));
        assert_eq!(next_selection_index(0, 0, 1), None);
    }

    #[test]
    fn searched_commands_rank_before_tasks() {
        assert!(PaletteSection::Commands.query_rank() < PaletteSection::Tasks.query_rank());
        assert!(PaletteSection::Tasks.query_rank() < PaletteSection::Settings.query_rank());
    }

    #[test]
    fn no_results_appears_only_after_background_search_settles() {
        assert!(!should_show_command_palette_empty_state(0, true));
        assert!(should_show_command_palette_empty_state(0, false));
        assert!(!should_show_command_palette_empty_state(1, false));
        assert!(should_keep_previous_command_palette_results(0, true, 1));
        assert!(!should_keep_previous_command_palette_results(0, false, 1));
        assert!(!should_keep_previous_command_palette_results(1, true, 1));
    }

    #[test]
    fn result_height_hugs_rows_and_section_headers() {
        let item = |section, order| {
            CommandPaletteItem::command(
                section,
                format!("Item {order}"),
                "icons/search.svg",
                None,
                PaletteAction::NewTask,
                "",
                order,
            )
        };
        let items = vec![
            item(PaletteSection::Tasks, 0),
            item(PaletteSection::Tasks, 1),
            item(PaletteSection::Commands, 2),
        ];
        assert_eq!(
            command_palette_results_height(&items, false),
            SECTION_HEADER_HEIGHT * 2.0 + RESULT_ROW_HEIGHT * 3.0 + RESULTS_BOTTOM_PADDING
        );
        assert_eq!(
            command_palette_results_height(&[], true),
            EMPTY_RESULTS_HEIGHT + RESULTS_BOTTOM_PADDING
        );
    }

    #[test]
    fn render_reads_only_the_cached_result_snapshot() {
        let source = include_str!("./command_palette.rs");
        let start = source
            .find("\n    pub(super) fn render_command_palette(")
            .expect("render function must exist");
        let end = source[start..]
            // Match from the final newline only so this accepts both LF and
            // CRLF checkouts.
            .find("\n#[cfg(test)]")
            .map(|offset| start + offset)
            .expect("test module marker must exist");
        let render = &source[start..end];
        for forbidden in [
            "refresh_command_palette_results(",
            "command_palette_task_candidates(",
            "session_message_search(",
            "background_executor(",
            "std::fs",
            "Command::new",
            "read_dir",
        ] {
            assert!(
                !render.contains(forbidden),
                "palette render must not call `{forbidden}`"
            );
        }
    }
}
