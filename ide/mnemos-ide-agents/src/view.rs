// SPDX-License-Identifier: AGPL-3.0-only
//! `AgentsPanel`: the Tools-panel list of running CLI agent sessions. Clicking
//! a row switches to the tab running that agent.
//!
//! Agents are grouped by what they need from the user: NEEDS YOU (blocked on a
//! permission), WORKING, and DONE (finished a turn, or restored and waiting).
//! Within a group, newest first. Line one: a status dot, the project, what the
//! agent is doing and how long ago. Line two: for a working agent the prompt it
//! is working on, for a finished one what it answered, cleaned of harness
//! markup. An agent that finished since you last opened it is "new": green dot,
//! full-strength text. Clicking it (or the agent starting again) clears that;
//! older finished agents recede. Cancel shows only on hover, at the far right, next to
//! "suggest" on an idle Claude agent, which asks the user's own `claude` CLI
//! (headless, on this machine) for the likely next prompt and types it into
//! the agent's input without sending it.
//!
//! Agents on the user's other machines (published through admin sync) follow
//! the local ones, under each machine's name, read-only except for "open",
//! which opens a Claude session's claude.ai Remote Control page in the
//! system browser.
//!
//! SESSIONS comes last, after every live agent: the past Claude Code
//! sessions of every project with a checkout here, the active project first, then by the newest session. A
//! session that ran on the other machine is listed as soon as transcript sync
//! has pulled it in. One line per session (`age * title [branch] note`), at
//! most five per project until "more", forty in all. `*` marks each project's
//! newest session. Sessions are grouped by project root and include ones
//! started in its subfolders, so `*` is not necessarily what
//! `claude --continue` (which goes by the exact folder) would pick.
//!
//! | Session | Click | Right-click |
//! | --- | --- | --- |
//! | idle | resume **in place** | resume a **fork** (new session id) |
//! | running here | switch to its terminal | switch to its terminal |
//! | running on another machine | fork from what has synced; `open` pill when it has a Remote Control URL | same |
//! | pulled from another machine minutes ago, not known to run anywhere ("from Windows") | fork | fork |
//! | folder missing here | nothing | nothing |
//!
//! Resuming in place is the click default because that is what the owner asked
//! for ("do the resume there"). A session another machine is running is never
//! resumed in place, so either click forks it instead; so is one whose newest
//! turns arrived from another machine within the last few minutes
//! (`SessionState::PulledRecently`), since that machine's `claude` may still
//! run without showing in its live list. Either way the
//! session opens as a terminal tab inside its project's own tab (the app
//! finds that tab in any window), never as a new project tab. A second click
//! within a few seconds of a launch is ignored, and the app refuses another
//! launch while the terminal it started still runs `claude --resume`, so a
//! double click starts one `claude`, not two.
//!
//! The app owns the subscription to `CLIAgentSessionsModel` and pushes new
//! row lists in via `set_rows`; past sessions arrive through
//! `set_session_groups`. The whole panel scrolls.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use instant::Instant;

use mnemos_core::ui::appearance::Appearance;
use mnemos_core::ui::theme::color::internal_colors;
use mnemos_ui_core::accessibility::AccessibilityContent;
use mnemos_ui_core::color::ColorU;
use mnemos_ui_core::elements::{
    Border, Clipped, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, Expanded, Fill, Flex, Hoverable, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, ParentElement, Radius, ScrollbarWidth, Shrinkable, Stack, Text,
};
use mnemos_ui_core::fonts::{FamilyId, Properties, Weight};
use mnemos_ui_core::r#async::Timer;
use mnemos_ui_core::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext,
};

use crate::types::{
    is_remote_control_url, AgentKind, AgentRow, AgentStatus, PluginHint, PluginHintSeverity,
    RemoteMachine, SessionGroup, SessionRow, SessionState,
};

const ROW_H_PAD: f32 = 10.0;
const ROW_V_PAD: f32 = 8.0;
const ROW_GAP: f32 = 4.0;
const ROW_RADIUS: f32 = 6.0;
/// How long a failed suggestion's reason stays on the row.
const SUGGEST_ERROR_SHOWN: Duration = Duration::from_secs(8);
const DOT_SIZE: f32 = 8.0;
const BLOCKED_BAR_WIDTH: f32 = 3.0;
/// How often ages ("3m") are re-rendered when nothing else changes.
const AGE_TICK: Duration = Duration::from_secs(30);
/// Line-two preview length; the row clips anyway, this bounds the work.
const PREVIEW_CHARS: usize = 140;

/// Chrome glyphs. ASCII only: the UI font has no coverage for dots and arrows
/// on Windows (the git tree learned that the hard way). Status dots are drawn
/// as shapes, not characters.
const CANCEL_GLYPH: &str = "x";
const SUGGEST_LABEL: &str = "suggest";
const SUGGESTING_LABEL: &str = "thinking...";
const OPEN_LABEL: &str = "open";
const ELLIPSIS: &str = "...";
const SESSIONS_TITLE: &str = "SESSIONS";
const REFRESH_LABEL: &str = "refresh";
const MORE_LABEL: &str = "more";
const LESS_LABEL: &str = "less";
const CHEVRON_OPEN: &str = "v";
const CHEVRON_CLOSED: &str = ">";
/// Marks each project's newest session.
const LATEST_MARKER: &str = "*";

/// Sessions shown per project until its "more" is clicked.
const SESSIONS_PER_PROJECT: usize = 5;
/// Session rows drawn in all, whatever is expanded: a plain column re-renders
/// every row every frame.
const MAX_SESSION_ROWS: usize = 40;
const SESSION_ROW_HEIGHT: f32 = 24.0;
/// Fixed width for the age column so titles line up.
const SESSION_AGE_WIDTH: f32 = 30.0;
const SESSION_TITLE_CHARS: usize = 64;
/// A session that was just launched ignores further clicks this long, so a
/// double click starts one `claude`, not two. After it, the app still
/// refuses a second launch while the terminal it started runs (or is still
/// starting) `claude --resume`, however slow the first hook is.
const RELAUNCH_GUARD: Duration = Duration::from_secs(4);

/// Which session the cancel `x` should act on: a CLI agent, identified by
/// its terminal view id.
#[derive(Clone, Debug)]
pub enum CancelTarget {
    /// Ctrl-C against a PTY-backed CLI agent.
    CliAgent { terminal_view_id_bits: u64 },
}

#[derive(Clone, Debug)]
pub enum AgentsPanelEvent {
    /// User clicked `x` on a row. The app sends a Ctrl-C to the agent's PTY.
    CancelRequested { target: CancelTarget },
    /// User clicked the row (anywhere outside the `x`). The app should
    /// switch to the workspace tab hosting that terminal.
    ActivateRequested { terminal_view_id_bits: u64 },
    /// User clicked "suggest" on an idle Claude row. The app reads the
    /// session's last exchange, asks the local `claude` CLI for the next prompt,
    /// types it into the agent's input, and reports back with [`AgentsPanel::finish_suggest`].
    SuggestRequested { terminal_view_id_bits: u64 },
    /// User clicked "open" on another machine's Claude row. `url` has passed
    /// [`crate::is_remote_control_url`]; the app opens it in the system
    /// browser.
    OpenRemoteRequested { url: String },
    /// Resume a past Claude Code session in its project's tab. `fork` asks
    /// for `--fork-session` (a copy with a new id); `false` continues the
    /// original. The panel has already refused what the session's state
    /// does not allow (see the module docs); the app validates the id again.
    ResumeSessionRequested {
        session_id: String,
        /// Where the session ran, on this machine.
        cwd: PathBuf,
        /// The project checkout the session was listed under.
        project_root: PathBuf,
        project_id: Option<String>,
        fork: bool,
    },
    /// The SESSIONS "refresh" chip: re-read the transcripts.
    RefreshSessionsRequested,
}

#[derive(Clone, Debug)]
pub enum AgentsPanelAction {
    Cancel { target: CancelTarget },
    Activate { terminal_view_id_bits: u64 },
    Suggest { terminal_view_id_bits: u64 },
    OpenRemote { url: String },
    ResumeSession { session_id: String, fork: bool },
    ToggleSessions,
    ToggleSessionsMore { root: PathBuf },
    RefreshSessions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SuggestState {
    Pending,
    Failed(String),
    /// Typed into the agent's input. Kept, so the pill is not offered again,
    /// until the row's group or `last_activity` moves on from these values.
    Suggested {
        group: Group,
        last_activity: Option<SystemTime>,
    },
}

pub struct AgentsPanel {
    rows: Vec<AgentRow>,
    /// Mouse state for the row body (click-to-activate) and its hover pills,
    /// by terminal view id. Kept across `set_rows` so hover and a click in
    /// progress survive agent updates; pruned when a row goes away. Local
    /// rows only: remote ids are list positions and may collide.
    row_handles: HashMap<u64, MouseStateHandle>,
    cancel_handles: HashMap<u64, MouseStateHandle>,
    suggest_handles: HashMap<u64, MouseStateHandle>,
    /// Suggestions in flight, just failed or just typed, by terminal view id.
    suggesting: HashMap<u64, SuggestState>,
    /// The user's other machines and their agents.
    remote: Vec<RemoteMachine>,
    /// Mouse state for the "open" pill on remote rows, by Remote Control
    /// URL (remote ids are list positions; the URL is unique per session).
    open_handles: HashMap<String, MouseStateHandle>,
    /// Each local row's last seen group, to notice a turn finishing.
    last_group: HashMap<u64, Group>,
    /// Finished since the user last opened them.
    unseen: HashSet<u64>,
    font_family: Option<FamilyId>,
    /// Kept for the setters the workspace calls; the compact list sorts by
    /// status and recency instead of by project.
    active_project: String,
    active_project_path: String,
    /// Optional banner shown above the rows when the Warp Claude Code plugin
    /// can't feed events (missing/outdated/needs-reload). `None` hides it.
    plugin_hint: Option<PluginHint>,
    /// An age re-render tick is scheduled.
    ticking: bool,
    /// Past Claude Code sessions, by project.
    sessions: Vec<SessionGroup>,
    sessions_collapsed: bool,
    /// Project roots whose "more" is open.
    sessions_expanded: HashSet<PathBuf>,
    sessions_header_handle: MouseStateHandle,
    sessions_refresh_handle: MouseStateHandle,
    /// By session id.
    session_row_handles: HashMap<String, MouseStateHandle>,
    /// By Remote Control URL.
    session_open_handles: HashMap<String, MouseStateHandle>,
    /// By project root.
    session_more_handles: HashMap<PathBuf, MouseStateHandle>,
    /// Sessions this panel asked to resume, and when (see [`RELAUNCH_GUARD`]).
    session_launched: HashMap<String, Instant>,
    scroll_state: ClippedScrollStateHandle,
}

impl AgentsPanel {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            row_handles: HashMap::new(),
            cancel_handles: HashMap::new(),
            suggest_handles: HashMap::new(),
            suggesting: HashMap::new(),
            remote: Vec::new(),
            open_handles: HashMap::new(),
            last_group: HashMap::new(),
            unseen: HashSet::new(),
            font_family: None,
            active_project: String::new(),
            active_project_path: String::new(),
            plugin_hint: None,
            ticking: false,
            sessions: Vec::new(),
            sessions_collapsed: false,
            sessions_expanded: HashSet::new(),
            sessions_header_handle: MouseStateHandle::default(),
            sessions_refresh_handle: MouseStateHandle::default(),
            session_row_handles: HashMap::new(),
            session_open_handles: HashMap::new(),
            session_more_handles: HashMap::new(),
            session_launched: HashMap::new(),
            scroll_state: ClippedScrollStateHandle::default(),
        }
    }

    /// Tell the panel which project label is currently active.
    pub fn set_active_project(&mut self, project: String, ctx: &mut ViewContext<Self>) {
        if self.active_project == project {
            return;
        }
        self.active_project = project;
        ctx.notify();
    }

    /// Tell the panel which filesystem path is the active project's root.
    pub fn set_active_project_path(&mut self, path: String, ctx: &mut ViewContext<Self>) {
        if self.active_project_path == path {
            return;
        }
        self.active_project_path = path;
        ctx.notify();
    }

    pub fn with_font(mut self, family: FamilyId) -> Self {
        self.font_family = Some(family);
        self
    }

    pub fn set_font(&mut self, family: FamilyId, ctx: &mut ViewContext<Self>) {
        self.font_family = Some(family);
        ctx.notify();
    }

    /// Replace the current row set. Mouse-state handles are kept for rows
    /// that are still there and dropped for the rest.
    pub fn set_rows(&mut self, rows: Vec<AgentRow>, ctx: &mut ViewContext<Self>) {
        let ids: HashSet<u64> = rows.iter().map(|r| r.terminal_view_id_bits).collect();
        for handles in [
            &mut self.row_handles,
            &mut self.cancel_handles,
            &mut self.suggest_handles,
        ] {
            handles.retain(|bits, _| ids.contains(bits));
            for bits in &ids {
                handles.entry(*bits).or_default();
            }
        }
        self.suggesting.retain(|bits, state| {
            let Some(row) = rows.iter().find(|r| r.terminal_view_id_bits == *bits) else {
                return false;
            };
            suggest_state_still_current(state, row)
        });
        let groups: HashMap<u64, Group> = rows
            .iter()
            .map(|r| (r.terminal_view_id_bits, group_of(r)))
            .collect();
        for (bits, group) in &groups {
            match (self.last_group.get(bits), group) {
                // A turn ended (or a permission prompt was answered and the
                // turn then ended) while the user looked elsewhere.
                (Some(Group::Working | Group::NeedsYou), Group::Done) => {
                    self.unseen.insert(*bits);
                }
                (_, Group::Working | Group::NeedsYou) => {
                    self.unseen.remove(bits);
                }
                _ => {}
            }
        }
        self.unseen.retain(|bits| groups.contains_key(bits));
        self.last_group = groups;
        self.rows = rows;
        if !self.ticking {
            self.ticking = true;
            self.schedule_age_tick(ctx);
        }
        ctx.notify();
    }

    /// Replace the other machines' agents (already filtered to fresh ones).
    pub fn set_remote_machines(&mut self, remote: Vec<RemoteMachine>, ctx: &mut ViewContext<Self>) {
        let urls: HashSet<&str> = remote
            .iter()
            .flat_map(|m| &m.rows)
            .filter_map(openable_url)
            .collect();
        self.open_handles
            .retain(|url, _| urls.contains(url.as_str()));
        for url in urls {
            self.open_handles.entry(url.to_string()).or_default();
        }
        self.remote = remote;
        ctx.notify();
    }

    /// Replace the past sessions. The app re-reads the transcripts and works
    /// out each one's state (running here, elsewhere, ...); the panel only
    /// draws and routes clicks. Mouse state is kept for sessions still there.
    pub fn set_session_groups(&mut self, groups: Vec<SessionGroup>, ctx: &mut ViewContext<Self>) {
        if self.sessions == groups {
            return;
        }
        let ids: HashSet<&str> = groups
            .iter()
            .flat_map(|g| &g.rows)
            .map(|r| r.id.as_str())
            .collect();
        self.session_row_handles
            .retain(|id, _| ids.contains(id.as_str()));
        for id in &ids {
            self.session_row_handles.entry(id.to_string()).or_default();
        }
        let urls: HashSet<&str> = groups
            .iter()
            .flat_map(|g| &g.rows)
            .filter_map(session_open_url)
            .collect();
        self.session_open_handles
            .retain(|url, _| urls.contains(url.as_str()));
        for url in urls {
            self.session_open_handles
                .entry(url.to_string())
                .or_default();
        }
        let roots: HashSet<&Path> = groups.iter().map(|g| g.root.as_path()).collect();
        self.session_more_handles
            .retain(|root, _| roots.contains(root.as_path()));
        self.sessions_expanded
            .retain(|root| roots.contains(root.as_path()));
        for root in roots {
            self.session_more_handles
                .entry(root.to_path_buf())
                .or_default();
        }
        self.session_launched
            .retain(|_, at| at.elapsed() < RELAUNCH_GUARD);
        self.sessions = groups;
        // No age tick of its own for sessions: the workspace's sessions tick
        // repaints them (`repaint_session_ages`), and only while this shows.
        ctx.notify();
    }

    /// Re-render so the sessions' ages move on. The workspace calls this on
    /// its sessions tick while the panel is on screen.
    pub fn repaint_session_ages(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.sessions.is_empty() {
            ctx.notify();
        }
    }

    /// A suggestion the app was asked for is done: typed into the agent's
    /// input (`Ok`), or not, with a short reason shown on the row for a while.
    pub fn finish_suggest(
        &mut self,
        terminal_view_id_bits: u64,
        result: Result<(), String>,
        ctx: &mut ViewContext<Self>,
    ) {
        match result {
            Ok(()) => {
                // The user has seen this one: a suggestion was typed for it.
                self.unseen.remove(&terminal_view_id_bits);
                match self
                    .rows
                    .iter()
                    .find(|r| r.terminal_view_id_bits == terminal_view_id_bits)
                {
                    Some(row) => {
                        self.suggesting.insert(
                            terminal_view_id_bits,
                            SuggestState::Suggested {
                                group: group_of(row),
                                last_activity: row.last_activity,
                            },
                        );
                    }
                    None => {
                        self.suggesting.remove(&terminal_view_id_bits);
                    }
                }
            }
            Err(reason) => {
                self.suggesting
                    .insert(terminal_view_id_bits, SuggestState::Failed(reason.clone()));
                let _ = ctx.spawn(
                    async {
                        Timer::after(SUGGEST_ERROR_SHOWN).await;
                    },
                    move |panel, _, ctx| {
                        if panel.suggesting.get(&terminal_view_id_bits)
                            == Some(&SuggestState::Failed(reason.clone()))
                        {
                            panel.suggesting.remove(&terminal_view_id_bits);
                            ctx.notify();
                        }
                    },
                );
            }
        }
        ctx.notify();
    }

    /// Set (or clear) the plugin-health banner shown above the rows.
    pub fn set_plugin_hint(&mut self, hint: Option<PluginHint>, ctx: &mut ViewContext<Self>) {
        if self.plugin_hint == hint {
            return;
        }
        self.plugin_hint = hint;
        ctx.notify();
    }

    /// Ages only change with time, so re-render on a slow timer while there
    /// are rows to age.
    fn schedule_age_tick(&mut self, ctx: &mut ViewContext<Self>) {
        let _ = ctx.spawn(
            async {
                Timer::after(AGE_TICK).await;
            },
            |panel, _, ctx| {
                if panel.rows.is_empty() && panel.remote.is_empty() {
                    panel.ticking = false;
                    return;
                }
                ctx.notify();
                panel.schedule_age_tick(ctx);
            },
        );
    }
}

impl Default for AgentsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Entity for AgentsPanel {
    type Event = AgentsPanelEvent;
}

impl View for AgentsPanel {
    fn ui_name() -> &'static str {
        "AgentsPanel"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let style = PanelStyle::from_appearance(app);

        let total = self.rows.len() + self.remote.iter().map(|m| m.rows.len()).sum::<usize>();
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(ROW_GAP)
            .with_child(self.render_header(total, &style));

        if let Some(banner) = self.render_plugin_hint(&style) {
            column.add_child(banner);
        }

        let now = SystemTime::now();
        if self.rows.is_empty() {
            column.add_child(self.render_note(
                "No agents running. Start claude, codex, gemini or opencode in a terminal.",
                &style,
            ));
        }
        let order = sorted_order(&self.rows);
        for group in [Group::NeedsYou, Group::Working, Group::Done] {
            let members: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&i| group_of(&self.rows[i]) == group)
                .collect();
            if members.is_empty() {
                continue;
            }
            column.add_child(self.render_section(group.title(), members.len(), &style));
            for idx in members {
                column.add_child(self.render_row(Some(idx), &self.rows[idx], now, &style));
            }
        }
        // Live agents stay together at the top (here, then the other
        // machines); past sessions come last.
        for machine in &self.remote {
            column.add_child(self.render_section(
                format!(
                    "ON {}  {}",
                    machine.platform_name().to_uppercase(),
                    machine.label
                ),
                machine.rows.len(),
                &style,
            ));
            if machine.rows.is_empty() {
                column.add_child(self.render_note("No agents running.", &style));
            }
            for idx in sorted_order(&machine.rows) {
                column.add_child(self.render_row(None, &machine.rows[idx], now, &style));
            }
        }
        self.render_sessions(&mut column, now, &style);
        ClippedScrollable::vertical(
            self.scroll_state.clone(),
            Self::pad(column.finish()),
            ScrollbarWidth::Auto,
            style.fg_20_fill,
            style.fg_40_fill,
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish()
    }

    fn accessibility_contents(&self, _ctx: &AppContext) -> Option<AccessibilityContent> {
        None
    }
}

impl AgentsPanel {
    fn pad(content: Box<dyn Element>) -> Box<dyn Element> {
        Container::new(content)
            .with_horizontal_padding(6.0)
            .with_vertical_padding(8.0)
            .finish()
    }

    fn text(
        &self,
        s: impl Into<String>,
        size: f32,
        color: ColorU,
        weight: Weight,
    ) -> Box<dyn Element> {
        match self.font_family {
            Some(family) => Text::new_inline(s.into(), family, size)
                .with_color(color)
                .with_style(Properties {
                    weight,
                    ..Default::default()
                })
                .finish(),
            None => Stack::new().finish(),
        }
    }

    fn render_header(&self, total: usize, style: &PanelStyle) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.0)
            .with_child(self.text(
                "AGENTS",
                style.font_size - 2.0,
                style.fg_60,
                Weight::Semibold,
            ));
        if total > 0 {
            row.add_child(self.text(
                total.to_string(),
                style.font_size - 2.0,
                style.fg_40,
                Weight::Normal,
            ));
        }
        Container::new(row.finish())
            .with_horizontal_padding(4.0)
            .with_padding_top(2.0)
            .with_padding_bottom(6.0)
            .finish()
    }

    fn render_section(
        &self,
        label: impl Into<String>,
        count: usize,
        style: &PanelStyle,
    ) -> Box<dyn Element> {
        let row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.0)
            .with_child(self.text(label, style.font_size - 3.0, style.fg_60, Weight::Semibold))
            .with_child(self.text(
                count.to_string(),
                style.font_size - 3.0,
                style.fg_40,
                Weight::Normal,
            ))
            .finish();
        Container::new(row)
            .with_horizontal_padding(4.0)
            .with_padding_top(10.0)
            .with_padding_bottom(2.0)
            .finish()
    }

    /// A small bordered button for the hover actions.
    fn render_pill(
        &self,
        label: &str,
        handle: MouseStateHandle,
        action: AgentsPanelAction,
        style: &PanelStyle,
    ) -> Box<dyn Element> {
        let text = self.text(
            label,
            style.font_size - 2.0,
            style.fg_full,
            Weight::Semibold,
        );
        Hoverable::new(handle, move |state| {
            let mut c = Container::new(text)
                .with_horizontal_padding(7.0)
                .with_vertical_padding(1.0)
                .with_border(Border::all(1.0).with_border_fill(style.fg_20_fill))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.0)));
            if state.is_hovered() {
                c = c.with_background(style.overlay_selected);
            }
            c.finish()
        })
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .finish()
    }

    fn render_note(&self, message: &str, style: &PanelStyle) -> Box<dyn Element> {
        Container::new(self.text(message, style.font_size - 1.0, style.fg_60, Weight::Normal))
            .with_horizontal_padding(ROW_H_PAD)
            .with_vertical_padding(ROW_V_PAD)
            .finish()
    }

    /// Build the plugin-health banner, or `None` when there's nothing to say.
    fn render_plugin_hint(&self, style: &PanelStyle) -> Option<Box<dyn Element>> {
        let hint = self.plugin_hint.as_ref()?;
        let fg = match hint.severity {
            PluginHintSeverity::Warning => style.fg_full,
            PluginHintSeverity::Info => style.fg_90,
        };
        let label = Clipped::new(self.text(
            hint.message.clone(),
            style.font_size - 1.0,
            fg,
            Weight::Normal,
        ))
        .finish();
        let bg = match hint.severity {
            PluginHintSeverity::Warning => style.overlay_selected,
            PluginHintSeverity::Info => style.overlay_hover,
        };
        Some(
            Container::new(label)
                .with_horizontal_padding(ROW_H_PAD)
                .with_vertical_padding(ROW_V_PAD)
                .with_background(bg)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.0)))
                .finish(),
        )
    }

    fn render_dot(&self, row: &AgentRow, fresh: bool, style: &PanelStyle) -> Box<dyn Element> {
        let dot = Container::new(Stack::new().finish())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(DOT_SIZE / 2.0)));
        let dot = match group_of(row) {
            Group::NeedsYou => dot.with_background(style.warning),
            Group::Working => dot.with_background(style.fg_full_fill),
            // New results: solid green. Seen ones and fresh sessions: a ring,
            // green when there is a result, grey when there is none yet.
            Group::Done if fresh => dot.with_background(style.done),
            Group::Done if row.response.is_some() => {
                dot.with_border(Border::all(1.5).with_border_fill(style.done))
            }
            Group::Done => dot.with_border(Border::all(1.0).with_border_fill(style.fg_60_fill)),
        };
        ConstrainedBox::new(dot.finish())
            .with_width(DOT_SIZE)
            .with_min_height(DOT_SIZE)
            .with_max_height(DOT_SIZE)
            .finish()
    }

    /// The SESSIONS section: a collapsible header with "refresh", then one
    /// sub-header per project and its sessions.
    fn render_sessions(&self, column: &mut Flex, now: SystemTime, style: &PanelStyle) {
        let total: usize = self.sessions.iter().map(|g| g.rows.len()).sum();
        column.add_child(self.render_sessions_header(total, style));
        if self.sessions_collapsed {
            return;
        }
        if self.sessions.is_empty() {
            column.add_child(self.render_note("No Claude sessions in your projects yet.", style));
            return;
        }
        let mut budget = MAX_SESSION_ROWS;
        for idx in session_group_order(&self.sessions, &self.active_project_path) {
            if budget == 0 {
                break;
            }
            let group = &self.sessions[idx];
            let expanded = self.sessions_expanded.contains(&group.root);
            let shown = visible_session_rows(group.rows.len(), expanded).min(budget);
            budget -= shown;
            column.add_child(self.render_session_group_header(group, expanded, style));
            for (i, row) in group.rows.iter().take(shown).enumerate() {
                column.add_child(self.render_session_row(row, i == 0, now, style));
            }
        }
    }

    fn render_sessions_header(&self, total: usize, style: &PanelStyle) -> Box<dyn Element> {
        let chevron = if self.sessions_collapsed {
            CHEVRON_CLOSED
        } else {
            CHEVRON_OPEN
        };
        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.0)
            .with_child(self.text(
                format!("{chevron} {SESSIONS_TITLE}"),
                style.font_size - 3.0,
                style.fg_60,
                Weight::Semibold,
            ));
        if total > 0 {
            row.add_child(self.text(
                total.to_string(),
                style.font_size - 3.0,
                style.fg_40,
                Weight::Normal,
            ));
        }
        let refresh = {
            Hoverable::new(self.sessions_refresh_handle.clone(), |state| {
                let color = if state.is_hovered() {
                    style.fg_full
                } else {
                    style.fg_40
                };
                Container::new(self.text(
                    REFRESH_LABEL,
                    style.font_size - 3.0,
                    color,
                    Weight::Normal,
                ))
                .with_horizontal_padding(4.0)
                .finish()
            })
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(AgentsPanelAction::RefreshSessions))
            .finish()
        };
        row.add_child(refresh);
        let body = Container::new(row.finish())
            .with_horizontal_padding(4.0)
            .with_padding_top(10.0)
            .with_padding_bottom(2.0)
            .finish();
        Hoverable::new(self.sessions_header_handle.clone(), |_| body)
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(AgentsPanelAction::ToggleSessions))
            .with_defer_events_to_children()
            .finish()
    }

    /// The project name, its session count and, past five, "more" / "less".
    fn render_session_group_header(
        &self,
        group: &SessionGroup,
        expanded: bool,
        style: &PanelStyle,
    ) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.0)
            .with_child(self.text(
                group.project.clone(),
                style.font_size - 2.0,
                style.fg_90,
                Weight::Semibold,
            ))
            .with_child(self.text(
                group.rows.len().to_string(),
                style.font_size - 3.0,
                style.fg_40,
                Weight::Normal,
            ));
        if group.rows.len() > SESSIONS_PER_PROJECT {
            let label = if expanded { LESS_LABEL } else { MORE_LABEL };
            let root = group.root.clone();
            row.add_child(
                Hoverable::new(
                    self.session_more_handles
                        .get(&group.root)
                        .cloned()
                        .unwrap_or_default(),
                    |state| {
                        let color = if state.is_hovered() {
                            style.fg_full
                        } else {
                            style.fg_60
                        };
                        self.text(label, style.font_size - 3.0, color, Weight::Normal)
                    },
                )
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(AgentsPanelAction::ToggleSessionsMore {
                        root: root.clone(),
                    })
                })
                .finish(),
            );
        }
        Container::new(Clipped::new(row.finish()).finish())
            .with_padding_left(6.0)
            .with_padding_right(4.0)
            .with_padding_top(6.0)
            .with_padding_bottom(1.0)
            .finish()
    }

    /// One line: `age * title [branch]`, and on the right what stops or
    /// shapes a resume ("running", "on <machine>", "folder missing", "from
    /// Windows") plus "open" for another machine's live session.
    fn render_session_row(
        &self,
        row: &SessionRow,
        newest: bool,
        now: SystemTime,
        style: &PanelStyle,
    ) -> Box<dyn Element> {
        let dimmed = matches!(row.state, SessionState::FolderMissing);
        let age = now
            .duration_since(row.modified)
            .map(format_session_age)
            .unwrap_or_else(|_| "now".to_string());
        let mut left = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.0)
            .with_child(
                ConstrainedBox::new(self.text(
                    age,
                    style.font_size - 3.0,
                    style.fg_40,
                    Weight::Normal,
                ))
                .with_min_width(SESSION_AGE_WIDTH)
                .finish(),
            );
        left.add_child(
            ConstrainedBox::new(if newest {
                self.text(
                    LATEST_MARKER,
                    style.font_size - 2.0,
                    style.fg_90,
                    Weight::Normal,
                )
            } else {
                Stack::new().finish()
            })
            .with_min_width(6.0)
            .finish(),
        );
        let title_color = if dimmed {
            style.fg_40
        } else if row.title_is_ai {
            style.fg_full
        } else {
            style.fg_90
        };
        left.add_child(self.text(
            truncate(&row.title, SESSION_TITLE_CHARS),
            style.font_size - 1.0,
            title_color,
            Weight::Normal,
        ));
        if let Some(branch) = &row.branch {
            left.add_child(
                Container::new(self.text(
                    truncate(branch, 18),
                    style.font_size - 3.0,
                    style.fg_60,
                    Weight::Normal,
                ))
                .with_horizontal_padding(5.0)
                .with_vertical_padding(1.0)
                .with_background(style.overlay_selected)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(3.0)))
                .finish(),
            );
        }

        let mut right = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.0);
        if let Some(note) = session_note(row) {
            let color = match row.state {
                SessionState::LiveHere { .. } => style.done_text,
                SessionState::LiveElsewhere { .. } => style.fg_90,
                _ => style.fg_40,
            };
            right.add_child(self.text(note, style.font_size - 3.0, color, Weight::Normal));
        }
        if let Some(url) = session_open_url(row) {
            right.add_child(
                self.render_pill(
                    OPEN_LABEL,
                    self.session_open_handles
                        .get(url)
                        .cloned()
                        .unwrap_or_default(),
                    AgentsPanelAction::OpenRemote {
                        url: url.to_string(),
                    },
                    style,
                ),
            );
        }

        let line = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.0)
            .with_child(Shrinkable::new(1.0, Clipped::new(left.finish()).finish()).finish())
            .with_child(right.finish())
            .finish();
        let hover_bg = style.overlay_hover;
        let body = move |hovered: bool| {
            let mut container = Container::new(line)
                .with_padding_left(6.0)
                .with_padding_right(4.0)
                .with_vertical_padding(2.0)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.0)));
            if hovered && !dimmed {
                container = container.with_background(hover_bg);
            }
            ConstrainedBox::new(container.finish())
                .with_min_height(SESSION_ROW_HEIGHT)
                .finish()
        };
        let in_place_id = row.id.clone();
        let fork_id = row.id.clone();
        Hoverable::new(
            self.session_row_handles
                .get(&row.id)
                .cloned()
                .unwrap_or_default(),
            move |state| body(state.is_hovered()),
        )
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(AgentsPanelAction::ResumeSession {
                session_id: in_place_id.clone(),
                fork: false,
            })
        })
        .on_right_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(AgentsPanelAction::ResumeSession {
                session_id: fork_id.clone(),
                fork: true,
            })
        })
        // "open" is the pill's alone.
        .with_defer_events_to_children()
        .finish()
    }

    /// One agent. `idx` is the row's index for a local agent; a remote agent
    /// (`None`) has no click and no hover actions, only an "open" pill when
    /// it carries a Remote Control URL.
    fn render_row(
        &self,
        idx: Option<usize>,
        row: &AgentRow,
        now: SystemTime,
        style: &PanelStyle,
    ) -> Box<dyn Element> {
        let blocked = matches!(row.status, AgentStatus::Blocked { .. });
        let group = group_of(row);
        let fresh = idx.is_some() && self.unseen.contains(&row.terminal_view_id_bits);
        // Finished agents the user has already looked at step back, so the
        // ones that need attention or have new results stand out.
        let receded = group == Group::Done && !fresh;
        let (title_color, detail_color) = if receded {
            (style.fg_60, style.fg_40)
        } else {
            (style.fg_full, style.fg_60)
        };
        let bits = row.terminal_view_id_bits;
        // Remote rows (`idx` None) carry list positions as ids, which can
        // collide with local ones, so they never consult the per-id maps.
        let suggest_state = idx.and_then(|_| self.suggesting.get(&bits));

        let project = if row.project.is_empty() {
            row.cwd
                .as_deref()
                .and_then(|c| c.rsplit(['/', '\\']).find(|s| !s.is_empty()))
                .unwrap_or("agent")
                .to_string()
        } else {
            row.project.clone()
        };
        let doing = match suggest_state {
            Some(SuggestState::Pending) => SUGGESTING_LABEL.to_string(),
            _ => activity_label(row),
        };
        let age = row
            .last_activity
            .and_then(|at| now.duration_since(at).ok())
            .map(format_age)
            .unwrap_or_default();
        let right_label = if age.is_empty() {
            doing
        } else {
            format!("{doing}  {age}")
        };
        let preview = match suggest_state {
            Some(SuggestState::Failed(reason)) => reason.clone(),
            _ => row_preview(row),
        };

        let build = |hovered: bool| -> Box<dyn Element> {
            let mut left = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.0)
                .with_child(self.render_dot(row, fresh, style))
                .with_child(self.text(
                    project.clone(),
                    style.font_size,
                    title_color,
                    Weight::Semibold,
                ));
            if row.agent != AgentKind::Claude {
                left.add_child(self.text(
                    row.agent.label(),
                    style.font_size - 2.0,
                    style.fg_60,
                    Weight::Normal,
                ));
            }

            let right: Box<dyn Element> = match idx {
                Some(_) if hovered && suggest_state != Some(&SuggestState::Pending) => {
                    let mut actions = Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(4.0);
                    let suggested = matches!(suggest_state, Some(SuggestState::Suggested { .. }));
                    if can_suggest(row) && !suggested {
                        actions.add_child(self.render_pill(
                            SUGGEST_LABEL,
                            self.suggest_handles.get(&bits).cloned().unwrap_or_default(),
                            AgentsPanelAction::Suggest {
                                terminal_view_id_bits: bits,
                            },
                            style,
                        ));
                    }
                    if row.agent.supports_cancel() {
                        let target = CancelTarget::CliAgent {
                            terminal_view_id_bits: bits,
                        };
                        actions.add_child(self.render_pill(
                            CANCEL_GLYPH,
                            self.cancel_handles.get(&bits).cloned().unwrap_or_default(),
                            AgentsPanelAction::Cancel { target },
                            style,
                        ));
                    }
                    actions.finish()
                }
                _ => {
                    let color = if blocked {
                        style.fg_full
                    } else if fresh {
                        style.done_text
                    } else {
                        detail_color
                    };
                    let label = self.text(
                        right_label.clone(),
                        style.font_size - 2.0,
                        color,
                        Weight::Normal,
                    );
                    match openable_url(row).filter(|_| idx.is_none()) {
                        Some(url) => Flex::row()
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_spacing(6.0)
                            .with_child(label)
                            .with_child(self.render_pill(
                                OPEN_LABEL,
                                self.open_handles.get(url).cloned().unwrap_or_default(),
                                AgentsPanelAction::OpenRemote {
                                    url: url.to_string(),
                                },
                                style,
                            ))
                            .finish(),
                        None => label,
                    }
                }
            };

            let line_one = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.0)
                .with_child(Shrinkable::new(1.0, Clipped::new(left.finish()).finish()).finish())
                .with_child(right)
                .finish();
            let mut lines = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(3.0)
                .with_child(line_one);
            // A session restored or just started has no prompt yet; an empty
            // second line ("...") only adds noise.
            if !preview.is_empty() {
                let line_two = Clipped::new(self.text(
                    preview.clone(),
                    style.font_size - 1.0,
                    if blocked || fresh {
                        style.fg_90
                    } else {
                        detail_color
                    },
                    Weight::Normal,
                ))
                .finish();
                lines.add_child(
                    Container::new(line_two)
                        .with_padding_left(DOT_SIZE + 8.0)
                        .finish(),
                );
            }
            let content = Container::new(lines.finish())
                .with_horizontal_padding(ROW_H_PAD)
                .with_vertical_padding(ROW_V_PAD)
                .finish();

            // A blocked agent needs the user: a bar on the left edge, visible
            // even when the row is hovered.
            let mut framed = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch);
            if blocked {
                framed.add_child(
                    ConstrainedBox::new(
                        Container::new(Stack::new().finish())
                            .with_background(style.accent)
                            .finish(),
                    )
                    .with_width(BLOCKED_BAR_WIDTH)
                    .finish(),
                );
            }
            framed.add_child(Expanded::new(1.0, content).finish());

            // Every row is a soft card; hover lifts it a step.
            let bg = if hovered {
                style.overlay_selected
            } else {
                style.overlay_hover
            };
            Container::new(framed.finish())
                .with_background(bg)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_RADIUS)))
                .finish()
        };

        if idx.is_none() {
            return build(false);
        }
        let row_handle = self.row_handles.get(&bits).cloned().unwrap_or_default();
        Hoverable::new(row_handle, |state| build(state.is_hovered()))
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(AgentsPanelAction::Activate {
                    terminal_view_id_bits: bits,
                });
            })
            // A click on a pill is the pill's alone; the row must not also
            // switch tabs.
            .with_defer_events_to_children()
            .finish()
    }
}

impl TypedActionView for AgentsPanel {
    type Action = AgentsPanelAction;

    fn handle_action(&mut self, action: &AgentsPanelAction, ctx: &mut ViewContext<Self>) {
        match action {
            AgentsPanelAction::Cancel { target } => {
                ctx.emit(AgentsPanelEvent::CancelRequested {
                    target: target.clone(),
                });
            }
            AgentsPanelAction::Activate {
                terminal_view_id_bits,
            } => {
                if self.unseen.remove(terminal_view_id_bits) {
                    ctx.notify();
                }
                ctx.emit(AgentsPanelEvent::ActivateRequested {
                    terminal_view_id_bits: *terminal_view_id_bits,
                });
            }
            AgentsPanelAction::Suggest {
                terminal_view_id_bits,
            } => {
                if matches!(
                    self.suggesting.get(terminal_view_id_bits),
                    Some(SuggestState::Pending | SuggestState::Suggested { .. })
                ) {
                    return;
                }
                self.suggesting
                    .insert(*terminal_view_id_bits, SuggestState::Pending);
                ctx.notify();
                ctx.emit(AgentsPanelEvent::SuggestRequested {
                    terminal_view_id_bits: *terminal_view_id_bits,
                });
            }
            AgentsPanelAction::OpenRemote { url } => {
                if is_remote_control_url(url) {
                    ctx.emit(AgentsPanelEvent::OpenRemoteRequested { url: url.clone() });
                }
            }
            AgentsPanelAction::ResumeSession { session_id, fork } => {
                self.resume_session(session_id, *fork, ctx);
            }
            AgentsPanelAction::ToggleSessions => {
                self.sessions_collapsed = !self.sessions_collapsed;
                ctx.notify();
            }
            AgentsPanelAction::ToggleSessionsMore { root } => {
                if !self.sessions_expanded.remove(root) {
                    self.sessions_expanded.insert(root.clone());
                }
                ctx.notify();
            }
            AgentsPanelAction::RefreshSessions => {
                ctx.emit(AgentsPanelEvent::RefreshSessionsRequested);
            }
        }
    }
}

impl AgentsPanel {
    /// A click (in place) or right-click (`fork`) on a past session, routed by its state.
    fn resume_session(&mut self, session_id: &str, fork: bool, ctx: &mut ViewContext<Self>) {
        // The list may have been replaced between paint and click.
        let Some((group, row)) = self
            .sessions
            .iter()
            .find_map(|g| g.rows.iter().find(|r| r.id == session_id).map(|r| (g, r)))
        else {
            return;
        };
        match session_click(&row.state, fork) {
            None => {}
            Some(SessionClick::Activate {
                terminal_view_id_bits,
            }) => {
                ctx.emit(AgentsPanelEvent::ActivateRequested {
                    terminal_view_id_bits,
                });
            }
            Some(SessionClick::Resume { fork }) => {
                if self
                    .session_launched
                    .get(session_id)
                    .is_some_and(|at| at.elapsed() < RELAUNCH_GUARD)
                {
                    return;
                }
                let event = AgentsPanelEvent::ResumeSessionRequested {
                    session_id: row.id.clone(),
                    cwd: row.cwd.clone(),
                    project_root: group.root.clone(),
                    project_id: group.project_id.clone(),
                    fork,
                };
                self.session_launched
                    .insert(session_id.to_string(), Instant::now());
                ctx.emit(event);
            }
        }
    }
}

/// What a click on a past session does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionClick {
    Resume { fork: bool },
    Activate { terminal_view_id_bits: u64 },
}

/// `!fork` is a plain click, `fork` a right-click. See the module docs.
fn session_click(state: &SessionState, fork: bool) -> Option<SessionClick> {
    match state {
        SessionState::Resumable => Some(SessionClick::Resume { fork }),
        SessionState::LiveHere {
            terminal_view_id_bits,
        } => Some(SessionClick::Activate {
            terminal_view_id_bits: *terminal_view_id_bits,
        }),
        // Never a second writer on a session another machine is (or may
        // still be) running: fork it.
        SessionState::LiveElsewhere { .. } | SessionState::PulledRecently => {
            Some(SessionClick::Resume { fork: true })
        }
        SessionState::FolderMissing => None,
    }
}

/// The note on the right of a session row.
fn session_note(row: &SessionRow) -> Option<String> {
    match &row.state {
        SessionState::Resumable | SessionState::PulledRecently => {
            row.origin.as_ref().map(|o| format!("from {o}"))
        }
        SessionState::LiveHere { .. } => Some("running".to_string()),
        SessionState::LiveElsewhere { machine, .. } => Some(format!("on {machine}")),
        SessionState::FolderMissing => Some("folder missing".to_string()),
    }
}

/// The Remote Control URL "open" offers on a session another machine runs.
fn session_open_url(row: &SessionRow) -> Option<&str> {
    match &row.state {
        SessionState::LiveElsewhere {
            remote_url: Some(url),
            ..
        } if is_remote_control_url(url) => Some(url.as_str()),
        _ => None,
    }
}

/// Project groups in display order: the active project first (the one whose
/// root holds `active_path`, the deepest when roots nest), then the project
/// with the newest session, then by name.
fn session_group_order(groups: &[SessionGroup], active_path: &str) -> Vec<usize> {
    let active = (!active_path.is_empty()).then(|| Path::new(active_path));
    let active_idx = active.and_then(|active| {
        groups
            .iter()
            .enumerate()
            .filter(|(_, g)| active.starts_with(&g.root))
            .max_by_key(|(_, g)| g.root.as_os_str().len())
            .map(|(i, _)| i)
    });
    let newest = |g: &SessionGroup| g.rows.iter().map(|r| r.modified).max();
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&a, &b| {
        (Some(b) == active_idx)
            .cmp(&(Some(a) == active_idx))
            .then_with(|| newest(&groups[b]).cmp(&newest(&groups[a])))
            .then_with(|| groups[a].project.cmp(&groups[b].project))
    });
    order
}

/// How many of a project's `len` sessions show.
fn visible_session_rows(len: usize, expanded: bool) -> usize {
    if expanded {
        len
    } else {
        len.min(SESSIONS_PER_PROJECT)
    }
}

/// Like [`format_age`], with weeks past a week: "now", "5m", "3h", "6d", "12w".
fn format_session_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs >= 7 * 86_400 {
        format!("{}w", (secs / (7 * 86_400)).min(99))
    } else {
        format_age(elapsed)
    }
}

/// Row indices in display order: by group (needs you, working, done), then
/// newest first (no `last_activity` last), then project.
fn sorted_order(rows: &[AgentRow]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        group_of(ra)
            .cmp(&group_of(rb))
            // `Some` sorts above `None`, so descending puts `None` last.
            .then_with(|| rb.last_activity.cmp(&ra.last_activity))
            .then_with(|| ra.project.cmp(&rb.project))
    });
    order
}

/// Whether a row's suggest state survives a row update. A typed suggestion
/// is spent once the agent moves on (its group or `last_activity` changes);
/// pending and failed states have their own lifetimes.
fn suggest_state_still_current(state: &SuggestState, row: &AgentRow) -> bool {
    match state {
        SuggestState::Suggested {
            group,
            last_activity,
        } => group_of(row) == *group && row.last_activity == *last_activity,
        SuggestState::Pending | SuggestState::Failed(_) => true,
    }
}

/// "suggest" is offered on a Claude agent that is waiting for the user: idle
/// after a reply, or restored and not yet prompted (its transcript still has
/// the last exchange).
fn can_suggest(row: &AgentRow) -> bool {
    row.agent == AgentKind::Claude
        && match row.status {
            AgentStatus::Success => true,
            AgentStatus::InProgress => never_prompted(row),
            AgentStatus::Blocked { .. } => false,
        }
}

/// The Remote Control URL "open" should offer for a row, if it has a valid one.
fn openable_url(row: &AgentRow) -> Option<&str> {
    row.remote_url
        .as_deref()
        .filter(|url| is_remote_control_url(url))
}

/// What the agent is doing, for the right side of line one.
fn activity_label(row: &AgentRow) -> String {
    match &row.status {
        AgentStatus::Blocked { .. } => "needs you".to_string(),
        // The turn ended but subagents, background shells or monitors are
        // still going: the agent is not done yet.
        AgentStatus::InProgress if row.background_tasks > 0 => {
            background_label(row.background_tasks)
        }
        // A session restored after a restart, or just started, reports as in
        // progress before it has been asked anything; it is waiting for input.
        AgentStatus::InProgress if never_prompted(row) => "ready".to_string(),
        AgentStatus::InProgress => match row.tool_name.as_deref().filter(|t| !t.is_empty()) {
            Some(tool) => truncate(tool, 18),
            None => "working".to_string(),
        },
        AgentStatus::Success => "done".to_string(),
    }
}

/// "1 background task", "3 background tasks".
fn background_label(count: u32) -> String {
    match count {
        1 => "1 background task".to_string(),
        n => format!("{n} background tasks"),
    }
}

/// The panel's three groups, in display order (the derived `Ord` is that order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    NeedsYou,
    Working,
    Done,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::NeedsYou => "NEEDS YOU",
            Group::Working => "WORKING",
            Group::Done => "DONE",
        }
    }
}

/// A restored or new session that has not been prompted reports as in
/// progress, but it is waiting for the user like a finished one.
fn group_of(row: &AgentRow) -> Group {
    match row.status {
        AgentStatus::Blocked { .. } => Group::NeedsYou,
        AgentStatus::InProgress if row.background_tasks > 0 => Group::Working,
        AgentStatus::InProgress if never_prompted(row) => Group::Done,
        AgentStatus::InProgress => Group::Working,
        AgentStatus::Success => Group::Done,
    }
}

/// No prompt, reply or tool seen yet.
fn never_prompted(row: &AgentRow) -> bool {
    row.query.is_none() && row.response.is_none() && row.tool_name.is_none()
}

/// Line two. Blocked: what it is asking. Working: the prompt it is working
/// on, or, once only background tasks are left, what the turn answered. Done:
/// what it answered (the result is what a finished row is for), else the
/// prompt.
fn row_preview(row: &AgentRow) -> String {
    let (first, second) = match &row.status {
        AgentStatus::Blocked { message } => (message.as_deref(), row.query.as_deref()),
        AgentStatus::InProgress if row.background_tasks > 0 => {
            (row.response.as_deref(), row.query.as_deref())
        }
        AgentStatus::InProgress => (row.query.as_deref(), None),
        AgentStatus::Success => (row.response.as_deref(), row.query.as_deref()),
    };
    [first, second]
        .into_iter()
        .flatten()
        .map(clean_preview)
        .find(|s| !s.is_empty())
        .map(|s| truncate(&s, PREVIEW_CHARS))
        .unwrap_or_default()
}

/// Strip what Claude Code's harness and markdown put into prompts and replies
/// so the preview reads like a sentence:
/// - harness messages become a short label (a background task finishing, a
///   message from another session, a continued conversation, a slash command);
/// - `<system-reminder>` blocks, `[Image ...]` placeholders and any remaining
///   `<tag>`s go;
/// - markdown code fences, headings, emphasis and bullets go;
/// - whitespace collapses to single spaces.
pub fn clean_preview(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("<task-notification>") {
        return "Background task finished".to_string();
    }
    if trimmed.starts_with("Another Claude session sent a message")
        || trimmed.starts_with("<cross-session-message")
    {
        return "Message from another session".to_string();
    }
    if trimmed.starts_with("This session is being continued from a previous conversation") {
        return "Continued from an earlier conversation".to_string();
    }
    if trimmed.starts_with("<command-") {
        if let Some(name) = between(trimmed, "<command-name>", "</command-name>") {
            let args = between(trimmed, "<command-args>", "</command-args>").unwrap_or("");
            return format!("{} {}", name.trim(), args.trim())
                .trim()
                .to_string();
        }
    }

    let mut s = trimmed.to_string();
    while let Some(start) = s.find("<system-reminder>") {
        match s[start..].find("</system-reminder>") {
            Some(len) => s.replace_range(start..start + len + "</system-reminder>".len(), " "),
            None => s.truncate(start),
        }
    }
    let s = remove_spans(&s, "[Image", ']');
    let s = remove_tags(&s);

    let mut words: Vec<&str> = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            continue;
        }
        let line = line.trim_start_matches('#').trim_start();
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line);
        words.extend(line.split_whitespace());
    }
    words.join(" ").replace("**", "").replace('`', "")
}

/// The text between the first `open` and the following `close`.
fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let len = s[start..].find(close)?;
    Some(&s[start..start + len])
}

/// Drop every span from `open` through the next `close` (or the end).
fn remove_spans(s: &str, open: &str, close: char) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(close) {
            Some(end) => rest = &rest[start + end + close.len_utf8()..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

/// Drop harness `<tag ...>` and `</tag>` markup, keeping the text between.
/// Only names with a `-` or `_` count (`system-reminder`,
/// `local-command-stdout`, `pasted_content`, ...), so prose and code such as
/// `a < b`, `<-` or `Vec<String>` stay.
fn remove_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match (is_harness_tag(after), after.find('>')) {
            (true, Some(end)) => {
                out.push(' ');
                rest = &after[end + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether the text after a `<` opens or closes a harness tag: a name that
/// starts with a letter, is made of letters, digits, `-` and `_`, holds at
/// least one `-` or `_`, and ends at whitespace, `/` or `>`.
fn is_harness_tag(after_lt: &str) -> bool {
    let body = after_lt.strip_prefix('/').unwrap_or(after_lt);
    let end = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(body.len());
    let name = &body[..end];
    let terminated = body[end..]
        .chars()
        .next()
        .is_some_and(|c| c == '>' || c == '/' || c.is_whitespace());
    name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.contains(['-', '_'])
        && terminated
}

/// "now", "5m", "3h", "2d".
pub fn format_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(ELLIPSIS.len())).collect();
    out.push_str(ELLIPSIS);
    out
}

#[derive(Clone, Copy)]
struct PanelStyle {
    fg_full: ColorU,
    fg_90: ColorU,
    fg_60: ColorU,
    fg_40: ColorU,
    fg_full_fill: Fill,
    fg_20_fill: Fill,
    fg_40_fill: Fill,
    fg_60_fill: Fill,
    overlay_hover: Fill,
    overlay_selected: Fill,
    accent: Fill,
    warning: Fill,
    done: Fill,
    done_text: ColorU,
    font_size: f32,
}

impl PanelStyle {
    fn from_appearance(app: &AppContext) -> Self {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        Self {
            fg_full: theme.foreground().into_solid(),
            fg_90: theme.foreground().with_opacity(90).into_solid(),
            fg_60: theme.foreground().with_opacity(60).into_solid(),
            fg_40: theme.foreground().with_opacity(40).into_solid(),
            fg_full_fill: theme.foreground().into(),
            fg_20_fill: theme.foreground().with_opacity(20).into(),
            fg_40_fill: theme.foreground().with_opacity(40).into(),
            fg_60_fill: theme.foreground().with_opacity(60).into(),
            overlay_hover: internal_colors::fg_overlay_1(theme).into(),
            overlay_selected: internal_colors::fg_overlay_2(theme).into(),
            accent: theme.accent().into(),
            warning: Fill::Solid(theme.ui_yellow_color()),
            done: Fill::Solid(theme.ansi_fg_green()),
            done_text: theme.ansi_fg_green(),
            font_size: appearance.ui_font_size(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph the panel draws must exist in the shipped UI font. The
    /// first git-tree draft used non-ASCII chevrons and they rendered as
    /// `*` and a superscript `v` on Windows.
    #[test]
    fn all_chrome_glyphs_are_ascii() {
        for glyph in [
            CANCEL_GLYPH,
            ELLIPSIS,
            SUGGEST_LABEL,
            SUGGESTING_LABEL,
            OPEN_LABEL,
            background_label(1).as_str(),
            background_label(12).as_str(),
            "AGENTS",
            "needs you",
            "working",
            "ready",
            "done",
            "NEEDS YOU",
            "WORKING",
            "DONE",
            "ON ",
            SESSIONS_TITLE,
            REFRESH_LABEL,
            MORE_LABEL,
            LESS_LABEL,
            CHEVRON_OPEN,
            CHEVRON_CLOSED,
            LATEST_MARKER,
            "running",
            "on ",
            "folder missing",
            "from ",
            "No Claude sessions in your projects yet.",
        ] {
            assert!(glyph.is_ascii(), "non-ASCII glyph in panel chrome: {glyph}");
        }
    }

    #[test]
    fn truncate_is_char_safe_and_ascii_suffixed() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(
            truncate("tr\u{e6}ningsdag \u{f8}\u{e5}", 8),
            "tr\u{e6}ni..."
        );
        let long = "\u{e6}".repeat(200);
        let out = truncate(&long, 160);
        assert_eq!(out.chars().count(), 160);
        assert!(out.ends_with(ELLIPSIS));
    }

    #[test]
    fn previews_drop_harness_markup() {
        assert_eq!(
            clean_preview(
                "<task-notification><task-id>wusi02hca</task-id><tool-use-id>x</tool-use-id>"
            ),
            "Background task finished"
        );
        assert_eq!(
            clean_preview("agent menuen er dårlig [Image #8]"),
            "agent menuen er dårlig"
        );
        assert_eq!(
            clean_preview("fix it <system-reminder>secret harness text</system-reminder> now"),
            "fix it now"
        );
        assert_eq!(clean_preview("  two\n\n lines  "), "two lines");
        assert_eq!(clean_preview("unclosed [Image #3"), "unclosed");
        assert_eq!(
            clean_preview("[Image: source: /private/tmp/x/12.png] make it nicer"),
            "make it nicer"
        );
        assert_eq!(
            clean_preview(
                "Another Claude session sent a message: <cross-session-message from=x>hi"
            ),
            "Message from another session"
        );
        assert_eq!(
            clean_preview(
                "This session is being continued from a previous conversation that ran out"
            ),
            "Continued from an earlier conversation"
        );
        assert_eq!(
            clean_preview("<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>42</command-args>"),
            "/review 42"
        );
        assert_eq!(
            clean_preview("```yaml\nstatus: ok\n```\n## Done\n- **pushed** `abc`"),
            "status: ok Done pushed abc"
        );
        assert_eq!(
            clean_preview("<b>bold</b> and a < b"),
            "<b>bold</b> and a < b"
        );
        assert_eq!(
            clean_preview("use Vec<String> here"),
            "use Vec<String> here"
        );
        assert_eq!(
            clean_preview("<local-command-stdout>ok</local-command-stdout>"),
            "ok"
        );
        assert_eq!(
            clean_preview("see <pasted_content id=1>the log</pasted_content> above"),
            "see the log above"
        );
        // The slash-command label only applies to a command message.
        assert_eq!(
            clean_preview("why does <command-name>/x</command-name> show"),
            "why does /x show"
        );
    }

    fn row(status: AgentStatus, query: Option<&str>, response: Option<&str>) -> AgentRow {
        AgentRow {
            terminal_view_id_bits: 1,
            agent: AgentKind::Claude,
            status,
            project: "p".into(),
            cwd: None,
            query: query.map(Into::into),
            response: response.map(Into::into),
            tool_name: None,
            last_activity: None,
            background_tasks: 0,
            remote_url: None,
            session_id: None,
        }
    }

    #[test]
    fn finished_rows_preview_their_result() {
        let done = row(
            AgentStatus::Success,
            Some("fix it"),
            Some("Fixed and pushed."),
        );
        assert_eq!(row_preview(&done), "Fixed and pushed.");
        let working = row(AgentStatus::InProgress, Some("fix it"), Some("old reply"));
        assert_eq!(row_preview(&working), "fix it");
        let no_reply = row(AgentStatus::Success, Some("fix it"), None);
        assert_eq!(row_preview(&no_reply), "fix it");
        let fresh = row(AgentStatus::InProgress, None, None);
        assert_eq!(group_of(&fresh), Group::Done);
        assert_eq!(row_preview(&fresh), "");
    }

    #[test]
    fn ages_are_coarse() {
        assert_eq!(format_age(Duration::from_secs(12)), "now");
        assert_eq!(format_age(Duration::from_secs(185)), "3m");
        assert_eq!(format_age(Duration::from_secs(2 * 3600 + 5)), "2h");
        assert_eq!(format_age(Duration::from_secs(3 * 86_400)), "3d");
    }

    #[test]
    fn rows_sort_by_group_then_newest_first() {
        let at = |secs: u64| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        let with = |status: AgentStatus, query: Option<&str>, last, project: &str| AgentRow {
            last_activity: last,
            project: project.into(),
            ..row(status, query, None)
        };
        let rows = vec![
            // A restored, unprompted session: DONE, older than the result.
            with(AgentStatus::InProgress, None, at(100), "restored"),
            with(AgentStatus::Success, Some("q"), at(200), "result"),
            with(AgentStatus::Success, Some("q"), None, "undated"),
            with(AgentStatus::InProgress, Some("q"), at(50), "working"),
            with(
                AgentStatus::Blocked { message: None },
                Some("q"),
                at(10),
                "blocked",
            ),
        ];
        let order: Vec<&str> = sorted_order(&rows)
            .into_iter()
            .map(|i| rows[i].project.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["blocked", "working", "result", "restored", "undated"]
        );
    }

    #[test]
    fn typed_suggestion_lasts_until_the_agent_moves_on() {
        let at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
        let idle = AgentRow {
            last_activity: at,
            ..row(AgentStatus::Success, Some("q"), Some("a"))
        };
        let state = SuggestState::Suggested {
            group: Group::Done,
            last_activity: at,
        };
        assert!(suggest_state_still_current(&state, &idle));
        let newer = AgentRow {
            last_activity: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(101)),
            ..idle.clone()
        };
        assert!(!suggest_state_still_current(&state, &newer));
        let working = AgentRow {
            status: AgentStatus::InProgress,
            ..idle.clone()
        };
        assert!(!suggest_state_still_current(&state, &working));
        assert!(suggest_state_still_current(
            &SuggestState::Pending,
            &working
        ));
    }

    #[test]
    fn background_tasks_keep_a_finished_turn_working() {
        let waiting = AgentRow {
            background_tasks: 2,
            ..row(
                AgentStatus::InProgress,
                Some("fix it"),
                Some("Started 2 agents."),
            )
        };
        assert_eq!(group_of(&waiting), Group::Working);
        assert_eq!(activity_label(&waiting), "2 background tasks");
        assert_eq!(row_preview(&waiting), "Started 2 agents.");
        assert!(!can_suggest(&waiting));
        let one = AgentRow {
            background_tasks: 1,
            ..row(AgentStatus::InProgress, None, None)
        };
        // Not mistaken for a restored, never-prompted session.
        assert_eq!(group_of(&one), Group::Working);
        assert_eq!(activity_label(&one), "1 background task");
        // A stale count on a finished row changes nothing.
        let done = AgentRow {
            background_tasks: 3,
            ..row(AgentStatus::Success, Some("q"), Some("a"))
        };
        assert_eq!(group_of(&done), Group::Done);
        assert_eq!(activity_label(&done), "done");
    }

    #[test]
    fn only_claude_remote_control_urls_are_openable() {
        let ok = "https://claude.ai/code/session_01TestFakeSessionIdAbc";
        assert!(is_remote_control_url(ok));
        for bad in [
            "",
            "https://claude.ai/code/session_",
            "http://claude.ai/code/session_01Test",
            "https://claude.ai.evil.com/code/session_01Test",
            "https://evil.com/?https://claude.ai/code/session_01Test",
            "https://claude.ai/code/session_01Test/../../logout",
            "https://claude.ai/code/session_01Test?next=https://evil.com",
            "https://claude.ai/code/session_01Test#x",
            "file:///etc/passwd",
            "javascript:alert(1)",
        ] {
            assert!(!is_remote_control_url(bad), "accepted {bad}");
        }
        let remote = AgentRow {
            remote_url: Some(ok.into()),
            ..row(AgentStatus::Success, None, None)
        };
        assert_eq!(openable_url(&remote), Some(ok));
        let bad = AgentRow {
            remote_url: Some("https://evil.com".into()),
            ..remote.clone()
        };
        assert_eq!(openable_url(&bad), None);
    }

    fn session(id: &str, secs: u64, state: SessionState) -> SessionRow {
        SessionRow {
            id: id.into(),
            title: id.into(),
            title_is_ai: true,
            branch: None,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            cwd: PathBuf::from("/code/p"),
            origin: None,
            state,
        }
    }

    fn group(project: &str, root: &str, rows: Vec<SessionRow>) -> SessionGroup {
        SessionGroup {
            project_id: Some(project.into()),
            project: project.into(),
            root: PathBuf::from(root),
            rows,
        }
    }

    #[test]
    fn session_groups_put_the_active_project_first_then_the_newest() {
        let groups = vec![
            group(
                "old",
                "/code/old",
                vec![session("a", 10, SessionState::Resumable)],
            ),
            group(
                "new",
                "/code/new",
                vec![session("b", 30, SessionState::Resumable)],
            ),
            group(
                "active",
                "/code/active",
                vec![session("c", 20, SessionState::Resumable)],
            ),
            group(
                "nested",
                "/code/active/sub",
                vec![session("d", 5, SessionState::Resumable)],
            ),
        ];
        let names = |order: Vec<usize>| -> Vec<&str> {
            order
                .into_iter()
                .map(|i| groups[i].project.as_str())
                .collect()
        };
        assert_eq!(
            names(session_group_order(&groups, "/code/active/src")),
            vec!["active", "new", "old", "nested"]
        );
        // The deepest root holding the active path wins.
        assert_eq!(
            names(session_group_order(&groups, "/code/active/sub/x")),
            vec!["nested", "new", "active", "old"]
        );
        // No active project, or one without sessions: newest first.
        assert_eq!(
            names(session_group_order(&groups, "")),
            vec!["new", "active", "old", "nested"]
        );
        assert_eq!(
            names(session_group_order(&groups, "/elsewhere")),
            vec!["new", "active", "old", "nested"]
        );
        // `/code/activex` is not under `/code/active`.
        assert_eq!(
            names(session_group_order(&groups, "/code/activex")),
            vec!["new", "active", "old", "nested"]
        );
    }

    #[test]
    fn five_sessions_per_project_until_more() {
        assert_eq!(visible_session_rows(3, false), 3);
        assert_eq!(visible_session_rows(12, false), SESSIONS_PER_PROJECT);
        assert_eq!(visible_session_rows(12, true), 12);
    }

    #[test]
    fn session_clicks_follow_the_state() {
        use SessionClick::*;
        let idle = SessionState::Resumable;
        assert_eq!(session_click(&idle, true), Some(Resume { fork: true }));
        assert_eq!(session_click(&idle, false), Some(Resume { fork: false }));

        // Running here: both clicks switch to it, never a second process.
        let here = SessionState::LiveHere {
            terminal_view_id_bits: 7,
        };
        assert_eq!(
            session_click(&here, true),
            Some(Activate {
                terminal_view_id_bits: 7
            })
        );
        assert_eq!(
            session_click(&here, false),
            Some(Activate {
                terminal_view_id_bits: 7
            })
        );

        // Running on another machine: either click forks, never in place.
        let elsewhere = SessionState::LiveElsewhere {
            machine: "ALICE-PC".into(),
            remote_url: None,
        };
        assert_eq!(session_click(&elsewhere, true), Some(Resume { fork: true }));
        assert_eq!(session_click(&elsewhere, false), Some(Resume { fork: true }));

        // Pulled from another machine minutes ago: either click forks too.
        let recent = SessionState::PulledRecently;
        assert_eq!(session_click(&recent, true), Some(Resume { fork: true }));
        assert_eq!(session_click(&recent, false), Some(Resume { fork: true }));

        assert_eq!(session_click(&SessionState::FolderMissing, true), None);
        assert_eq!(session_click(&SessionState::FolderMissing, false), None);
    }

    #[test]
    fn session_rows_say_why_and_offer_open_only_with_a_valid_url() {
        let ok = "https://claude.ai/code/session_01Test";
        let elsewhere = |url: Option<&str>| {
            session(
                "s",
                1,
                SessionState::LiveElsewhere {
                    machine: "ALICE-PC".into(),
                    remote_url: url.map(Into::into),
                },
            )
        };
        assert_eq!(session_open_url(&elsewhere(Some(ok))), Some(ok));
        assert_eq!(
            session_open_url(&elsewhere(Some("https://evil.example/x"))),
            None
        );
        assert_eq!(session_open_url(&elsewhere(None)), None);
        assert_eq!(
            session_note(&elsewhere(None)).as_deref(),
            Some("on ALICE-PC")
        );
        assert_eq!(
            session_open_url(&session("s", 1, SessionState::Resumable)),
            None
        );

        let mut pulled = session("s", 1, SessionState::Resumable);
        assert_eq!(session_note(&pulled), None);
        pulled.origin = Some("Windows".into());
        assert_eq!(session_note(&pulled).as_deref(), Some("from Windows"));
        pulled.state = SessionState::PulledRecently;
        assert_eq!(session_note(&pulled).as_deref(), Some("from Windows"));
        pulled.state = SessionState::Resumable;
        assert_eq!(
            session_note(&session("s", 1, SessionState::FolderMissing)).as_deref(),
            Some("folder missing")
        );
        for note in [
            session_note(&elsewhere(None)),
            session_note(&pulled),
            session_note(&session(
                "s",
                1,
                SessionState::LiveHere {
                    terminal_view_id_bits: 1,
                },
            )),
        ] {
            assert!(note.unwrap().is_ascii());
        }
    }

    #[test]
    fn session_ages_reach_weeks() {
        assert_eq!(format_session_age(Duration::from_secs(90)), "1m");
        assert_eq!(format_session_age(Duration::from_secs(6 * 86_400)), "6d");
        assert_eq!(format_session_age(Duration::from_secs(15 * 86_400)), "2w");
    }

    #[test]
    fn groups_order_needs_you_working_done() {
        assert!(Group::NeedsYou < Group::Working);
        assert!(Group::Working < Group::Done);
    }
}
