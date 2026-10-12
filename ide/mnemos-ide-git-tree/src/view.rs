// SPDX-License-Identifier: AGPL-3.0-only
//! `GitTreeView` — the git tree panel: branches, open pull requests, remotes,
//! tags and the commit graph.
//!
//! One flat, virtualised row list rather than nested widgets. `UniformList`
//! requires every row to be the same height, so sections, refs and commits all
//! render at `ROW_HEIGHT`; the alternative (a plain column) would re-render up
//! to `MAX_COMMITS` rows every frame.
//!
//! Interaction, mirroring VS Code:
//!   - click a section header      -> collapse / expand it
//!   - click a branch or tag       -> check it out
//!   - click a pull request        -> check out its head branch (fetching it
//!     from origin first if needed); fork PRs only explain themselves
//!   - click a PR's "open" chip    -> open it on GitHub in the system browser
//!   - click a commit              -> load its diff (selection)
//!   - click a selected commit's
//!     "Checkout" / "Branch" chip  -> detach HEAD there / branch from there
//!   - right-click a commit        -> check it out directly
//!
//! Checkout is deliberately a *second* click on commits: the first selects and
//! reveals the chips. A single stray click can then never detach HEAD. Branch
//! rows check out on first click because that is unambiguous and what was asked
//! for. Every checkout goes through libgit2's *safe* strategy, so it refuses
//! rather than discarding uncommitted work.
//!
//! Observer pattern mirrors `AgentsPanel`: the app owns the ViewHandle, calls
//! `set_layout` / `set_refs` when the repo changes, and subscribes to
//! `GitTreeEvent`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mnemos_core::ui::appearance::Appearance;
use mnemos_core::ui::theme::color::internal_colors;
use mnemos_ui_core::accessibility::AccessibilityContent;
use mnemos_ui_core::color::ColorU;
use mnemos_ui_core::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Fill, Flex,
    Hoverable, MainAxisSize, MouseStateHandle, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, Radius, Scrollable, ScrollableElement, ScrollbarWidth, ScrollStateHandle,
    Stack, Text, UniformList, UniformListState,
};
use mnemos_ui_core::geometry::vector::vec2f;
use mnemos_ui_core::fonts::FamilyId;
use mnemos_ui_core::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::lane_router::{Edge, EdgeKind, GraphLayout};
use crate::model::{CommitNode, RefKind, RefNode};
use crate::prs::{
    pr_checkout_plan, ChecksState, PrCheckout, PrSummary, ReviewState, DRAFT_LABEL, FORK_LABEL,
};

// ── layout constants ─────────────────────────────────────────────────────────

/// Rows are single-line and dense, like VS Code's Git Graph. `UniformList`
/// forces one height for every row, so a two-line commit row would also make
/// every section header and branch row 44px tall — which is what made the first
/// version look so sparse.
const ROW_HEIGHT: f32 = 24.0;
const ROW_V_PADDING: f32 = 2.0;
const ROW_H_PADDING: f32 = 8.0;
/// Width of each lane column in the graph gutter.
const LANE_WIDTH: f32 = 12.0;
/// Dot diameter for the commit circle.
const DOT_SIZE: f32 = 7.0;
/// Thickness of connector stubs (vertical/horizontal lines).
const CONNECTOR_W: f32 = 2.0;
/// Width of the left-edge accent strip on the selected row.
const SELECTION_ACCENT_W: f32 = 2.0;
const PILL_H_PAD: f32 = 5.0;
const PILL_V_PAD: f32 = 1.0;
const PILL_RADIUS: f32 = 3.0;
/// Indent applied to ref rows so they read as children of their section.
const REF_INDENT: f32 = 14.0;
/// Maximum number of branch-ref pills to show per row before truncating.
const MAX_BRANCH_PILLS: usize = 3;
/// Soft cap for the commit summary rendered inline. Real ellipsis happens at
/// element layout; this just protects against pathological multi-KB summaries.
const SUMMARY_MAX_CHARS: usize = 72;
/// PR rows also carry a head branch and up to four chips, so the title gets
/// less room than a commit summary.
const PR_TITLE_MAX_CHARS: usize = 44;
const PR_HEAD_MAX_CHARS: usize = 24;
const PANEL_MAX_HEIGHT: f32 = 420.0;
const PANEL_MAX_WIDTH: f32 = 460.0;

/// Glyphs are ASCII on purpose. The first version used triangles, a bullet and
/// fork symbols (U+25BE, U+25B8, U+25CF, U+2442, U+233E), and the UI font has
/// no coverage for them: they rendered as `*` and a stray superscript `v` on
/// Windows. The ahead/behind counters use `+` / `-` rather than arrows for the
/// same reason (bundled Roboto has no U+2191 / U+2193); their colour carries
/// the direction. Anything drawn here must exist in every font we ship, and
/// the tests at the bottom of this file check the labels stay ASCII.
const CHEVRON_OPEN: &str = "v";
const CHEVRON_CLOSED: &str = ">";
const CURRENT_MARKER: &str = "*";
const OPEN_PR_LABEL: &str = "open";

/// Counter for commits a branch has that its upstream lacks.
fn ahead_label(n: usize) -> String {
    format!("+{n}")
}

/// Counter for commits the upstream has that the branch lacks.
fn behind_label(n: usize) -> String {
    format!("-{n}")
}

// ── sections ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Branches,
    PullRequests,
    Remotes,
    Tags,
    Commits,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Branches => "BRANCHES",
            Section::PullRequests => "PULL REQUESTS",
            Section::Remotes => "REMOTES",
            Section::Tags => "TAGS",
            Section::Commits => "COMMITS",
        }
    }
}

/// One rendered line. Refs and commits carry an index into the backing slices
/// rather than a clone, so rebuilding rows stays cheap.
#[derive(Clone, Debug)]
enum TreeRow {
    Section { section: Section, count: usize },
    Ref { idx: usize },
    Pr { idx: usize },
    Commit { idx: usize },
}

// ── public events / actions ───────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum GitTreeEvent {
    /// User clicked a commit row. OID is the full 40-hex string.
    CommitSelected(String),
    /// User asked to check out a branch (local or remote short name).
    CheckoutBranch(String),
    /// User asked to check out a bare commit, detaching HEAD.
    CheckoutCommit(String),
    /// User asked to create a branch at a commit.
    CreateBranchAt(String),
    /// The dropdown just opened. The app refreshes the PR list on this
    /// (throttled), since that costs a network call.
    PanelOpened,
    /// A PR's head branch is not here yet: fetch `origin/<branch>`, then check
    /// it out. The name has passed `is_valid_branch_name`.
    FetchAndCheckoutBranch(String),
    /// Open a PR in the system browser. The app re-validates the URL.
    OpenPullRequest(String),
}

#[derive(Clone, Debug)]
pub enum GitTreeAction {
    SelectCommit(String),
    ToggleSection(Section),
    CheckoutBranch(String),
    CheckoutCommit(String),
    CreateBranchAt(String),
    /// Click on a PR row, by PR number.
    CheckoutPullRequest(u64),
    OpenPullRequest(String),
    /// Open/close the whole panel. It hangs off a compact trigger button rather
    /// than sitting inline — rendered inline it swallowed the header.
    TogglePanel,
}

// ── view struct ───────────────────────────────────────────────────────────────

/// Scrollable git tree: branches, remotes, tags, commit graph.
pub struct GitTreeView {
    /// Ordered newest-first commit list (same order as the layout sweep).
    commits: Arc<Vec<CommitNode>>,
    /// Branches and tags, already sorted by the model.
    refs: Arc<Vec<RefNode>>,
    /// Open PRs of the repo's origin, most recently updated first. Empty when
    /// gh is unavailable or origin is not on github.com.
    prs: Arc<Vec<PrSummary>>,
    /// Lane assignment per OID.
    commit_lanes: Arc<HashMap<String, u32>>,
    /// All directed edges for drawing connectors.
    edges: Arc<Vec<Edge>>,
    /// Maximum lane index used — determines gutter width.
    max_lane: u32,
    /// Selected commit OID (highlighted row).
    selected_oid: Option<String>,
    /// Where HEAD is — branch name, or short OID when detached.
    head_label: Option<String>,
    head_detached: bool,
    /// Sections the user has collapsed.
    collapsed: HashSet<Section>,
    /// Flattened rows, rebuilt whenever data or collapse state changes.
    rows: Arc<Vec<TreeRow>>,
    /// Transient message (e.g. a refused checkout), shown as a banner.
    status: Option<String>,
    /// Whether the dropdown panel is showing. Closed by default so the header
    /// shows only the compact branch button.
    is_open: bool,
    font_family: Option<FamilyId>,
    /// Scroll state shared with the `Scrollable` wrapper.
    scroll_state: ScrollStateHandle,
    /// Virtualised list state.
    list_state: UniformListState,
}

impl GitTreeView {
    pub fn new() -> Self {
        Self {
            commits: Arc::new(Vec::new()),
            refs: Arc::new(Vec::new()),
            prs: Arc::new(Vec::new()),
            commit_lanes: Arc::new(HashMap::new()),
            edges: Arc::new(Vec::new()),
            max_lane: 0,
            selected_oid: None,
            head_label: None,
            head_detached: false,
            collapsed: HashSet::new(),
            rows: Arc::new(Vec::new()),
            status: None,
            is_open: false,
            font_family: None,
            scroll_state: ScrollStateHandle::default(),
            list_state: UniformListState::new(),
        }
    }

    pub fn with_font(mut self, family: FamilyId) -> Self {
        self.font_family = Some(family);
        self
    }

    pub fn set_font(&mut self, family: FamilyId, ctx: &mut ViewContext<Self>) {
        self.font_family = Some(family);
        ctx.notify();
    }

    /// Primary setter — called by the app whenever the repo changes.
    ///
    /// Stores the layout + commits in `Arc` so they can be cheaply cloned into
    /// the `UniformList` closure (which must be `'static`).
    pub fn set_layout(
        &mut self,
        layout: GraphLayout,
        commits: Vec<CommitNode>,
        ctx: &mut ViewContext<Self>,
    ) {
        // Preserve selection only if the OID still exists.
        if let Some(ref oid) = self.selected_oid {
            if !layout.commit_lanes.contains_key(oid.as_str()) {
                self.selected_oid = None;
            }
        }
        self.max_lane = layout.max_lane;
        self.commit_lanes = Arc::new(layout.commit_lanes);
        self.edges = Arc::new(layout.edges);
        self.commits = Arc::new(commits);
        self.rebuild_rows();
        ctx.notify();
    }

    /// Push branches/tags and the current HEAD label into the tree.
    pub fn set_refs(
        &mut self,
        refs: Vec<RefNode>,
        head_label: Option<String>,
        head_detached: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        self.refs = Arc::new(refs);
        self.head_label = head_label;
        self.head_detached = head_detached;
        self.rebuild_rows();
        ctx.notify();
    }

    /// Push the repo's open pull requests. An empty list hides the section.
    pub fn set_pull_requests(&mut self, prs: Vec<PrSummary>, ctx: &mut ViewContext<Self>) {
        self.prs = Arc::new(prs);
        self.rebuild_rows();
        ctx.notify();
    }

    /// Whether the dropdown is showing. The app only refreshes PRs while it is.
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Show a one-line status banner — used for checkout failures, which are
    /// expected (a safe checkout refuses when the tree is dirty) and must not
    /// be swallowed.
    pub fn set_status(&mut self, status: Option<String>, ctx: &mut ViewContext<Self>) {
        self.status = status;
        ctx.notify();
    }

    /// Convenience setter for callers that have already run `assign_lanes` on
    /// the commit slice (lane index embedded in `CommitNode::lane`).
    /// Builds a minimal layout from the embedded lane values — no edge geometry.
    pub fn set_commits(&mut self, commits: Vec<CommitNode>, ctx: &mut ViewContext<Self>) {
        // Rebuild commit_lanes map from the embedded lane field.
        let mut commit_lanes = HashMap::with_capacity(commits.len());
        let mut max_lane = 0u32;
        for c in &commits {
            let lane = c.lane as u32;
            commit_lanes.insert(c.oid.clone(), lane);
            if lane > max_lane {
                max_lane = lane;
            }
        }

        if let Some(ref oid) = self.selected_oid {
            if !commit_lanes.contains_key(oid.as_str()) {
                self.selected_oid = None;
            }
        }

        self.max_lane = max_lane;
        self.commit_lanes = Arc::new(commit_lanes);
        self.edges = Arc::new(Vec::new()); // no edge geometry in this path
        self.commits = Arc::new(commits);
        self.rebuild_rows();
        ctx.notify();
    }

    /// Programmatically select a commit.
    pub fn select_commit(&mut self, oid: String, ctx: &mut ViewContext<Self>) {
        self.selected_oid = Some(oid);
        self.rebuild_rows();
        ctx.notify();
    }

    /// Flatten sections + visible children into the row list.
    fn rebuild_rows(&mut self) {
        self.rows = Arc::new(build_rows(
            &self.refs,
            self.prs.len(),
            self.commits.len(),
            &self.collapsed,
        ));
    }
}

/// Sections in display order, each followed by its visible children.
///
/// Sections with no members are omitted entirely rather than rendered
/// empty — a repo with no tags should not show a dead "TAGS" header. Pull
/// requests sit right after local branches: they are the other half of
/// "work in flight", and remotes and tags are mostly reference.
fn build_rows(
    refs: &[RefNode],
    pr_count: usize,
    commit_count: usize,
    collapsed: &HashSet<Section>,
) -> Vec<TreeRow> {
    let mut rows = Vec::with_capacity(refs.len() + pr_count + commit_count + 5);

    let push_section = |rows: &mut Vec<TreeRow>, section: Section, children: Vec<TreeRow>| {
        if children.is_empty() {
            return;
        }
        rows.push(TreeRow::Section {
            section,
            count: children.len(),
        });
        if !collapsed.contains(&section) {
            rows.extend(children);
        }
    };
    let refs_of = |kind: RefKind| -> Vec<TreeRow> {
        refs.iter()
            .enumerate()
            .filter(|(_, r)| r.kind == kind)
            .map(|(idx, _)| TreeRow::Ref { idx })
            .collect()
    };

    push_section(&mut rows, Section::Branches, refs_of(RefKind::LocalBranch));
    push_section(
        &mut rows,
        Section::PullRequests,
        (0..pr_count).map(|idx| TreeRow::Pr { idx }).collect(),
    );
    push_section(&mut rows, Section::Remotes, refs_of(RefKind::RemoteBranch));
    push_section(&mut rows, Section::Tags, refs_of(RefKind::Tag));
    push_section(
        &mut rows,
        Section::Commits,
        (0..commit_count).map(|idx| TreeRow::Commit { idx }).collect(),
    );

    rows
}

impl Default for GitTreeView {
    fn default() -> Self {
        Self::new()
    }
}

// ── Entity / View / TypedActionView ──────────────────────────────────────────

impl Entity for GitTreeView {
    type Event = GitTreeEvent;
}

impl View for GitTreeView {
    fn ui_name() -> &'static str {
        "GitTreeView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let style = ViewStyle::from_appearance(app);
        let font_family = self.font_family.or(style.font_family);

        let trigger = render_trigger(
            self.head_label.as_deref(),
            self.head_detached,
            self.is_open,
            font_family,
            &style,
        );

        let mut stack = Stack::new().with_child(trigger);

        if self.is_open {
            stack.add_positioned_overlay_child(
                self.render_panel(app, &style, font_family),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 2.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::BottomLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }

        stack.finish()
    }

    fn accessibility_contents(&self, _ctx: &AppContext) -> Option<AccessibilityContent> {
        None
    }
}

impl GitTreeView {
    /// The dropdown body: status banners plus the virtualised row list.
    fn render_panel(
        &self,
        app: &AppContext,
        style: &ViewStyle,
        font_family: Option<FamilyId>,
    ) -> Box<dyn Element> {
        let style = *style;
        let _ = app;

        if self.rows.is_empty() {
            return Container::new(render_empty(&style, font_family))
                .with_background(style.panel_bg)
                .with_border(Border::all(1.0).with_border_fill(style.panel_border))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.0)))
                .finish();
        }

        // Clone Arcs (cheap) for the UniformList closure, which must own its data.
        let commits = Arc::clone(&self.commits);
        let refs = Arc::clone(&self.refs);
        let prs = Arc::clone(&self.prs);
        let rows = Arc::clone(&self.rows);
        let commit_lanes = Arc::clone(&self.commit_lanes);
        let edges = Arc::clone(&self.edges);
        let selected_oid = self.selected_oid.clone();
        let collapsed = self.collapsed.clone();
        let max_lane = self.max_lane;
        let item_count = rows.len();

        let list = UniformList::new(
            self.list_state.clone(),
            item_count,
            move |range, app| {
                let style = ViewStyle::from_appearance(app);
                let range_end = range.end.min(rows.len());

                // Pre-build per-index edge info to avoid borrow-in-move conflicts.
                let edge_info: Vec<(Vec<Edge>, Vec<Edge>)> = (range.start..range_end)
                    .map(|i| match &rows[i] {
                        TreeRow::Commit { idx } => {
                            let oid = &commits[*idx].oid;
                            let out =
                                edges.iter().filter(|e| &e.from_oid == oid).cloned().collect();
                            let inc =
                                edges.iter().filter(|e| &e.to_oid == oid).cloned().collect();
                            (out, inc)
                        }
                        _ => (Vec::new(), Vec::new()),
                    })
                    .collect();

                let commits = Arc::clone(&commits);
                let refs = Arc::clone(&refs);
                let prs = Arc::clone(&prs);
                let rows = Arc::clone(&rows);
                let commit_lanes = Arc::clone(&commit_lanes);
                let selected_oid = selected_oid.clone();
                let collapsed = collapsed.clone();
                let start = range.start;

                edge_info
                    .into_iter()
                    .enumerate()
                    .map(move |(rel_idx, (outgoing, incoming))| {
                        let row_idx = start + rel_idx;
                        match &rows[row_idx] {
                            TreeRow::Section { section, count } => render_section_header(
                                *section,
                                *count,
                                !collapsed.contains(section),
                                font_family,
                                &style,
                            ),
                            TreeRow::Ref { idx } => {
                                render_ref_row(&refs[*idx], font_family, &style)
                            }
                            TreeRow::Pr { idx } => {
                                render_pr_row(&prs[*idx], font_family, &style)
                            }
                            TreeRow::Commit { idx } => {
                                let idx = *idx;
                                let commit = &commits[idx];
                                let lane = *commit_lanes.get(&commit.oid).unwrap_or(&0);
                                let is_selected =
                                    selected_oid.as_deref() == Some(commit.oid.as_str());
                                let is_last = idx + 1 == commits.len();
                                let next_lane = if is_last {
                                    None
                                } else {
                                    Some(*commit_lanes.get(&commits[idx + 1].oid).unwrap_or(&0))
                                };

                                let outgoing_refs: Vec<&Edge> = outgoing.iter().collect();
                                let incoming_refs: Vec<&Edge> = incoming.iter().collect();

                                render_row(
                                    idx,
                                    commit,
                                    lane,
                                    max_lane,
                                    next_lane,
                                    &outgoing_refs,
                                    &incoming_refs,
                                    is_selected,
                                    font_family,
                                    &style,
                                )
                            }
                        }
                    })
            },
        );

        let scroll = Scrollable::vertical(
            self.scroll_state.clone(),
            list.finish_scrollable(),
            ScrollbarWidth::Auto,
            style.scrollbar_thumb,        // nonactive thumb
            style.scrollbar_thumb_active, // active thumb
            style.scrollbar_track,        // track background
        );

        let mut column = Flex::column().with_main_axis_size(MainAxisSize::Min);

        // No HEAD banner here — the trigger button already states where HEAD is,
        // and repeating it inside the dropdown just ate a row.
        if let Some(status) = self.status.as_deref() {
            column.add_child(render_status_banner(status, font_family, &style));
        }

        // UniformList panics with `unimplemented!()` if given unconstrained height,
        // and Scrollable debug-asserts that its child's width is finite. The
        // diff-view header is mounted inside a Flex::row whose max bounds are
        // INFINITY in both axes, so we must bound both ourselves.
        column.add_child(
            ConstrainedBox::new(
                Container::new(scroll.finish())
                    .with_vertical_padding(4.0)
                    .finish(),
            )
            .with_max_height(PANEL_MAX_HEIGHT)
            .with_max_width(PANEL_MAX_WIDTH)
            .finish(),
        );

        // Opaque background + border: this floats over the diff, so without its
        // own surface the commit text renders straight on top of the file list.
        ConstrainedBox::new(
            Container::new(column.finish())
                .with_background(style.panel_bg)
                .with_border(Border::all(1.0).with_border_fill(style.panel_border))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.0)))
                .finish(),
        )
        .with_max_width(PANEL_MAX_WIDTH)
        .finish()
    }
}

impl TypedActionView for GitTreeView {
    type Action = GitTreeAction;

    fn handle_action(&mut self, action: &GitTreeAction, ctx: &mut ViewContext<Self>) {
        match action {
            GitTreeAction::TogglePanel => {
                self.is_open = !self.is_open;
                self.status = None;
                if self.is_open {
                    ctx.emit(GitTreeEvent::PanelOpened);
                }
                ctx.notify();
            }
            GitTreeAction::SelectCommit(oid) => {
                self.selected_oid = Some(oid.clone());
                self.status = None;
                self.rebuild_rows();
                // Close on select: the point of picking a commit is to look at
                // its diff, which the panel would otherwise be covering.
                self.is_open = false;
                ctx.emit(GitTreeEvent::CommitSelected(oid.clone()));
                ctx.notify();
            }
            GitTreeAction::ToggleSection(section) => {
                if !self.collapsed.remove(section) {
                    self.collapsed.insert(*section);
                }
                self.rebuild_rows();
                ctx.notify();
            }
            GitTreeAction::CheckoutBranch(name) => {
                ctx.emit(GitTreeEvent::CheckoutBranch(name.clone()));
                ctx.notify();
            }
            GitTreeAction::CheckoutCommit(oid) => {
                ctx.emit(GitTreeEvent::CheckoutCommit(oid.clone()));
                ctx.notify();
            }
            GitTreeAction::CreateBranchAt(oid) => {
                ctx.emit(GitTreeEvent::CreateBranchAt(oid.clone()));
                ctx.notify();
            }
            GitTreeAction::CheckoutPullRequest(number) => {
                let Some(pr) = self.prs.iter().find(|p| p.number == *number) else {
                    return;
                };
                match pr_checkout_plan(pr, &self.refs) {
                    PrCheckout::Current { tip_oid } => {
                        // Same as clicking the current branch: show its tip.
                        self.handle_action(&GitTreeAction::SelectCommit(tip_oid), ctx);
                    }
                    PrCheckout::Branch(name) => {
                        ctx.emit(GitTreeEvent::CheckoutBranch(name));
                    }
                    PrCheckout::Fetch(branch) => {
                        self.status = Some(format!("fetching origin/{branch}..."));
                        ctx.emit(GitTreeEvent::FetchAndCheckoutBranch(branch));
                    }
                    PrCheckout::Fork => {
                        self.status = Some(format!(
                            "#{number} is from a fork, so it is not checked out here; use open"
                        ));
                    }
                    PrCheckout::InvalidBranch => {
                        self.status = Some(format!(
                            "#{number}: cannot check out branch {:?}",
                            pr.head_ref
                        ));
                    }
                }
                ctx.notify();
            }
            GitTreeAction::OpenPullRequest(url) => {
                ctx.emit(GitTreeEvent::OpenPullRequest(url.clone()));
            }
        }
    }
}

// ── row rendering (free functions, callable from UniformList closure) ─────────

fn render_empty(style: &ViewStyle, font_family: Option<FamilyId>) -> Box<dyn Element> {
    let label: Box<dyn Element> = match font_family {
        Some(family) => Text::new_inline("No commits found.".to_string(), family, style.font_size)
            .with_color(style.fg_60)
            .finish(),
        None => Stack::new().finish(),
    };
    Container::new(label)
        .with_horizontal_padding(16.0)
        .with_vertical_padding(16.0)
        .finish()
}

/// The compact button the panel hangs off. Shows where HEAD is and nothing
/// else — this sits in the diff header, which has very little room.
///
/// Detached HEAD gets the warning colour: it is a state people forget they are
/// in, and from here it is one click away.
fn render_trigger(
    head_label: Option<&str>,
    detached: bool,
    is_open: bool,
    font_family: Option<FamilyId>,
    style: &ViewStyle,
) -> Box<dyn Element> {
    let mut row = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(4.0);

    if let Some(family) = font_family {
        let label = head_label.unwrap_or("no repo");
        let text = if detached {
            format!("detached {label}")
        } else {
            label.to_string()
        };
        let color = if detached { style.warn_fg } else { style.fg_90 };

        row.add_child(
            Text::new_inline(truncate(&text, 28), family, style.font_size - 1.0)
                .with_color(color)
                .finish(),
        );
        row.add_child(
            Text::new_inline(
                if is_open { CHEVRON_OPEN } else { CHEVRON_CLOSED }.to_string(),
                family,
                style.font_size - 3.0,
            )
            .with_color(style.fg_60)
            .finish(),
        );
    }

    let body = Container::new(row.finish())
        .with_horizontal_padding(6.0)
        .with_vertical_padding(2.0)
        .with_background(if is_open {
            style.trigger_bg_active
        } else {
            style.trigger_bg
        })
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.0)))
        .finish();

    Hoverable::new(MouseStateHandle::default(), move |_hovered| body)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GitTreeAction::TogglePanel);
        })
        .finish()
}

fn render_status_banner(
    status: &str,
    font_family: Option<FamilyId>,
    style: &ViewStyle,
) -> Box<dyn Element> {
    let Some(family) = font_family else {
        return Stack::new().finish();
    };
    Container::new(
        Text::new_inline(status.to_string(), family, style.font_size - 1.0)
            .with_color(style.warn_fg)
            .finish(),
    )
    .with_horizontal_padding(ROW_H_PADDING)
    .with_vertical_padding(4.0)
    .with_background(style.warn_bg)
    .finish()
}

fn render_section_header(
    section: Section,
    count: usize,
    expanded: bool,
    font_family: Option<FamilyId>,
    style: &ViewStyle,
) -> Box<dyn Element> {
    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(4.0);

    if let Some(family) = font_family {
        let small = (style.font_size - 2.0).max(10.0);
        // Text chevrons rather than icons: this crate has no icon dependency and
        // the glyphs are present in every UI font we ship.
        row.add_child(
            Text::new_inline(
                if expanded { CHEVRON_OPEN } else { CHEVRON_CLOSED }.to_string(),
                family,
                small,
            )
            .with_color(style.fg_60)
            .finish(),
        );
        row.add_child(
            Text::new_inline(section.label().to_string(), family, small)
                .with_color(style.fg_60)
                .finish(),
        );
        row.add_child(
            Text::new_inline(count.to_string(), family, small)
                .with_color(style.fg_40)
                .finish(),
        );
    }

    let body = ConstrainedBox::new(
        Container::new(row.finish())
            .with_horizontal_padding(ROW_H_PADDING)
            .with_vertical_padding(ROW_V_PADDING)
            .with_background(style.header_bg)
            .finish(),
    )
    .with_min_height(ROW_HEIGHT)
    .finish();

    Hoverable::new(MouseStateHandle::default(), move |_hovered| body)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GitTreeAction::ToggleSection(section));
        })
        .finish()
}

/// A branch or tag. Clicking checks it out.
fn render_ref_row(
    node: &RefNode,
    font_family: Option<FamilyId>,
    style: &ViewStyle,
) -> Box<dyn Element> {
    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.0);

    if let Some(family) = font_family {
        let small = (style.font_size - 2.0).max(10.0);

        // Current-branch marker keeps its slot when absent so names stay aligned.
        // No per-kind glyph: the section header already says BRANCHES / TAGS, and
        // every symbol tried for it was missing from the UI font.
        row.add_child(
            Text::new_inline(
                if node.is_head { CURRENT_MARKER } else { " " }.to_string(),
                family,
                small,
            )
            .with_color(style.head_fg)
            .finish(),
        );

        let name_color = if node.is_head { style.head_fg } else { style.fg_full };
        row.add_child(
            Text::new_inline(node.name.clone(), family, style.font_size)
                .with_color(name_color)
                .finish(),
        );

        // Ahead/behind vs upstream: the number that tells you whether to push.
        if node.ahead > 0 {
            row.add_child(
                Text::new_inline(ahead_label(node.ahead), family, small)
                    .with_color(style.ahead_fg)
                    .finish(),
            );
        }
        if node.behind > 0 {
            row.add_child(
                Text::new_inline(behind_label(node.behind), family, small)
                    .with_color(style.behind_fg)
                    .finish(),
            );
        }
    }

    let body = ConstrainedBox::new(
        Container::new(row.finish())
            .with_horizontal_padding(ROW_H_PADDING + REF_INDENT)
            .with_vertical_padding(ROW_V_PADDING)
            .finish(),
    )
    .with_min_height(ROW_HEIGHT)
    .finish();

    let name = node.name.clone();
    let tip = node.tip_oid.clone();
    let is_head = node.is_head;
    let is_tag = node.kind == RefKind::Tag;

    Hoverable::new(MouseStateHandle::default(), move |_hovered| body)
        .on_click(move |ctx, _, _| {
            if is_head {
                // Already here — show its tip instead of a pointless checkout.
                ctx.dispatch_typed_action(GitTreeAction::SelectCommit(tip.clone()));
            } else if is_tag {
                // Checking out a tag detaches HEAD; route it through the commit
                // path so the app reports it as such.
                ctx.dispatch_typed_action(GitTreeAction::CheckoutCommit(tip.clone()));
            } else {
                ctx.dispatch_typed_action(GitTreeAction::CheckoutBranch(name.clone()));
            }
        })
        .finish()
}

/// An open pull request: `#123  title  (head)` plus state chips and an `open`
/// chip. Clicking the row checks out the head branch; the `open` chip opens
/// the PR on GitHub and, because the row defers to its children, does not
/// also check out.
fn render_pr_row(
    pr: &PrSummary,
    font_family: Option<FamilyId>,
    style: &ViewStyle,
) -> Box<dyn Element> {
    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.0);

    if let Some(family) = font_family {
        let small = (style.font_size - 2.0).max(10.0);

        row.add_child(
            Text::new_inline(format!("#{}", pr.number), family, small)
                .with_color(style.fg_60)
                .finish(),
        );
        row.add_child(
            Text::new_inline(truncate(&pr.title, PR_TITLE_MAX_CHARS), family, style.font_size)
                .with_color(if pr.is_draft { style.fg_60 } else { style.fg_full })
                .finish(),
        );
        row.add_child(
            Text::new_inline(
                format!("({})", truncate(&pr.head_ref, PR_HEAD_MAX_CHARS)),
                family,
                small,
            )
            .with_color(style.fg_40)
            .finish(),
        );

        if pr.is_draft {
            row.add_child(render_state_chip(DRAFT_LABEL, style.fg_60, family, style));
        }
        if pr.is_cross_repository {
            row.add_child(render_state_chip(FORK_LABEL, style.fg_60, family, style));
        }
        if let Some(label) = pr.review.label() {
            let color = match pr.review {
                ReviewState::Approved => style.ok_fg,
                _ => style.warn_fg,
            };
            row.add_child(render_state_chip(label, color, family, style));
        }
        if let Some(label) = pr.checks.label() {
            let color = match pr.checks {
                ChecksState::Pass => style.ok_fg,
                ChecksState::Fail => style.fail_fg,
                _ => style.warn_fg,
            };
            row.add_child(render_state_chip(label, color, family, style));
        }

        row.add_child(render_action_chip(
            OPEN_PR_LABEL,
            style.action_chip_bg,
            family,
            style,
            GitTreeAction::OpenPullRequest(pr.url.clone()),
        ));
    }

    let body = ConstrainedBox::new(
        Container::new(row.finish())
            .with_horizontal_padding(ROW_H_PADDING + REF_INDENT)
            .with_vertical_padding(ROW_V_PADDING)
            .finish(),
    )
    .with_min_height(ROW_HEIGHT)
    .finish();

    let number = pr.number;
    Hoverable::new(MouseStateHandle::default(), move |_hovered| body)
        .with_defer_events_to_children()
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GitTreeAction::CheckoutPullRequest(number));
        })
        .finish()
}

/// A non-interactive pill whose text colour carries the state.
fn render_state_chip(
    text: &str,
    color: ColorU,
    family: FamilyId,
    style: &ViewStyle,
) -> Box<dyn Element> {
    Container::new(
        Text::new_inline(text.to_string(), family, style.font_size - 3.0)
            .with_color(color)
            .finish(),
    )
    .with_horizontal_padding(PILL_H_PAD)
    .with_vertical_padding(PILL_V_PAD)
    .with_background(style.ref_pill_bg)
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PILL_RADIUS)))
    .finish()
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    _idx: usize,
    commit: &CommitNode,
    lane: u32,
    max_lane: u32,
    next_lane: Option<u32>,
    outgoing: &[&Edge],
    _incoming: &[&Edge],
    is_selected: bool,
    font_family: Option<FamilyId>,
    style: &ViewStyle,
) -> Box<dyn Element> {
    // ── Lane gutter ──────────────────────────────────────────────────────────
    let gutter_lanes = (max_lane + 1).max(lane + 1);
    let gutter = render_gutter(commit, lane, gutter_lanes, outgoing, next_lane, style);

    // One line: ref pills, summary, then muted metadata. Two lines would double
    // ROW_HEIGHT for every row in the list, section headers included.
    let mut content = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.0);

    let small_size = (style.font_size - 2.0).max(10.0);

    if let Some(family) = font_family {
        // Pills lead the line so the eye finds branch tips while scanning.
        for ref_name in commit.branch_refs.iter().take(MAX_BRANCH_PILLS) {
            let bg = ref_pill_bg(ref_name, commit.is_head, style);
            content.add_child(render_pill(ref_name, bg, family, style));
        }

        content.add_child(
            Text::new_inline(
                truncate(&commit.summary, SUMMARY_MAX_CHARS),
                family,
                style.font_size,
            )
            .with_color(style.fg_full)
            .finish(),
        );

        content.add_child(
            Text::new_inline(
                format!(
                    "{} · {}",
                    commit.short_oid,
                    format_relative_time(commit.committer_time)
                ),
                family,
                small_size,
            )
            .with_color(style.fg_60)
            .finish(),
        );

        // Action chips appear only once the row is selected, so the first click
        // can never check anything out by accident.
        if is_selected {
            content.add_child(render_action_chip(
                "checkout",
                style.action_chip_bg,
                family,
                style,
                GitTreeAction::CheckoutCommit(commit.oid.clone()),
            ));
            content.add_child(render_action_chip(
                "branch",
                style.action_chip_bg,
                family,
                style,
                GitTreeAction::CreateBranchAt(commit.oid.clone()),
            ));
        }
    }

    let row_inner = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.0)
        .with_child(gutter)
        .with_child(content.finish())
        .finish();

    // Selection state: subtle bg + left-edge accent strip. Unselected rows
    // have no card background — keeps the gutter lane line visually continuous
    // and lets the eye treat the column as one log, not a stack of cards.
    let body = if is_selected {
        Container::new(row_inner)
            .with_vertical_padding(ROW_V_PADDING)
            .with_horizontal_padding(ROW_H_PADDING)
            .with_background(style.row_bg_selected)
            .with_border(
                Border::left(SELECTION_ACCENT_W).with_border_fill(style.row_border_selected),
            )
            .finish()
    } else {
        Container::new(row_inner)
            .with_vertical_padding(ROW_V_PADDING)
            .with_horizontal_padding(ROW_H_PADDING)
            .finish()
    };

    let row_body = ConstrainedBox::new(body).with_min_height(ROW_HEIGHT).finish();

    let oid = commit.oid.clone();
    let oid_for_right = commit.oid.clone();
    // Row handles are synthesised fresh each render — Hoverable is stateless
    // (DA-flagged; hover state would not persist across frames anyway).
    // Deferring lets the `checkout` / `branch` chips take their own click
    // instead of also re-selecting the row.
    Hoverable::new(MouseStateHandle::default(), move |_hovered| row_body)
        .with_defer_events_to_children()
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GitTreeAction::SelectCommit(oid.clone()));
        })
        .on_right_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GitTreeAction::CheckoutCommit(oid_for_right.clone()));
        })
        .finish()
}

/// A small clickable chip that dispatches `action` without selecting the row.
fn render_action_chip(
    text: &str,
    background: Fill,
    family: FamilyId,
    style: &ViewStyle,
    action: GitTreeAction,
) -> Box<dyn Element> {
    let label = Text::new_inline(text.to_string(), family, style.font_size - 3.0)
        .with_color(style.fg_90)
        .finish();
    let chip = Container::new(label)
        .with_horizontal_padding(PILL_H_PAD)
        .with_vertical_padding(PILL_V_PAD)
        .with_background(background)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PILL_RADIUS)))
        .finish();

    Hoverable::new(MouseStateHandle::default(), move |_hovered| chip)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .finish()
}

/// Render the lane-graph gutter for one row.
///
/// Produces a fixed-width `Flex::row` strip with one `LANE_WIDTH`-wide cell
/// per lane. The commit's lane shows a coloured dot with a continuous vertical
/// line behind it (top half + dot + bottom half), so adjacent rows visually
/// connect into a single tree column. Pass-through lanes draw a full-height
/// thin vertical bar at the lane colour; empty lanes are transparent spacers.
fn render_gutter(
    _commit: &CommitNode,
    commit_lane: u32,
    total_lanes: u32,
    outgoing: &[&Edge],
    next_lane: Option<u32>,
    style: &ViewStyle,
) -> Box<dyn Element> {
    let dot_color = lane_color(commit_lane, style);

    // Lanes carrying non-straight (merge-parent) outgoing edges — these stay
    // active through the row even though the commit isn't on them.
    let merge_parent_lanes: Vec<u32> = outgoing
        .iter()
        .filter(|e| e.kind != EdgeKind::Straight)
        .map(|e| e.to_lane)
        .collect();

    let mut row = Flex::row().with_main_axis_size(MainAxisSize::Min);

    for l in 0..total_lanes {
        let color = lane_color(l, style);
        let cell: Box<dyn Element> = if l == commit_lane {
            // Continuous line through the dot lane: top half (faded if no
            // commit above), full dot, bottom half (faded if no commit below).
            // Using a Flex::column rather than Stack overlay keeps math simple
            // and lets adjacent rows' line halves meet exactly.
            let half_height = (ROW_HEIGHT - DOT_SIZE) / 2.0;

            let line_above = ConstrainedBox::new(
                Container::new(Stack::new().finish())
                    .with_background(Fill::Solid(dot_color))
                    .finish(),
            )
            .with_width(CONNECTOR_W)
            .with_height(half_height)
            .finish();

            let dot = ConstrainedBox::new(
                Container::new(Stack::new().finish())
                    .with_background(Fill::Solid(dot_color))
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(DOT_SIZE / 2.0)))
                    .finish(),
            )
            .with_width(DOT_SIZE)
            .with_height(DOT_SIZE)
            .finish();

            let line_below = ConstrainedBox::new(
                Container::new(Stack::new().finish())
                    .with_background(Fill::Solid(dot_color))
                    .finish(),
            )
            .with_width(CONNECTOR_W)
            .with_height(half_height)
            .finish();

            let column = Flex::column()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(line_above)
                .with_child(dot)
                .with_child(line_below)
                .finish();

            ConstrainedBox::new(Container::new(column).finish())
                .with_width(LANE_WIDTH)
                .with_height(ROW_HEIGHT)
                .finish()
        } else {
            // Pass-through: full-height thin line iff lane is active in this row.
            let is_active = merge_parent_lanes.contains(&l)
                || next_lane.map(|nl| nl == l).unwrap_or(false);

            if is_active {
                let line = ConstrainedBox::new(
                    Container::new(Stack::new().finish())
                        .with_background(Fill::Solid(color))
                        .finish(),
                )
                .with_width(CONNECTOR_W)
                .with_height(ROW_HEIGHT)
                .finish();
                ConstrainedBox::new(
                    Container::new(line)
                        .with_horizontal_padding((LANE_WIDTH - CONNECTOR_W) / 2.0)
                        .finish(),
                )
                .with_width(LANE_WIDTH)
                .with_height(ROW_HEIGHT)
                .finish()
            } else {
                ConstrainedBox::new(Container::new(Stack::new().finish()).finish())
                    .with_width(LANE_WIDTH)
                    .with_height(ROW_HEIGHT)
                    .finish()
            }
        };
        row.add_child(cell);
    }

    row.finish()
}

/// Pick the background colour for a branch-ref pill. HEAD branches get the
/// accent colour; remote refs (any prefix-`origin/` form) get a muted variant;
/// everything else gets the plain ref-pill colour.
fn ref_pill_bg(ref_name: &str, is_head: bool, style: &ViewStyle) -> Fill {
    if is_head {
        style.head_pill_bg
    } else if ref_name.starts_with("origin/")
        || ref_name.starts_with("upstream/")
        || ref_name.contains('/')
    {
        style.remote_pill_bg
    } else {
        style.ref_pill_bg
    }
}

fn render_pill(text: &str, background: Fill, family: FamilyId, style: &ViewStyle) -> Box<dyn Element> {
    let label = Text::new_inline(text.to_string(), family, style.font_size - 2.0)
        .with_color(style.fg_90)
        .finish();
    Container::new(label)
        .with_horizontal_padding(PILL_H_PAD)
        .with_vertical_padding(PILL_V_PAD)
        .with_background(background)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PILL_RADIUS)))
        .finish()
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Format a unix timestamp as a short relative-time string ("2h", "3d", "5w").
/// Compact units mirror typical git-log UI conventions (no "ago" suffix —
/// the meta line is dense enough already).
fn format_relative_time(committer_time: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(committer_time);
    let diff = now.saturating_sub(committer_time);
    if diff < 0 {
        // Clock skew or future commit — just show the raw delta as seconds.
        return "now".to_string();
    }
    if diff < 60 {
        "now".to_string()
    } else if diff < 3_600 {
        format!("{}m", diff / 60)
    } else if diff < 86_400 {
        format!("{}h", diff / 3_600)
    } else if diff < 604_800 {
        format!("{}d", diff / 86_400)
    } else if diff < 2_592_000 {
        format!("{}w", diff / 604_800)
    } else if diff < 31_536_000 {
        format!("{}mo", diff / 2_592_000)
    } else {
        format!("{}y", diff / 31_536_000)
    }
}

/// 5-colour palette cycling by lane index.
fn lane_color(lane: u32, style: &ViewStyle) -> ColorU {
    match lane % 5 {
        0 => style.lane_0,
        1 => style.lane_1,
        2 => style.lane_2,
        3 => style.lane_3,
        _ => style.lane_4,
    }
}

// ── theme helper ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct ViewStyle {
    fg_full: ColorU,
    fg_90: ColorU,
    fg_60: ColorU,
    fg_40: ColorU,
    row_bg_selected: Fill,
    row_border_selected: Fill,
    header_bg: Fill,
    panel_bg: Fill,
    panel_border: Fill,
    trigger_bg: Fill,
    trigger_bg_active: Fill,
    ref_pill_bg: Fill,
    head_pill_bg: Fill,
    remote_pill_bg: Fill,
    action_chip_bg: Fill,
    warn_bg: Fill,
    warn_fg: ColorU,
    head_fg: ColorU,
    ok_fg: ColorU,
    fail_fg: ColorU,
    ahead_fg: ColorU,
    behind_fg: ColorU,
    scrollbar_track: Fill,
    scrollbar_thumb: Fill,
    scrollbar_thumb_active: Fill,
    lane_0: ColorU,
    lane_1: ColorU,
    lane_2: ColorU,
    lane_3: ColorU,
    lane_4: ColorU,
    font_size: f32,
    font_family: Option<FamilyId>,
}

impl ViewStyle {
    fn from_appearance(app: &AppContext) -> Self {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font_size = appearance.ui_font_size();
        Self {
            fg_full: theme.foreground().into_solid(),
            fg_90: theme.foreground().with_opacity(90).into_solid(),
            fg_60: theme.foreground().with_opacity(60).into_solid(),
            fg_40: theme.foreground().with_opacity(40).into_solid(),
            row_bg_selected: internal_colors::fg_overlay_2(theme).into(),
            row_border_selected: internal_colors::fg_overlay_3(theme).into(),
            header_bg: internal_colors::fg_overlay_1(theme).into(),
            // The panel floats over the diff, so it needs an opaque surface of
            // its own — an overlay tinted with fg_overlay_* would let the file
            // list show through the commit text.
            panel_bg: theme.background().into(),
            panel_border: internal_colors::fg_overlay_3(theme).into(),
            trigger_bg: internal_colors::fg_overlay_1(theme).into(),
            trigger_bg_active: internal_colors::fg_overlay_3(theme).into(),
            ref_pill_bg: internal_colors::fg_overlay_2(theme).into(),
            head_pill_bg: internal_colors::fg_overlay_4(theme).into(),
            remote_pill_bg: internal_colors::fg_overlay_1(theme).into(),
            action_chip_bg: internal_colors::fg_overlay_3(theme).into(),
            warn_bg: internal_colors::fg_overlay_2(theme).into(),
            // Fixed hues rather than theme colours: these carry meaning
            // (current / ahead / behind / warning) and must stay legible on
            // whichever theme is loaded.
            warn_fg: ColorU::from_u32(0xF5A623FF),
            head_fg: ColorU::from_u32(0x7ED321FF),
            ok_fg: ColorU::from_u32(0x7ED321FF),
            fail_fg: ColorU::from_u32(0xE5534BFF),
            ahead_fg: ColorU::from_u32(0x7ED321FF),
            behind_fg: ColorU::from_u32(0xF5A623FF),
            scrollbar_track: internal_colors::fg_overlay_1(theme).into(),
            scrollbar_thumb: internal_colors::fg_overlay_2(theme).into(),
            scrollbar_thumb_active: internal_colors::fg_overlay_4(theme).into(),
            // Five perceptually distinct hues, semi-transparent so they work
            // on both dark and light themes.
            lane_0: ColorU::from_u32(0x4A90E2B0), // blue
            lane_1: ColorU::from_u32(0x7ED321B0), // green
            lane_2: ColorU::from_u32(0xF5A623B0), // amber
            lane_3: ColorU::from_u32(0xBD10E0B0), // purple
            lane_4: ColorU::from_u32(0xD0021BB0), // red
            font_size,
            font_family: Some(appearance.ui_font_family()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ahead_behind_labels_are_ascii() {
        for n in [1usize, 7, 42, 1_000, usize::MAX] {
            assert!(ahead_label(n).is_ascii(), "ahead label for {n}");
            assert!(behind_label(n).is_ascii(), "behind label for {n}");
        }
        assert_eq!(ahead_label(3), "+3");
        assert_eq!(behind_label(12), "-12");
    }

    #[test]
    fn tree_glyphs_are_ascii() {
        for glyph in [CHEVRON_OPEN, CHEVRON_CLOSED, CURRENT_MARKER, OPEN_PR_LABEL] {
            assert!(glyph.is_ascii(), "{glyph:?} is not ASCII");
        }
        assert!(Section::PullRequests.label().is_ascii());
    }

    fn rnode(name: &str, kind: RefKind) -> RefNode {
        RefNode {
            name: name.into(),
            kind,
            tip_oid: String::new(),
            is_head: false,
            ahead: 0,
            behind: 0,
        }
    }

    /// Compact row signature: `S:<label>:<count>`, `R<idx>`, `P<idx>`, `C<idx>`.
    fn sig(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeRow::Section { section, count } => format!("S:{}:{count}", section.label()),
                TreeRow::Ref { idx } => format!("R{idx}"),
                TreeRow::Pr { idx } => format!("P{idx}"),
                TreeRow::Commit { idx } => format!("C{idx}"),
            })
            .collect()
    }

    #[test]
    fn pr_section_sits_after_branches() {
        let refs = vec![
            rnode("main", RefKind::LocalBranch),
            rnode("origin/main", RefKind::RemoteBranch),
            rnode("v1", RefKind::Tag),
        ];
        let rows = build_rows(&refs, 2, 1, &HashSet::new());
        assert_eq!(
            sig(&rows),
            vec![
                "S:BRANCHES:1",
                "R0",
                "S:PULL REQUESTS:2",
                "P0",
                "P1",
                "S:REMOTES:1",
                "R1",
                "S:TAGS:1",
                "R2",
                "S:COMMITS:1",
                "C0",
            ]
        );
    }

    #[test]
    fn pr_section_omitted_when_empty_and_collapsible() {
        let refs = vec![rnode("main", RefKind::LocalBranch)];
        let rows = build_rows(&refs, 0, 1, &HashSet::new());
        assert_eq!(sig(&rows), vec!["S:BRANCHES:1", "R0", "S:COMMITS:1", "C0"]);

        let collapsed = HashSet::from([Section::PullRequests]);
        let rows = build_rows(&refs, 3, 0, &collapsed);
        assert_eq!(sig(&rows), vec!["S:BRANCHES:1", "R0", "S:PULL REQUESTS:3"]);
    }
}
