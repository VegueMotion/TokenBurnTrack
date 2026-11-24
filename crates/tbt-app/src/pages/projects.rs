use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window, div, prelude::*, px,
};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tbt_core::{Micros, ProjectId};
use tbt_store::{BranchStats, ProjectStats, QueryFilter, TokenSums};

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent, relative_time};
use crate::charts::format::{fmt_count, fmt_micros, fmt_pct, fmt_tokens, micros_to_usd};
use crate::charts::time::time_zone;
use crate::data::{self, DataSource, NO_DATA_MESSAGE};
use crate::palette::categorical;
use crate::ui::{self, Col};

pub const NO_PROJECT: &str = "(no project)";
pub const NO_BRANCH: &str = "(no branch)";
const TOP_N: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSort {
    Project,
    Sessions,
    Calls,
    Tokens,
    Cost,
    LastActive,
}

const EXPANDER: Col = Col::fixed("", 14.0);
const COLUMNS: [(ProjectSort, Col); 6] = [
    (ProjectSort::Project, Col::flex("Project")),
    (ProjectSort::Sessions, Col::num("Sessions", 70.0)),
    (ProjectSort::Calls, Col::num("Calls", 70.0)),
    (ProjectSort::Tokens, Col::num("Tokens", 80.0)),
    (ProjectSort::Cost, Col::num("Cost", 90.0)),
    (ProjectSort::LastActive, Col::num("Last active", 100.0)),
];

pub fn default_desc(key: ProjectSort) -> bool {
    key != ProjectSort::Project
}

pub fn project_label(p: &ProjectStats) -> String {
    p.name
        .clone()
        .filter(|n| !n.is_empty())
        .or_else(|| {
            p.path.as_deref().and_then(|path| {
                path.trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
        })
        .or_else(|| p.path.clone())
        .unwrap_or_else(|| NO_PROJECT.to_owned())
}

pub fn branch_label(b: &BranchStats) -> &str {
    b.branch
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(NO_BRANCH)
}

pub fn is_no_project(p: &ProjectStats) -> bool {
    p.id == ProjectId::NONE || p.path.is_none()
}

fn compare(a: &ProjectStats, b: &ProjectStats, key: ProjectSort) -> Ordering {
    match key {
        ProjectSort::Project => project_label(a)
            .to_lowercase()
            .cmp(&project_label(b).to_lowercase()),
        ProjectSort::Sessions => a.sessions.cmp(&b.sessions),
        ProjectSort::Calls => a.events.cmp(&b.events),
        ProjectSort::Tokens => a.tokens.total().cmp(&b.tokens.total()),
        ProjectSort::Cost => a.cost.cmp(&b.cost),
        ProjectSort::LastActive => a.last_active.cmp(&b.last_active),
    }
}

pub fn sort_projects(projects: &[ProjectStats], key: ProjectSort, desc: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..projects.len()).collect();
    idx.sort_by(|&i, &j| {
        let (a, b) = (&projects[i], &projects[j]);
        if key == ProjectSort::Project {
            let none = is_no_project(a).cmp(&is_no_project(b));
            if none != Ordering::Equal {
                return none;
            }
        }
        let o = compare(a, b, key);
        let o = if desc { o.reverse() } else { o };
        o.then_with(|| a.id.cmp(&b.id))
    });
    idx
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopProject {
    pub index: usize,
    pub label: String,
    pub cost: Micros,
    pub fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TopProjects {
    pub bars: Vec<TopProject>,
    pub rest_count: usize,
    pub rest_cost: Micros,
}

pub fn top_by_cost(projects: &[ProjectStats], n: usize) -> TopProjects {
    let order = sort_projects(projects, ProjectSort::Cost, true);
    let max = order
        .first()
        .map(|&i| micros_to_usd(projects[i].cost))
        .unwrap_or(0.0);
    let bars = order
        .iter()
        .take(n)
        .filter(|&&i| projects[i].cost > Micros::ZERO)
        .map(|&i| TopProject {
            index: i,
            label: project_label(&projects[i]),
            cost: projects[i].cost,
            fraction: if max > 0.0 {
                micros_to_usd(projects[i].cost) / max
            } else {
                0.0
            },
        })
        .collect::<Vec<_>>();
    let shown: HashSet<usize> = bars.iter().map(|b| b.index).collect();
    let rest: Vec<&ProjectStats> = (0..projects.len())
        .filter(|i| !shown.contains(i))
        .map(|i| &projects[i])
        .collect();
    TopProjects {
        bars,
        rest_count: rest.len(),
        rest_cost: rest.iter().map(|p| p.cost).sum(),
    }
}

pub fn share(part: Micros, whole: Micros) -> f64 {
    if whole.0 > 0 {
        (part.0 as f64 / whole.0 as f64).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub fn branch_total(p: &ProjectStats) -> Micros {
    p.branches.iter().map(|b| b.cost).sum()
}

pub fn branch_note(p: &ProjectStats) -> Option<String> {
    if p.branches.is_empty() {
        return Some(
            "No branch breakdown: these calls ran here from sessions started in another directory"
                .to_owned(),
        );
    }
    let total = branch_total(p);
    (total != p.cost).then(|| {
        format!(
            "Branches add up to {} (by session start directory) vs {} by call directory",
            fmt_micros(total),
            fmt_micros(p.cost)
        )
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectTotals {
    pub projects: usize,
    pub sessions: u64,
    pub events: u64,
    pub tokens: TokenSums,
    pub cost: Micros,
}

pub fn totals(projects: &[ProjectStats]) -> ProjectTotals {
    projects.iter().fold(
        ProjectTotals {
            projects: projects.len(),
            ..Default::default()
        },
        |mut t, p| {
            t.sessions += p.sessions;
            t.events += p.events;
            t.tokens.input += p.tokens.input;
            t.tokens.output += p.tokens.output;
            t.tokens.cache_write += p.tokens.cache_write;
            t.tokens.cache_read += p.tokens.cache_read;
            t.cost += p.cost;
            t
        },
    )
}

pub struct ProjectsData {
    pub projects: Vec<ProjectStats>,
    pub tz: TimeZone,
    pub no_data: bool,
}

impl ProjectsData {
    pub fn load(data: &dyn DataSource, filter: &QueryFilter) -> anyhow::Result<ProjectsData> {
        let projects = data.project_stats(filter)?;
        let no_data = projects.is_empty() && !data.has_data()?;
        Ok(ProjectsData {
            projects,
            tz: time_zone(&filter.time_zone),
            no_data,
        })
    }
}

pub struct ProjectsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<ProjectsData>,
    sort: ProjectSort,
    desc: bool,
    expanded: HashSet<ProjectId>,
    _load: Task<()>,
    _subscription: Subscription,
}

impl ProjectsPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription =
            cx.subscribe(&app_state, |this, _, _: &AppStateEvent, cx| this.reload(cx));
        let mut page = ProjectsPage {
            app_state,
            data,
            state: Loadable::Loading,
            sort: ProjectSort::Cost,
            desc: true,
            expanded: HashSet::new(),
            _load: Task::ready(()),
            _subscription: subscription,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.app_state.read(cx).query_filter(data::now());
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || ProjectsData::load(&*data, &filter),
            |this, state, _| this.state = state,
        );
    }

    fn sort_by(&mut self, key: ProjectSort, cx: &mut Context<Self>) {
        if self.sort == key {
            self.desc = !self.desc;
        } else {
            self.sort = key;
            self.desc = default_desc(key);
        }
        cx.notify();
    }

    fn toggle(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        cx.notify();
    }

    fn render_top(&self, projects: &[ProjectStats], cx: &Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (fg, muted, track) = (t.foreground, t.muted_foreground, t.border.opacity(0.4));
        let top = top_by_cost(projects, TOP_N);
        let title = format!("Top {} projects by cost", top.bars.len().max(1));
        ui::card(title, cx)
            .when(top.bars.is_empty(), |d| {
                d.child(ui::muted_text("No priced usage in range", cx))
            })
            .children(top.bars.iter().map(|b| {
                let p = &projects[b.index];
                h_flex()
                    .gap_3()
                    .items_center()
                    .text_sm()
                    .child(
                        div()
                            .w(px(180.))
                            .flex_none()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(if is_no_project(p) { muted } else { fg })
                            .child(b.label.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(ui::bar_cell(b.fraction, categorical(0), track)),
                    )
                    .child(
                        div()
                            .w(px(80.))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .text_color(fg)
                            .child(fmt_micros(b.cost)),
                    )
            }))
            .when(top.rest_count > 0, |d| {
                d.child(div().text_xs().text_color(muted).child(format!(
                    "+ {} more {} · {}",
                    top.rest_count,
                    if top.rest_count == 1 {
                        "project"
                    } else {
                        "projects"
                    },
                    fmt_micros(top.rest_cost)
                )))
            })
            .into_any_element()
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        h_flex()
            .w_full()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(t.border)
            .text_xs()
            .text_color(t.muted_foreground)
            .child(EXPANDER.cell(""))
            .children(COLUMNS.iter().enumerate().map(|(i, (key, col))| {
                let key = *key;
                let arrow = match (self.sort == key, self.desc) {
                    (false, _) => "",
                    (true, true) => " ▼",
                    (true, false) => " ▲",
                };
                col.cell(
                    div()
                        .id(("project-sort", i))
                        .cursor_pointer()
                        .when(self.sort == key, |d| d.text_color(t.foreground))
                        .hover(|s| s.text_color(t.foreground))
                        .on_click(cx.listener(move |this, _, _, cx| this.sort_by(key, cx)))
                        .child(format!("{}{arrow}", col.title)),
                )
            }))
    }

    fn render_project(
        &self,
        ix: usize,
        p: &ProjectStats,
        tz: &TimeZone,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> AnyElement {
        let t = cx.theme();
        let (fg, muted, border, hover_bg, track) = (
            t.foreground,
            t.muted_foreground,
            t.border.opacity(0.4),
            t.foreground.opacity(0.04),
            t.border.opacity(0.4),
        );
        let open = self.expanded.contains(&p.id);
        let none = is_no_project(p);
        let id = p.id;
        let name = v_flex()
            .min_w_0()
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(if none { muted } else { fg })
                    .when(!none, |d| d.font_medium())
                    .child(project_label(p)),
            )
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(muted)
                    .child(
                        p.path
                            .clone()
                            .unwrap_or_else(|| "Sessions without a working directory".into()),
                    ),
            );
        let last = p
            .last_active
            .map(|ts| relative_time(now, ts))
            .unwrap_or_else(|| "–".into());
        let last_title = p.last_active.map(|ts| {
            ts.to_zoned_datetime(tz.clone())
                .strftime("%b %-d, %H:%M")
                .to_string()
        });
        let cells: [AnyElement; 6] = [
            name.into_any_element(),
            fmt_count(p.sessions).into_any_element(),
            fmt_count(p.events).into_any_element(),
            fmt_tokens(p.tokens.total()).into_any_element(),
            div()
                .text_color(fg)
                .child(fmt_micros(p.cost))
                .into_any_element(),
            v_flex()
                .items_end()
                .child(last)
                .children(last_title.map(|s| div().text_xs().text_color(muted).child(s)))
                .into_any_element(),
        ];
        let row = h_flex()
            .id(("project", ix))
            .w_full()
            .gap_3()
            .px_3()
            .py_2()
            .items_center()
            .text_sm()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(id, cx)))
            .child(EXPANDER.cell(div().text_color(muted).child(if open { "▾" } else { "▸" })))
            .children(COLUMNS.iter().zip(cells).map(|((_, c), cell)| c.cell(cell)));
        let base = branch_total(p);
        let branches = open.then(|| {
            v_flex()
                .pb_2()
                .children(p.branches.iter().map(|b| {
                    let frac = share(b.cost, base);
                    h_flex()
                        .w_full()
                        .gap_3()
                        .px_3()
                        .py_1()
                        .items_center()
                        .text_xs()
                        .child(EXPANDER.cell(""))
                        .child(
                            Col::flex("").cell(
                                h_flex()
                                    .gap_2()
                                    .pl_3()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(180.))
                                            .flex_none()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .font_family("monospace")
                                            .text_color(if b.branch.is_some() { fg } else { muted })
                                            .child(branch_label(b).to_owned()),
                                    )
                                    .child(div().flex_1().max_w(px(220.)).child(ui::bar_cell(
                                        frac,
                                        categorical(0),
                                        track,
                                    )))
                                    .child(div().text_color(muted).child(fmt_pct(frac))),
                            ),
                        )
                        .child(
                            COLUMNS[1]
                                .1
                                .cell(div().text_color(muted).child(fmt_count(b.sessions))),
                        )
                        .child(COLUMNS[2].1.cell(""))
                        .child(COLUMNS[3].1.cell(""))
                        .child(COLUMNS[4].1.cell(fmt_micros(b.cost)))
                        .child(COLUMNS[5].1.cell(""))
                }))
                .children(branch_note(p).map(|note| {
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_3()
                        .text_xs()
                        .text_color(muted)
                        .child(EXPANDER.cell(""))
                        .child(div().pl_3().child(note))
                }))
        });
        v_flex()
            .border_b_1()
            .border_color(border)
            .child(row)
            .children(branches)
            .into_any_element()
    }

    fn render_ready(&self, d: &ProjectsData, cx: &Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border) = (t.muted_foreground, t.border);
        let now = data::now();
        let sum = totals(&d.projects);
        let order = sort_projects(&d.projects, self.sort, self.desc);
        let rows: Vec<AnyElement> = order
            .iter()
            .map(|&i| self.render_project(i, &d.projects[i], &d.tz, now, cx))
            .collect();
        v_flex()
            .gap_4()
            .p_4()
            .child(self.render_top(&d.projects, cx))
            .child(div().text_sm().text_color(muted).child(format!(
                "{} {} · {} sessions · {} calls · {} tokens · {} · click a row for branches",
                sum.projects,
                if sum.projects == 1 {
                    "project"
                } else {
                    "projects"
                },
                fmt_count(sum.sessions),
                fmt_count(sum.events),
                fmt_tokens(sum.tokens.total()),
                fmt_micros(sum.cost),
            )))
            .child(
                v_flex()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .overflow_hidden()
                    .child(self.render_header(cx))
                    .children(rows),
            )
            .into_any_element()
    }
}

impl Render for ProjectsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(d) if d.no_data => ui::status_message(NO_DATA_MESSAGE, cx),
            Loadable::Ready(d) if d.projects.is_empty() => {
                ui::status_message("No usage in range", cx)
            }
            Loadable::Ready(d) => self.render_ready(d, cx),
        };
        ui::page_scroll("projects-scroll", body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: Option<&str>, sessions: u64, cost: i64) -> BranchStats {
        BranchStats {
            branch: name.map(str::to_owned),
            sessions,
            cost: Micros(cost),
        }
    }

    fn project(id: i64, path: Option<&str>, cost: i64, events: u64, last: &str) -> ProjectStats {
        ProjectStats {
            id: ProjectId(id),
            path: path.map(str::to_owned),
            name: path.and_then(|p| p.rsplit('/').next()).map(str::to_owned),
            sessions: events / 2,
            events,
            tokens: TokenSums {
                input: events * 10,
                output: events,
                ..Default::default()
            },
            cost: Micros(cost),
            last_active: Some(last.parse().unwrap()),
            branches: vec![branch(Some("main"), events / 2, cost)],
        }
    }

    fn sample() -> Vec<ProjectStats> {
        vec![
            project(3, Some("/src/zeta"), 5_000_000, 10, "2026-11-20T10:00:00Z"),
            project(0, None, 9_000_000, 4, "2026-11-22T10:00:00Z"),
            project(1, Some("/src/Alpha"), 1_000_000, 40, "2026-11-21T10:00:00Z"),
            project(2, Some("/src/beta"), 5_000_000, 6, "2026-11-19T10:00:00Z"),
        ]
    }

    #[test]
    fn labels() {
        let s = sample();
        assert_eq!(project_label(&s[0]), "zeta");
        assert_eq!(project_label(&s[1]), NO_PROJECT);
        let mut p = s[2].clone();
        p.name = None;
        p.path = Some("/src/gamma/".into());
        assert_eq!(project_label(&p), "gamma");
        p.path = Some("/".into());
        assert_eq!(project_label(&p), "/");
        assert_eq!(branch_label(&branch(None, 1, 1)), NO_BRANCH);
        assert_eq!(branch_label(&branch(Some(""), 1, 1)), NO_BRANCH);
        assert_eq!(branch_label(&branch(Some("dev"), 1, 1)), "dev");
        assert!(is_no_project(&s[1]));
        assert!(!is_no_project(&s[0]));
    }

    #[test]
    fn sorts_by_column_with_stable_ties() {
        let s = sample();
        assert_eq!(sort_projects(&s, ProjectSort::Cost, true), vec![1, 3, 0, 2]);
        assert_eq!(
            sort_projects(&s, ProjectSort::Cost, false),
            vec![2, 3, 0, 1]
        );
        assert_eq!(
            sort_projects(&s, ProjectSort::Calls, true),
            vec![2, 0, 3, 1]
        );
        assert_eq!(
            sort_projects(&s, ProjectSort::LastActive, true),
            vec![1, 2, 0, 3]
        );
        assert_eq!(
            sort_projects(&s, ProjectSort::Tokens, false),
            vec![1, 3, 0, 2]
        );
    }

    #[test]
    fn name_sort_is_case_insensitive_and_keeps_no_project_last() {
        let s = sample();
        assert_eq!(
            sort_projects(&s, ProjectSort::Project, false),
            vec![2, 3, 0, 1]
        );
        assert_eq!(
            sort_projects(&s, ProjectSort::Project, true),
            vec![0, 3, 2, 1]
        );
        assert!(default_desc(ProjectSort::Cost));
        assert!(!default_desc(ProjectSort::Project));
    }

    #[test]
    fn top_by_cost_keeps_rest_and_skips_free_projects() {
        let mut s = sample();
        s.push(project(4, Some("/src/free"), 0, 2, "2026-11-19T10:00:00Z"));
        let top = top_by_cost(&s, 2);
        assert_eq!(
            top.bars.iter().map(|b| b.index).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(top.bars[0].label, NO_PROJECT);
        assert_eq!(top.bars[0].fraction, 1.0);
        assert!((top.bars[1].fraction - 5.0 / 9.0).abs() < 1e-9);
        assert_eq!(top.rest_count, 3);
        assert_eq!(top.rest_cost, Micros(6_000_000));

        let all = top_by_cost(&s, 10);
        assert_eq!(all.bars.len(), 4);
        assert_eq!(all.rest_count, 1);
        assert_eq!(all.rest_cost, Micros::ZERO);
        assert_eq!(top_by_cost(&[], 5), TopProjects::default());
    }

    #[test]
    fn branch_notes_flag_attribution_gaps() {
        let mut p = sample().remove(0);
        assert_eq!(branch_total(&p), p.cost);
        assert_eq!(branch_note(&p), None);
        p.branches.push(branch(None, 1, 2_000_000));
        assert_eq!(branch_total(&p), Micros(7_000_000));
        assert_eq!(
            branch_note(&p).unwrap(),
            "Branches add up to $7.00 (by session start directory) vs $5.00 by call directory"
        );
        p.branches.clear();
        assert!(branch_note(&p).unwrap().starts_with("No branch breakdown"));
    }

    #[test]
    fn totals_and_shares() {
        let s = sample();
        let t = totals(&s);
        assert_eq!(t.projects, 4);
        assert_eq!(t.events, 60);
        assert_eq!(t.sessions, 30);
        assert_eq!(t.cost, Micros(20_000_000));
        assert_eq!(t.tokens.total(), 660);
        assert_eq!(share(Micros(1), Micros(4)), 0.25);
        assert_eq!(share(Micros(1), Micros::ZERO), 0.0);
        assert_eq!(share(Micros(5), Micros(4)), 1.0);
    }

    #[test]
    fn mock_branches_add_up_to_project_cost() {
        use crate::data::MockData;
        use tbt_core::TimeRange;
        use tbt_store::BucketSize;
        let now: Timestamp = "2026-11-23T15:20:00Z".parse().unwrap();
        let data = MockData::new(5, now);
        let filter = QueryFilter::new(
            TimeRange::new("2026-11-01T00:00:00Z".parse().unwrap(), now),
            BucketSize::Day,
        );
        let d = ProjectsData::load(&data, &filter).unwrap();
        assert!(!d.projects.is_empty());
        for p in &d.projects {
            let branch_cost: Micros = p.branches.iter().map(|b| b.cost).sum();
            assert_eq!(branch_cost, p.cost, "{}", project_label(p));
            let branch_sessions: u64 = p.branches.iter().map(|b| b.sessions).sum();
            assert!(branch_sessions >= p.sessions);
        }
    }
}
