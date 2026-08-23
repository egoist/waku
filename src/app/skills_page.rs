//! The Skills settings page: one library across every ecosystem's skill
//! trees, presented as a mail-style master–detail split — the list of skills
//! on the left, the selected skill's full detail on the right — with enable
//! and delete management.
//!
//! Discovery is filesystem work and lives on the background executor
//! ([`Waku::ensure_skills_catalog`]); frames read only the cached catalog.
//! Mutations are one-shot user actions — each a single rename, write, or
//! trash call — so they run synchronously in their click handlers and then
//! invalidate the catalog.

use std::path::Path;

use gpui::KeyBinding;

use super::composer::next_picker_highlight;
use crate::skills::{SkillEntry, SkillSource, SkillsCatalog};

use super::*;

/// Key context the left pane declares around its search field.
const SKILLS_PANE_CONTEXT: &str = "SkillsPane";

/// The search field while focused inside the pane. The field holds real focus
/// while `up`/`down` walk the list selection, the same claim-from-under-it
/// arrangement the settings sidebar uses.
const SKILLS_SEARCH_CONTEXT: &str = "SkillsPane > TextInput";

const SKILLS_LIST_WIDTH: f32 = 264.0;

fn skill_source_icon(source: SkillSource) -> &'static str {
    match source {
        SkillSource::Shared => "icons/package.svg",
        SkillSource::Provider(provider) => crate::ui::provider_icon(provider),
    }
}

fn skill_icon(skill: &SkillEntry) -> &'static str {
    if skill.installs.len() > 1 {
        "icons/package.svg"
    } else {
        skill_source_icon(skill.primary().source)
    }
}

/// A landed catalog older than this is rescanned when the page opens.
const SKILLS_RESCAN_AFTER: Duration = Duration::from_secs(60);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNextEntry, Some(SKILLS_SEARCH_CONTEXT)),
        KeyBinding::new("up", SelectPreviousEntry, Some(SKILLS_SEARCH_CONTEXT)),
    ]);
}

/// One row of the virtualized skills list. Equality drives the prefix splice
/// in [`Waku::sync_skills_rows`]: a changed row — catalog identity, enabled
/// state, or selection — re-measures from that point on.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum SkillsRow {
    Section {
        label: SharedString,
        count: usize,
    },
    Skill {
        index: usize,
        row_key: u64,
        selected: bool,
    },
}

impl Waku {
    // ── Catalog ────────────────────────────────────────────────────────────

    /// Start a background library scan unless a current-enough catalog (or an
    /// in-flight scan) already covers it. Results from superseded scans are
    /// discarded by generation.
    pub(super) fn ensure_skills_catalog(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.skills_scan_pending {
            return;
        }
        let fresh = self.skills_catalog.is_some()
            && self
                .skills_scanned_at
                .is_some_and(|scanned| scanned.elapsed() < SKILLS_RESCAN_AFTER);
        if !force && fresh {
            return;
        }
        self.skills_scan_pending = true;
        self.skills_scan_generation += 1;
        let generation = self.skills_scan_generation;
        let projects = self.skill_scan_projects();
        let daemon = self.daemon.client();
        cx.spawn(async move |this, cx| {
            let catalog = cx
                .background_executor()
                .spawn(async move {
                    match daemon.request(
                        Uuid::nil(),
                        Uuid::nil(),
                        waku_client::Command::LoadSkills { projects },
                    )? {
                        waku_client::ResponsePayload::SkillsCatalog { catalog } => Ok(catalog),
                        _ => anyhow::bail!("the daemon returned an invalid skills response"),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.skills_scan_generation != generation {
                    // A mutation invalidated this scan; the flag and the
                    // result now belong to the newer one.
                    return;
                }
                this.skills_scan_pending = false;
                match catalog {
                    Ok(catalog) => {
                        this.skills_catalog = Some(Rc::new(catalog));
                        this.skills_scanned_at = Some(Instant::now());
                    }
                    Err(error) => this.show_toast(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Drop any in-flight scan's claim and rescan now. Called after every
    /// mutation, so the library on screen always re-reads the disk it just
    /// changed.
    fn invalidate_skills_catalog(&mut self, cx: &mut Context<Self>) {
        self.skills_scan_generation += 1;
        self.skills_scan_pending = false;
        self.ensure_skills_catalog(true, cx);
    }

    /// `(display name, path)` per scannable project. Projectless workspaces
    /// are generated directories that never hold curated skills.
    fn skill_scan_projects(&self) -> Vec<(String, PathBuf)> {
        self.state
            .projects
            .iter()
            .filter(|project| !project.is_projectless())
            .map(|project| (project.display_name(), project.path.clone()))
            .collect()
    }

    // ── Mutations ──────────────────────────────────────────────────────────

    /// Flip every copy of the skill keyed by `primary_dir`. A skill installed
    /// into several roots is one skill; the switch converges all of them.
    fn toggle_skill_enabled(
        &mut self,
        primary_dir: PathBuf,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let dirs = self
            .skills_catalog
            .as_ref()
            .and_then(|catalog| {
                catalog
                    .skills
                    .iter()
                    .find(|skill| skill.primary().dir == primary_dir)
            })
            .map(|skill| {
                skill
                    .installs
                    .iter()
                    .map(|install| install.dir.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![primary_dir.clone()]);
        // The switch answers immediately; the rescan confirms from disk.
        if let Some(catalog) = self.skills_catalog.as_ref() {
            let mut updated = catalog.as_ref().clone();
            for skill in &mut updated.skills {
                if skill.primary().dir == primary_dir {
                    skill.enabled = enabled;
                    skill.row_key = skill.row_key.wrapping_add(1);
                    for install in &mut skill.installs {
                        install.enabled = enabled;
                        install.skill_file = install.dir.join(if enabled {
                            crate::skills::SKILL_FILE
                        } else {
                            crate::skills::DISABLED_SKILL_FILE
                        });
                    }
                }
            }
            self.skills_catalog = Some(Rc::new(updated));
        }
        self.skills_scan_generation += 1;
        self.skills_scan_pending = false;
        let daemon = self.daemon.client();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    daemon.request(
                        Uuid::nil(),
                        Uuid::nil(),
                        waku_client::Command::SetSkillsEnabled { dirs, enabled },
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.show_toast(tr!("skills.toggle_failed", error = error));
                }
                this.invalidate_skills_catalog(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Trash every copy of the skill keyed by `primary_dir`.
    fn delete_skill(&mut self, primary_dir: PathBuf, cx: &mut Context<Self>) {
        let entry = self.skills_catalog.as_ref().and_then(|catalog| {
            catalog
                .skills
                .iter()
                .find(|skill| skill.primary().dir == primary_dir)
                .cloned()
        });
        let name = entry
            .as_ref()
            .map(|skill| skill.name.clone())
            .unwrap_or_else(|| {
                primary_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let dirs = entry
            .map(|skill| {
                skill
                    .installs
                    .iter()
                    .map(|install| install.dir.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![primary_dir.clone()]);
        if self.skills_selected.as_ref() == Some(&primary_dir) {
            self.skills_selected = None;
        }
        if let Some(catalog) = self.skills_catalog.as_ref() {
            let mut updated = catalog.as_ref().clone();
            updated
                .skills
                .retain(|skill| skill.primary().dir != primary_dir);
            self.skills_catalog = Some(Rc::new(updated));
        }
        self.skills_delete_arming = None;
        self.skills_scan_generation += 1;
        self.skills_scan_pending = false;
        let daemon = self.daemon.client();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    daemon.request(
                        Uuid::nil(),
                        Uuid::nil(),
                        waku_client::Command::TrashSkills { dirs },
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.show_success_toast(tr!("skills.deleted_toast", name = name)),
                    Err(error) => {
                        this.show_toast(tr!("skills.delete_failed", error = error));
                    }
                }
                this.invalidate_skills_catalog(cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn select_skill(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.skills_selected = Some(dir);
        self.skills_delete_arming = None;
        // Each skill's detail starts at its own top; a scroll position
        // carried over would land mid-document.
        self.skills_detail_scroll.set_offset(gpui::Point::default());
        cx.notify();
    }

    /// Walk the selection through the visible rows, the way a mailbox walks
    /// its message list. The search field keeps focus so typing keeps
    /// narrowing.
    fn step_skill_selection(&mut self, key: &str, cx: &mut Context<Self>) {
        let rows = self.skills_rows.borrow();
        let entries: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .filter_map(|(row_index, row)| match row {
                SkillsRow::Skill { index, .. } => Some((row_index, *index)),
                _ => None,
            })
            .collect();
        drop(rows);
        if entries.is_empty() {
            return;
        }
        let Some(catalog) = self.skills_catalog.clone() else {
            return;
        };
        let current = self.skills_selected.as_ref().and_then(|selected| {
            entries.iter().position(|(_, index)| {
                catalog
                    .skills
                    .get(*index)
                    .is_some_and(|skill| &skill.primary().dir == selected)
            })
        });
        let Some(next) = next_picker_highlight(current, entries.len(), key) else {
            return;
        };
        let (row_index, catalog_index) = entries[next];
        let Some(skill) = catalog.skills.get(catalog_index) else {
            return;
        };
        self.skills_selected = Some(skill.primary().dir.clone());
        self.skills_delete_arming = None;
        self.skills_detail_scroll.set_offset(gpui::Point::default());
        self.skills_list_state.scroll_to_reveal_item(row_index);
        cx.notify();
    }

    // ── Page ───────────────────────────────────────────────────────────────

    pub(super) fn render_skills_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let catalog = self.skills_catalog.clone();
        let query = self.skills_search.read(cx).content().trim().to_lowercase();

        let Some(catalog) = catalog else {
            // The startup prefetch makes this a first-frames-only state.
            return div()
                .size_full()
                .pt(px(12.0))
                .child(skills_status_row(&theme, tr!("skills.scanning")))
                .into_any_element();
        };
        if catalog.skills.is_empty() {
            return skills_empty_state(&theme).into_any_element();
        }

        let indices = self.visible_skill_indices(&catalog, &query);
        // The detail pane never sits empty while skills exist: the stored
        // selection wins when visible, the first visible row otherwise.
        let effective = self
            .skills_selected
            .as_ref()
            .filter(|selected| {
                indices.iter().any(|index| {
                    catalog
                        .skills
                        .get(*index)
                        .is_some_and(|skill| &&skill.primary().dir == selected)
                })
            })
            .cloned()
            .or_else(|| {
                indices
                    .first()
                    .and_then(|index| catalog.skills.get(*index))
                    .map(|skill| skill.primary().dir.clone())
            });
        let rows = self.skills_rows_from(&catalog, &indices, effective.as_deref());
        self.sync_skills_rows(&rows);

        let detail: AnyElement = if let Some(skill) = effective.as_ref().and_then(|dir| {
            catalog
                .skills
                .iter()
                .find(|skill| &skill.primary().dir == dir)
        }) {
            self.render_skill_detail_pane(skill, &theme, cx)
                .into_any_element()
        } else {
            skills_detail_placeholder(&theme).into_any_element()
        };

        div()
            .size_full()
            .min_h_0()
            .flex()
            .child(self.render_skills_list_column(&catalog, &query, &rows, &theme, cx))
            .child(div().flex_1().min_w_0().flex().flex_col().child(detail))
            .into_any_element()
    }

    fn render_skills_list_column(
        &self,
        catalog: &SkillsCatalog,
        query: &str,
        rows: &[SkillsRow],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let shown = rows
            .iter()
            .filter(|row| matches!(row, SkillsRow::Skill { .. }))
            .count();
        let total = catalog.skills.len();
        let footer = if !query.is_empty() || self.skills_source_filter.is_some() {
            tr!("skills.filter_caption", shown = shown, total = total)
        } else {
            let disabled = catalog.disabled_count();
            let mut caption = if total == 1 {
                tr!("skills.count_one")
            } else {
                tr!("skills.count_many", count = total)
            };
            if disabled > 0 {
                caption.push_str(" · ");
                caption.push_str(&tr!("skills.count_disabled", count = disabled));
            }
            caption
        };

        let body: AnyElement = if rows.is_empty() {
            skills_status_row(
                theme,
                if total == 0 {
                    tr!("skills.empty_title")
                } else {
                    tr!("skills.no_match")
                },
            )
            .into_any_element()
        } else {
            let entity = cx.entity().downgrade();
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    div().px(px(8.0)).size_full().child(
                        list(self.skills_list_state.clone(), move |index, _window, cx| {
                            entity
                                .upgrade()
                                .map(|entity| {
                                    entity.update(cx, |this, cx| this.skills_row(index, cx))
                                })
                                .unwrap_or_else(|| div().into_any_element())
                        })
                        .size_full(),
                    ),
                )
                .child(scrollbar::vertical(
                    &self.skills_list_state,
                    &self.skills_scrollbar,
                ))
                .into_any_element()
        };

        div()
            .key_context(SKILLS_PANE_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNextEntry, _, cx| {
                this.step_skill_selection("down", cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousEntry, _, cx| {
                this.step_skill_selection("up", cx);
            }))
            .w(px(SKILLS_LIST_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_none()
                    .px(px(10.0))
                    // Level with the detail pane's header: its icon tile spans
                    // 18–56, so the 28px search row centers on the same axis.
                    .pt(px(22.0))
                    .pb(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .child(
                        TextField::new("skills-search-field", self.skills_search.clone())
                            .icon("icons/search.svg", 13.0)
                            .w_full(),
                    )
                    .child(self.render_skills_source_filter(catalog, theme, cx)),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .h(px(26.0))
                    .px(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(sp(12.5))
                    .text_color(theme.text_ghost)
                    .child(SharedString::from(footer)),
            )
    }

    /// The provider filter over the list. `None` — every source — is the
    /// default; the menu shows per-source counts so an empty pick is never a
    /// surprise.
    fn render_skills_source_filter(
        &self,
        catalog: &SkillsCatalog,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.skills_source_filter;
        let chip_label = match current {
            None => tr!("skills.filter_all"),
            Some(source) => source.label(),
        };
        let mut counts: HashMap<SkillSource, usize> = HashMap::new();
        for skill in &catalog.skills {
            let mut seen = Vec::new();
            for install in &skill.installs {
                if !seen.contains(&install.source) {
                    seen.push(install.source);
                    *counts.entry(install.source).or_default() += 1;
                }
            }
        }
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("skills-source-filter", cx);
        let sources = [
            SkillSource::Shared,
            SkillSource::Provider(ProviderKind::Claude),
            SkillSource::Provider(ProviderKind::Codex),
            SkillSource::Provider(ProviderKind::Cursor),
            SkillSource::Provider(ProviderKind::Fx),
            SkillSource::Provider(ProviderKind::OpenCode),
            SkillSource::Provider(ProviderKind::Pi),
            SkillSource::Provider(ProviderKind::OhMyPi),
            SkillSource::Provider(ProviderKind::Amp),
        ];
        dropdown_menu(
            MenuChip::new("skills-source-filter")
                .icon(
                    match current {
                        None => "icons/package.svg",
                        Some(source) => skill_source_icon(source),
                    },
                    theme.text_tertiary,
                )
                .label(chip_label)
                .outlined()
                .background(theme.raised)
                .height(px(26.0))
                .selected(handle.is_open())
                .w_full()
                .justify_between(),
            "skills-source-filter-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                let mut items = Vec::new();
                {
                    let weak = weak.clone();
                    items.push(
                        MenuItem::new(tr!("skills.filter_all"), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.skills_source_filter = None;
                                cx.notify();
                            });
                        })
                        .selected(current.is_none()),
                    );
                }
                items.push(MenuItem::Separator);
                for source in sources {
                    let weak = weak.clone();
                    let count = counts.get(&source).copied().unwrap_or(0);
                    let label = if count > 0 {
                        format!("{} · {count}", source.label())
                    } else {
                        source.label()
                    };
                    items.push(
                        MenuItem::new(label, move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.skills_source_filter = Some(source);
                                cx.notify();
                            });
                        })
                        .icon(skill_source_icon(source))
                        .selected(current == Some(source)),
                    );
                }
                items
            },
        )
    }

    // ── List rows ──────────────────────────────────────────────────────────

    /// Catalog indices the query and source filter leave visible, in catalog
    /// order. A grouped skill passes a source filter when any of its copies
    /// lives in that source's tree.
    fn visible_skill_indices(&self, catalog: &SkillsCatalog, query: &str) -> Vec<usize> {
        catalog
            .skills
            .iter()
            .enumerate()
            .filter(|(_, skill)| {
                self.skills_source_filter.is_none_or(|filter| {
                    skill
                        .installs
                        .iter()
                        .any(|install| install.source == filter)
                }) && (query.is_empty() || skill_matches(skill, query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The visible rows: a section per scope group, each holding its skills.
    fn skills_rows_from(
        &self,
        catalog: &SkillsCatalog,
        indices: &[usize],
        selected: Option<&Path>,
    ) -> Vec<SkillsRow> {
        let mut rows = Vec::new();
        let mut section_start: Option<usize> = None;
        let mut current: Option<&Option<String>> = None;
        for &index in indices {
            let Some(skill) = catalog.skills.get(index) else {
                continue;
            };
            if current != Some(&skill.project) {
                current = Some(&skill.project);
                if let Some(start) = section_start {
                    backfill_section_count(&mut rows, start);
                }
                section_start = Some(rows.len());
                let label = SharedString::from(match &skill.project {
                    Some(project) => project.clone(),
                    None => tr!("skills.section_user"),
                });
                rows.push(SkillsRow::Section { label, count: 0 });
            }
            rows.push(SkillsRow::Skill {
                index,
                row_key: skill.row_key,
                selected: selected == Some(skill.primary().dir.as_path()),
            });
        }
        if let Some(start) = section_start {
            backfill_section_count(&mut rows, start);
        }
        rows
    }

    /// Keep the virtualized list in sync with the freshly computed rows.
    /// Sharing a prefix keeps scroll position across filter keystrokes and
    /// selection moves; everything after the first change re-measures.
    fn sync_skills_rows(&self, rows: &[SkillsRow]) {
        let mut cached = self.skills_rows.borrow_mut();
        if cached.as_slice() == rows {
            return;
        }
        let prefix = cached
            .iter()
            .zip(rows.iter())
            .take_while(|(cached, fresh)| cached == fresh)
            .count();
        let old_count = cached.len();
        *cached = rows.to_vec();
        if old_count == 0 {
            self.skills_list_state.reset(rows.len());
        } else {
            self.skills_list_state
                .splice(prefix..old_count, rows.len() - prefix);
        }
    }

    /// One list row, built only while visible. Reads the per-frame row cache;
    /// a stale index from a frame racing a rescan renders empty rather than
    /// panicking.
    fn skills_row(&self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let rows = self.skills_rows.borrow();
        let Some(entry) = rows.get(row) else {
            return div().into_any_element();
        };
        match entry {
            SkillsRow::Section { label, count } => {
                let first = row == 0;
                div()
                    .w_full()
                    .pt(px(if first { 10.0 } else { 18.0 }))
                    .pb(px(4.0))
                    .px(px(9.0))
                    .flex()
                    .items_baseline()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(sp(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(label.to_uppercase())),
                    )
                    .child(
                        div()
                            .text_size(sp(12.5))
                            .text_color(theme.text_ghost)
                            .child(SharedString::from(count.to_string())),
                    )
                    .into_any_element()
            }
            SkillsRow::Skill {
                index, selected, ..
            } => {
                let index = *index;
                let selected = *selected;
                drop(rows);
                let catalog = self.skills_catalog.clone();
                let Some(skill) = catalog
                    .as_deref()
                    .and_then(|catalog| catalog.skills.get(index))
                else {
                    return div().into_any_element();
                };
                self.render_skills_list_row(skill, selected, &theme, cx)
            }
        }
    }

    fn render_skills_list_row(
        &self,
        skill: &SkillEntry,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dir = skill.primary().dir.clone();
        let enabled = skill.enabled;
        div()
            .w_full()
            .pb(px(1.0))
            .child(
                div()
                    .id(SharedString::from(format!("skill-item-{}", skill.row_key)))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .w_full()
                    .px(px(9.0))
                    .py(px(7.0))
                    .rounded(px(8.0))
                    .cursor_default()
                    .when(selected, |element| {
                        element.bg(theme.sidebar_item_background)
                    })
                    .when(!selected, |element| {
                        element.hover(|element| element.bg(theme.overlay))
                    })
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(
                        div()
                            .w(px(26.0))
                            .h(px(26.0))
                            .flex_none()
                            .rounded(px(6.0))
                            .bg(theme.overlay)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                skill_icon(skill),
                                13.0,
                                theme
                                    .text_secondary
                                    .opacity(if enabled { 1.0 } else { 0.45 }),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(sp(12.5))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if enabled {
                                                theme.text
                                            } else {
                                                theme.text_secondary
                                            })
                                            .child(SharedString::from(skill.name.clone())),
                                    )
                                    .when(!enabled, |element| {
                                        element.child(
                                            div()
                                                .flex_none()
                                                .text_size(sp(12.5))
                                                .text_color(theme.warning)
                                                .child(tr!("skills.disabled_badge")),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .mt(px(1.0))
                                    .text_size(sp(12.5))
                                    .text_color(theme.text_tertiary)
                                    .truncate()
                                    .child(SharedString::from(if skill.description.is_empty() {
                                        skill.sources_label()
                                    } else {
                                        skill.description.clone()
                                    })),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_skill(dir.clone(), cx);
                    })),
            )
            .into_any_element()
    }

    // ── Detail pane ────────────────────────────────────────────────────────

    fn render_skill_detail_pane(
        &self,
        skill: &SkillEntry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let dir = skill.primary().dir.clone();
        let skill_file = skill.primary().skill_file.clone();
        let enabled = skill.enabled;
        let armed = self.skills_delete_arming.as_ref() == Some(&dir);

        let scope_caption = match &skill.project {
            Some(project) => tr!("skills.scope_in_project", project = project),
            None => tr!("skills.scope_user_detail"),
        };
        let caption = format!("{} · {}", skill.sources_label(), scope_caption);

        let toggle = div()
            .id(SharedString::from(format!(
                "skill-enabled-{}",
                skill.row_key
            )))
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .w(px(36.0))
            .h(px(20.0))
            .p(px(2.0))
            .flex_none()
            .rounded_full()
            .cursor_default()
            .bg(if enabled { theme.inverse } else { theme.inset })
            .border_1()
            .border_color(if enabled {
                theme.inverse
            } else {
                theme.border_strong
            })
            .flex()
            .items_center()
            .when(enabled, |element| element.justify_end())
            .child(div().w(px(14.0)).h(px(14.0)).rounded_full().bg(if enabled {
                theme.on_inverse
            } else {
                theme.text_tertiary
            }))
            .on_click(cx.listener({
                let dir = dir.clone();
                move |this, _, _, cx| {
                    this.toggle_skill_enabled(dir.clone(), !enabled, cx);
                }
            }));

        let mut contents = Vec::new();
        if skill.supporting_files == 1 {
            contents.push(tr!("skills.file_count_one"));
        } else if skill.supporting_files > 1 {
            contents.push(tr!(
                "skills.file_count_many",
                count = skill.supporting_files
            ));
        }
        contents.push(format_bytes(skill.total_bytes));

        let mono_value = |value: String, size: f32| {
            div()
                .min_w_0()
                .truncate()
                .font_family(crate::md::render::MONO_FAMILY)
                .text_size(px(size.max(12.5)))
                .text_color(theme.text_secondary)
                .child(SharedString::from(value))
                .into_any_element()
        };
        let mut info_rows: Vec<(String, AnyElement)> = vec![(
            tr!("skills.detail_invoke"),
            mono_value(format!("/{}", skill.name), 10.5),
        )];
        // One location line per copy; a grouped skill labels each line with
        // the ecosystem that reads it.
        let grouped = skill.installs.len() > 1;
        for install in &skill.installs {
            info_rows.push((
                if grouped {
                    install.source.label()
                } else {
                    tr!("skills.detail_location")
                },
                mono_value(compact_path(&install.dir), 10.0),
            ));
        }
        info_rows.push((
            tr!("skills.detail_contents"),
            plain_info_value(theme, contents.join(" · ")),
        ));
        if let Some(modified_at) = skill.modified_at {
            info_rows.push((
                tr!("skills.detail_updated"),
                plain_info_value(
                    theme,
                    updated_label(unix_time().saturating_sub(modified_at)),
                ),
            ));
        }
        if let Some(tools) = &skill.allowed_tools {
            info_rows.push((tr!("skills.allowed_tools"), mono_value(tools.clone(), 10.0)));
        }
        let info_count = info_rows.len();
        let mut info = div().mt(px(16.0)).flex().flex_col();
        for (index, (label, value)) in info_rows.into_iter().enumerate() {
            info = info.child(skill_info_row(theme, label, value, index + 1 == info_count));
        }

        let action_button = |id: SharedString, icon_path: &'static str, label: String| {
            div()
                .id(id)
                .tab_index(0)
                .focus_visible(|style| style.border_color(theme.accent))
                .h(px(26.0))
                .px(px(10.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border_strong)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.0))
                .cursor_default()
                .text_size(sp(12.5))
                .text_color(theme.text_secondary)
                .hover(|element| element.bg(theme.overlay))
                .child(icon(icon_path, 11.0, theme.text_tertiary))
                .child(SharedString::from(label))
        };

        let open_button = action_button(
            SharedString::from(format!("skill-open-{}", skill.row_key)),
            "icons/pencil.svg",
            tr!("skills.open_file"),
        )
        .on_click(cx.listener({
            let skill_file = skill_file.clone();
            move |this, _, _, cx| {
                if this.daemon.is_remote() {
                    this.show_toast(tr!("errors.remote_host_path"));
                    cx.notify();
                } else {
                    crate::platform::open_with_default_app(&skill_file, cx);
                }
            }
        }));

        let reveal_button = action_button(
            SharedString::from(format!("skill-reveal-{}", skill.row_key)),
            "icons/folder.svg",
            tr!("skills.reveal"),
        )
        .on_click(cx.listener({
            let skill_file = skill_file.clone();
            move |this, _, _, cx| {
                if this.daemon.is_remote() {
                    this.show_toast(tr!("errors.remote_host_path"));
                    cx.notify();
                } else {
                    crate::platform::reveal_in_file_manager(&skill_file, cx);
                }
            }
        }));

        let copy_feedback_id = format!("skill-copy-{}", skill.row_key);
        let copied = self.control_was_copied(&copy_feedback_id);
        let copy_button = action_button(
            SharedString::from(copy_feedback_id.clone()),
            if copied {
                "icons/check.svg"
            } else {
                "icons/copy.svg"
            },
            if copied {
                tr!("common.copied")
            } else {
                tr!("skills.copy_path")
            },
        )
        .on_click(cx.listener({
            let dir = dir.clone();
            let copy_feedback_id = copy_feedback_id.clone();
            move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(dir.display().to_string()));
                this.show_control_copied(copy_feedback_id.clone(), cx);
            }
        }));

        let delete_button = div()
            .id(SharedString::from(format!(
                "skill-delete-{}",
                skill.row_key
            )))
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(26.0))
            .px(px(10.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(if armed {
                theme.danger
            } else {
                theme.border_strong
            })
            .when(armed, |element| element.bg(theme.danger.opacity(0.12)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .text_size(sp(12.5))
            .text_color(if armed {
                theme.danger
            } else {
                theme.text_secondary
            })
            .hover(|element| element.bg(theme.overlay).text_color(theme.danger))
            .child(icon(
                "icons/trash.svg",
                11.0,
                if armed {
                    theme.danger
                } else {
                    theme.text_tertiary
                },
            ))
            .child(if armed {
                tr!("skills.confirm_delete")
            } else {
                tr!("skills.delete")
            })
            .on_click(cx.listener({
                let dir = dir.clone();
                move |this, _, _, cx| {
                    if this.skills_delete_arming.as_ref() == Some(&dir) {
                        this.delete_skill(dir.clone(), cx);
                    } else {
                        this.skills_delete_arming = Some(dir.clone());
                        cx.notify();
                    }
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.skills_delete_arming.take().is_some() {
                    cx.notify();
                }
            }));

        // The skill's document, rendered with the transcript's own markdown
        // engine. The parse is cached per selected skill; re-rendering an
        // unchanged document costs `Rc` clones, not a re-parse.
        let palette = MarkdownPalette::from_theme(theme);
        let document: Option<AnyElement> = (!skill.body.is_empty()).then(|| {
            let mut cache = self.skills_detail_markdown.borrow_mut();
            let primary_dir = skill.primary().dir.clone();
            if !matches!(cache.as_ref(), Some((cached, _)) if cached == &primary_dir) {
                *cache = Some((primary_dir, MarkdownView::new()));
            }
            let (_, view) = cache.as_mut().expect("entry ensured above");
            view.set_text(&skill.body, false);
            let ctx = MarkdownCtx::new(
                format!("skill-md-{}", skill.row_key),
                &palette,
                self.scaled_markdown_metrics(MarkdownMetrics::COMPACT),
                self.skills_selection.clone(),
            );
            div()
                .mt(px(18.0))
                .pt(px(14.0))
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .font_family(crate::md::render::MONO_FAMILY)
                        .text_size(sp(12.5))
                        .text_color(theme.text_ghost)
                        .child("SKILL.md"),
                )
                .child(
                    div()
                        .mt(px(10.0))
                        .text_color(theme.text_secondary)
                        .children(md::render::markdown(view, &ctx)),
                )
                .into_any_element()
        });
        let selection_input = {
            let selection = self.skills_selection.clone();
            canvas(
                |_, _, _| (),
                move |_, _, window, _| md::render::install_selection_input(window, &selection),
            )
            .absolute()
            .w(px(0.0))
            .h(px(0.0))
        };

        let content = div()
            .id("skill-detail-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.skills_detail_scroll)
            .px(px(24.0))
            .pt(px(18.0))
            .pb(px(20.0))
            // Painted before the document, so the frame's selection registry
            // holds exactly the text elements this frame put on screen.
            .child(md::render::frame_reset(self.skills_selection.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .w(px(38.0))
                            .h(px(38.0))
                            .flex_none()
                            .rounded(px(9.0))
                            .bg(theme.overlay)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                skill_icon(skill),
                                18.0,
                                theme
                                    .text_secondary
                                    .opacity(if enabled { 1.0 } else { 0.45 }),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(sp(15.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if enabled {
                                                theme.text
                                            } else {
                                                theme.text_secondary
                                            })
                                            .child(SharedString::from(skill.name.clone())),
                                    )
                                    .when(!enabled, |element| {
                                        element.child(
                                            div()
                                                .flex_none()
                                                .text_size(sp(12.5))
                                                .text_color(theme.warning)
                                                .child(tr!("skills.disabled_badge")),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(sp(12.5))
                                    .text_color(theme.text_tertiary)
                                    .truncate()
                                    .child(SharedString::from(caption)),
                            ),
                    )
                    .child(toggle),
            )
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(sp(12.5))
                    .line_height(sp(17.0))
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(if skill.description.is_empty() {
                        tr!("skills.no_description")
                    } else {
                        skill.description.clone()
                    })),
            )
            .child(info)
            .when(skill.duplicates > 0, |element| {
                element.child(
                    div()
                        .mt(px(12.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(icon("icons/alert.svg", 11.0, theme.warning))
                        .child(div().text_size(sp(12.5)).text_color(theme.warning).child(
                            SharedString::from(if skill.duplicates == 1 {
                                tr!("skills.duplicate_one")
                            } else {
                                tr!("skills.duplicate_many", count = skill.duplicates)
                            }),
                        )),
                )
            })
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(7.0))
                    .child(open_button)
                    .child(reveal_button)
                    .child(copy_button)
                    .child(div().flex_1())
                    .child(delete_button),
            )
            .children(document)
            .child(selection_input);

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(content)
            .child(scrollbar::vertical(
                &self.skills_detail_scroll,
                &self.skills_detail_scrollbar,
            ))
    }
}

fn skills_empty_state(theme: &Theme) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .px(px(40.0))
        .py(px(40.0))
        .child(
            div()
                .w(px(44.0))
                .h(px(44.0))
                .rounded(px(11.0))
                .bg(theme.overlay)
                .flex()
                .items_center()
                .justify_center()
                .child(icon("icons/package.svg", 21.0, theme.text_tertiary)),
        )
        .child(
            div()
                .text_size(sp(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(tr!("skills.empty_title")),
        )
        .child(
            div()
                .max_w(px(420.0))
                .text_size(sp(12.5))
                .line_height(sp(17.0))
                .text_color(theme.text_secondary)
                .text_center()
                .child(tr!("skills.empty_description")),
        )
}

fn skills_detail_placeholder(theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.0))
        .child(icon("icons/package.svg", 22.0, theme.text_ghost))
        .child(
            div()
                .text_size(sp(12.5))
                .text_color(theme.text_ghost)
                .child(tr!("skills.select_placeholder")),
        )
}

/// One label/value line of the detail pane's info table.
fn skill_info_row(theme: &Theme, label: String, value: AnyElement, last: bool) -> Div {
    div()
        .py(px(8.0))
        .when(!last, |element| {
            element.border_b_1().border_color(theme.border)
        })
        .flex()
        .items_baseline()
        .gap(px(12.0))
        .child(
            div()
                .w(px(84.0))
                .flex_none()
                .text_size(sp(12.5))
                .text_color(theme.text_tertiary)
                .child(SharedString::from(label)),
        )
        .child(div().flex_1().min_w_0().flex().child(value))
}

fn plain_info_value(theme: &Theme, value: String) -> AnyElement {
    div()
        .text_size(sp(12.5))
        .text_color(theme.text_secondary)
        .child(SharedString::from(value))
        .into_any_element()
}

/// Fill in a section header's count now that its span is known.
fn backfill_section_count(rows: &mut [SkillsRow], start: usize) {
    let count = rows.len() - start - 1;
    if let Some(SkillsRow::Section { count: slot, .. }) = rows.get_mut(start) {
        *slot = count;
    }
}

fn skill_matches(skill: &SkillEntry, query: &str) -> bool {
    skill.name.to_lowercase().contains(query)
        || skill.description.to_lowercase().contains(query)
        || skill
            .installs
            .iter()
            .any(|install| install.source.label().to_lowercase().contains(query))
        || skill
            .project
            .as_ref()
            .is_some_and(|project| project.to_lowercase().contains(query))
}

fn skills_status_row(theme: &Theme, message: String) -> Div {
    div()
        .px(px(18.0))
        .py(px(16.0))
        .text_size(sp(12.5))
        .text_color(theme.text_tertiary)
        .child(SharedString::from(message))
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Bare relative time for the Updated row. Precision beyond the day is noise.
fn updated_label(elapsed_seconds: u64) -> String {
    if elapsed_seconds < 90 {
        tr!("skills.updated_just_now")
    } else if elapsed_seconds < 3600 {
        tr!("skills.updated_minutes", count = elapsed_seconds / 60)
    } else if elapsed_seconds < 86_400 {
        tr!("skills.updated_hours", count = elapsed_seconds / 3600)
    } else {
        tr!("skills.updated_days", count = elapsed_seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_format_compactly() {
        assert_eq!(format_bytes(412), "412 B");
        assert_eq!(format_bytes(3_300), "3.2 KB");
        assert_eq!(format_bytes(1_200_000), "1.1 MB");
    }
}
