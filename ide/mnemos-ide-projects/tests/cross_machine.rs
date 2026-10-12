// SPDX-License-Identifier: AGPL-3.0-only
//! Real-data acceptance test for cross-machine projects.
//!
//! Replays what happened on Alice's two machines, from the files they actually
//! had on 2026-09-24:
//! - `registry_server.json`: the server copy of `projects.json` (identical to
//!   the Windows file): 15 records.
//! - `registry_mac.json`: the Mac's file, the same first 12 records plus
//!   gizmo.
//! - `machine_mac.json` / `machine_win.json`: each machine's checkouts (with
//!   their git remotes) and the tabs its SQLite restored.
//!
//! Each machine is a fake filesystem plus a real `ProjectStore` on disk in a
//! temp dir, updated through `ProjectStore::update_blocking_at` exactly as the
//! app will. The server is played by exchanging registries and running
//! `registry::merge` on each side.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;
use mnemos_ide_projects::registry::{self, is_fs_root, path_parent, same_path};
use mnemos_ide_projects::{
    plan, FsEnv, LocalResolver, ProjectStore, ReconcileMode, ReconcilePlan, TabSnapshot,
};

const ADMIN: &str = "3f83538b-9bd1-4cbb-a9b5-e41fae754c35";
const GLOBEX: &str = "566a0580-ae4b-4c5b-ab14-4fc4ea28ab89";
const PORTFOLIO: &str = "da72feae-5e88-4bd4-ab33-352c0e565af9";
const TCBV2: &str = "3092ffc6-28d9-4af6-a2a3-04e7a379d9e4";
const GIZMO: &str = "project-1788169499352";
const SKOLE: &str = "project-1789463394705";
const MNEMOS: &str = "project-1790233543395";
// Ids the Mac tabs carried for records the old sync had erased; reconcile
// re-registers sprocket and games under the same id.
const SPROCKET: &str = "95808a98-3004-473f-a389-665d734452a4";
const GAMES: &str = "project-1787490980893";
// Not re-registered: its tab ("notes app") sits in ~/code/qa, a
// folder whose name contradicts the title, so the tab is unlinked instead.
const THINKING: &str = "project-1785937133450";

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 24, h, m, 0).unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct DirFixture {
    path: String,
    git: bool,
    remote: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TabFixture {
    key: usize,
    title: Option<String>,
    project_id: Option<String>,
    cwds: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MachineFixture {
    platform: String,
    home: String,
    dirs: Vec<DirFixture>,
    tabs: Vec<TabFixture>,
}

fn fixture_text(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture_text(name)).unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// A machine described as data
// ─────────────────────────────────────────────────────────────────────────────

struct FakeFs {
    platform: &'static str,
    home: String,
    dirs: Vec<DirFixture>,
    /// Every directory that exists: the listed ones, their ancestors, home.
    all: Vec<String>,
}

impl FakeFs {
    fn new(m: &MachineFixture) -> Self {
        let platform: &'static str = match m.platform.as_str() {
            "darwin" => "darwin",
            "win32" => "win32",
            _ => "linux",
        };
        let mut all: Vec<String> = Vec::new();
        let listed = std::iter::once(m.home.clone()).chain(m.dirs.iter().map(|d| d.path.clone()));
        for start in listed {
            let mut cur = Some(start);
            while let Some(c) = cur {
                if !all.iter().any(|a| same_path(a, &c, platform)) {
                    all.push(c.clone());
                }
                cur = path_parent(&c, platform);
            }
        }
        Self {
            platform,
            home: m.home.clone(),
            dirs: m.dirs.clone(),
            all,
        }
    }

    fn listed(&self, path: &str) -> Option<&DirFixture> {
        self.dirs
            .iter()
            .find(|d| same_path(&d.path, path, self.platform))
    }
}

impl FsEnv for FakeFs {
    fn platform(&self) -> &'static str {
        self.platform
    }

    fn is_dir(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        self.all.iter().any(|a| same_path(a, &s, self.platform))
    }

    fn home(&self) -> Option<PathBuf> {
        Some(PathBuf::from(&self.home))
    }

    fn git_remote(&self, dir: &Path) -> Option<String> {
        self.listed(&dir.to_string_lossy())?
            .remote
            .as_deref()
            .and_then(registry::normalize_git_remote)
    }

    fn git_root(&self, cwd: &Path) -> Option<PathBuf> {
        let mut cur = Some(cwd.to_string_lossy().into_owned());
        while let Some(c) = cur {
            if same_path(&c, &self.home, self.platform) || is_fs_root(&c, self.platform) {
                return None;
            }
            if let Some(d) = self.listed(&c) {
                if d.git {
                    return Some(PathBuf::from(&d.path));
                }
            }
            cur = path_parent(&c, self.platform);
        }
        None
    }

    fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
        let s = dir.to_string_lossy();
        self.all
            .iter()
            .filter(|a| {
                path_parent(a, self.platform).is_some_and(|p| same_path(&p, &s, self.platform))
            })
            .map(PathBuf::from)
            .collect()
    }
}

struct Machine {
    env: FakeFs,
    resolver: LocalResolver,
    tabs: Vec<TabSnapshot>,
    next_key: usize,
    projects_path: PathBuf,
    workspace_path: PathBuf,
    _dir: tempfile::TempDir,
}

impl Machine {
    fn new(machine: &str, registry: &str) -> Self {
        let m: MachineFixture = serde_json::from_str(&fixture_text(machine)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let projects_path = dir.path().join("projects.json");
        let workspace_path = dir.path().join("workspace_state.json");
        std::fs::write(&projects_path, fixture_text(registry)).unwrap();
        let tabs: Vec<TabSnapshot> = m
            .tabs
            .iter()
            .map(|t| TabSnapshot {
                key: t.key,
                project_id: t.project_id.clone(),
                title: t.title.clone(),
                cwds: t.cwds.iter().map(PathBuf::from).collect(),
                safe_to_close: true,
            })
            .collect();
        let next_key = tabs.iter().map(|t| t.key).max().unwrap_or(0) + 1;
        Self {
            env: FakeFs::new(&m),
            resolver: LocalResolver::new(),
            tabs,
            next_key,
            projects_path,
            workspace_path,
            _dir: dir,
        }
    }

    fn platform(&self) -> &'static str {
        self.env.platform
    }

    fn store(&self) -> ProjectStore {
        ProjectStore::load_blocking_from(&self.projects_path, &self.workspace_path).unwrap()
    }

    fn registry(&self) -> Value {
        self.store().registry_value()
    }

    fn update<R>(&self, f: impl FnOnce(&mut ProjectStore) -> R) -> R {
        ProjectStore::update_blocking_at(&self.projects_path, &self.workspace_path, f).unwrap()
    }

    /// Reconcile against the store on disk (locked update, saved if changed).
    fn plan(&mut self, mode: ReconcileMode, now: DateTime<Utc>) -> ReconcilePlan {
        let (env, resolver, tabs) = (&self.env, &mut self.resolver, &self.tabs);
        ProjectStore::update_blocking_at(&self.projects_path, &self.workspace_path, |store| {
            plan(store, tabs, mode, env, resolver, now)
        })
        .unwrap()
    }

    /// Execute a plan the way the app does.
    fn apply(&mut self, p: &ReconcilePlan) {
        for (key, id) in &p.relink {
            self.tab_mut(*key).project_id = id.clone();
        }
        self.tabs.retain(|t| !p.close.contains(&t.key));
        for o in &p.open {
            let cwd = o.cwd.clone().unwrap_or_else(|| PathBuf::from(&self.env.home));
            self.tabs.push(TabSnapshot {
                key: self.next_key,
                project_id: Some(o.project_id.clone()),
                title: Some(o.title.clone()),
                cwds: vec![cwd],
                safe_to_close: true,
            });
            self.next_key += 1;
        }
    }

    /// Pull: merge a remote registry into ours (what mnemos-ide-sync's apply does).
    fn pull(&self, remote: &Value) -> Value {
        let platform = self.platform();
        self.update(|store| {
            let merged = registry::merge(&store.registry_value(), remote, platform);
            store.replace_registry(&merged).unwrap();
            merged
        })
    }

    fn tab(&self, key: usize) -> &TabSnapshot {
        self.tabs.iter().find(|t| t.key == key).unwrap()
    }

    fn tab_mut(&mut self, key: usize) -> &mut TabSnapshot {
        self.tabs.iter_mut().find(|t| t.key == key).unwrap()
    }

    fn tab_ids(&self) -> BTreeSet<String> {
        self.tabs.iter().filter_map(|t| t.project_id.clone()).collect()
    }

    fn open_ids(&self) -> BTreeSet<String> {
        self.store().open_project_ids().into_iter().collect()
    }

    /// `distinct_roots`: also require that no two tabs in different project
    /// roots share an id. Holds once the machines have converged; right after
    /// the Mac's first startup its Skole tab still rides on tcbv2's id,
    /// because the skole record has not arrived from Windows yet.
    fn assert_invariants(&self, label: &str, distinct_roots: bool) {
        let store = self.store();
        // Every open record has a tab here.
        let tab_ids = self.tab_ids();
        for id in store.open_project_ids() {
            assert!(tab_ids.contains(&id), "{label}: open record {id} has no tab");
        }
        // Every linked tab points at a live record.
        for id in &tab_ids {
            assert!(store.projects.iter().any(|p| &p.id == id), "{label}: tab links dead id {id}");
        }
        if !distinct_roots {
            return;
        }
        // No tab id is shared by tabs sitting in two different project roots.
        let mut roots: HashMap<String, BTreeSet<String>> = HashMap::new();
        for t in &self.tabs {
            let Some(id) = &t.project_id else { continue };
            let root = t
                .cwds
                .iter()
                .find_map(|c| mnemos_ide_projects::tab_root(&self.env, c));
            if let Some(root) = root {
                roots
                    .entry(id.clone())
                    .or_default()
                    .insert(root.to_string_lossy().to_lowercase());
            }
        }
        for (id, set) in roots {
            assert!(set.len() <= 1, "{label}: tabs of {id} sit in different roots {set:?}");
        }
    }
}

fn relinks(p: &ReconcilePlan) -> BTreeMap<usize, Option<String>> {
    p.relink.iter().cloned().collect()
}

fn record<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("record {id} missing"))
}

fn live_ids(v: &Value) -> BTreeSet<String> {
    v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// Both machines' first start with this build, then one exchange through the
/// server. Returns the machines and the id the Mac minted for `mobile`.
fn startups() -> (Machine, Machine, String) {
    let mut mac = Machine::new("machine_mac.json", "registry_mac.json");
    let mut win = Machine::new("machine_win.json", "registry_server.json");

    // ---- Mac startup ----
    let p = mac.plan(ReconcileMode::Startup, at(8, 0));
    assert!(p.store_changed);
    assert!(p.open.is_empty(), "mac startup opened {:?}", p.open);
    assert!(p.close.is_empty());
    let r = relinks(&p);
    let mobile = r[&8].clone().expect("mobile gets its own record");
    assert_eq!(r[&1].as_deref(), Some(PORTFOLIO));
    assert_eq!(r[&2].as_deref(), Some(TCBV2));
    // Skole has no record on the Mac yet; it follows the id it shared with tcbv2.
    assert_eq!(r[&3].as_deref(), Some(TCBV2));
    assert_eq!(r[&7].as_deref(), Some(GLOBEX));
    assert_ne!(mobile, GLOBEX, "mobile is a different repo than globex");
    assert_eq!(r[&12].as_deref(), Some(PORTFOLIO));
    for key in [4, 5, 6, 9, 10] {
        assert!(!r.contains_key(&key), "tab {key} should keep its id, got {:?}", r.get(&key));
    }
    // "notes app" in ~/code/qa: the title contradicts the folder, so
    // the dead id is dropped and the tab stays a plain tab.
    assert_eq!(r[&11], None);
    mac.apply(&p);
    let reg = mac.registry();
    assert_eq!(record(&reg, SPROCKET)["name"], "sprocket");
    assert_eq!(record(&reg, SPROCKET)["gitRepo"], "acme/sprocket");
    assert_eq!(record(&reg, SPROCKET)["paths"]["darwin"], "/Users/alice/code/sprocket");
    assert_eq!(record(&reg, &mobile)["gitRepo"], "globex/GXMOBILE");
    assert_eq!(record(&reg, &mobile)["paths"]["darwin"], "/Users/alice/Development/globex/mobile");
    assert_eq!(record(&reg, GAMES)["paths"]["darwin"], "/Users/alice/code/games");
    assert!(!live_ids(&reg).contains(THINKING), "thinking must not be registered at ~/code/qa");
    assert!(
        reg["projects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["paths"]["darwin"] != "/Users/alice/code/qa"),
        "no record may claim ~/code/qa"
    );
    assert_eq!(record(&reg, GIZMO)["gitRepo"], "acme/gizmo");
    assert_eq!(record(&reg, PORTFOLIO)["paths"]["darwin"], "/Users/alice/code/Portfolio");
    mac.assert_invariants("mac startup", false);
    assert!(mac.plan(ReconcileMode::Startup, at(8, 1)).is_empty(), "mac startup is idempotent");

    // ---- Windows startup ----
    let p = win.plan(ReconcileMode::Startup, at(9, 0));
    assert!(p.store_changed);
    assert!(p.open.is_empty(), "win startup opened {:?}", p.open);
    assert!(p.close.is_empty());
    // mnemos learns it is acme/TCB and folds into the Mac's tcbv2.
    assert_eq!(relinks(&p), BTreeMap::from([(3, Some(TCBV2.to_string()))]));
    win.apply(&p);
    let reg = win.registry();
    assert_eq!(registry::resolve_id(&reg, MNEMOS).as_deref(), Some(TCBV2));
    assert!(!live_ids(&reg).contains(MNEMOS));
    assert_eq!(record(&reg, TCBV2)["paths"]["win32"], "C:\\code\\mnemos");
    assert_eq!(record(&reg, TCBV2)["paths"]["darwin"], "/Users/alice/code/tcbv2");
    assert_eq!(record(&reg, GIZMO)["paths"]["win32"], "C:\\code\\gizmo");
    win.assert_invariants("win startup", true);
    assert!(win.plan(ReconcileMode::Startup, at(9, 1)).is_empty(), "win startup is idempotent");

    (mac, win, mobile)
}

/// Exchange registries through the "server": each side merges the other's.
fn exchange(mac: &Machine, win: &Machine) {
    let r_mac = mac.registry();
    let r_win = win.registry();
    let on_win = win.pull(&r_mac);
    let on_mac = mac.pull(&r_win);
    assert!(
        registry::projects_semantically_equal(&on_win, &on_mac),
        "machines disagree after merge:\nwin: {on_win:#}\nmac: {on_mac:#}"
    );
    // Stored forms agree too (normalize-on-save changes nothing further).
    assert!(registry::projects_semantically_equal(&win.registry(), &mac.registry()));
}

/// The full scenario up to a converged steady state.
fn converged() -> (Machine, Machine, String) {
    let (mut mac, mut win, mobile) = startups();
    exchange(&mac, &win);

    let expected: BTreeSet<String> = [
        PORTFOLIO, TCBV2, SKOLE, ADMIN, SPROCKET, GLOBEX, &mobile, GAMES, GIZMO,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(mac.open_ids(), expected);
    assert_eq!(win.open_ids(), expected);

    // ---- Windows live ----
    let p = win.plan(ReconcileMode::Live, at(10, 5));
    assert!(p.relink.is_empty(), "win live relinked {:?}", p.relink);
    assert!(p.close.is_empty());
    let opened: BTreeMap<String, Option<String>> = p
        .open
        .iter()
        .map(|o| (o.project_id.clone(), o.cwd.as_ref().map(|c| c.to_string_lossy().into_owned())))
        .collect();
    let want: BTreeMap<String, Option<String>> = [
        (PORTFOLIO, Some("C:\\code\\portfolio")),
        (SPROCKET, Some("C:\\code\\sprocket")),
        (GAMES, Some("C:\\code\\games")),
        (GLOBEX, Some("C:\\development\\globex")),
        (ADMIN, Some("C:\\code\\Admin")),
        (mobile.as_str(), None),
    ]
    .into_iter()
    .map(|(id, cwd)| (id.to_string(), cwd.map(str::to_string)))
    .collect();
    assert_eq!(opened, want);
    let titles: BTreeMap<&str, &str> =
        p.open.iter().map(|o| (o.project_id.as_str(), o.title.as_str())).collect();
    assert_eq!(titles[PORTFOLIO], "Portfolio");
    win.apply(&p);

    // ---- Mac live ----
    let p = mac.plan(ReconcileMode::Live, at(10, 0));
    assert_eq!(relinks(&p), BTreeMap::from([(3, Some(SKOLE.to_string()))]));
    assert!(p.open.is_empty(), "mac live opened {:?}", p.open);
    assert!(p.close.is_empty());
    mac.apply(&p);
    assert_eq!(record(&mac.registry(), SKOLE)["paths"]["darwin"], "/Users/alice/skole");

    for m in [&mac, &win] {
        assert_eq!(m.open_ids(), expected);
        assert_eq!(m.tab_ids(), expected);
    }
    mac.assert_invariants("mac live", true);
    win.assert_invariants("win live", true);
    assert!(mac.plan(ReconcileMode::Live, at(10, 10)).is_empty(), "mac live is idempotent");
    assert!(win.plan(ReconcileMode::Live, at(10, 10)).is_empty(), "win live is idempotent");

    // What each side learned travels too, and changes nothing on screen.
    exchange(&mac, &win);
    let reg = mac.registry();
    assert_eq!(record(&reg, SKOLE)["paths"]["darwin"], "/Users/alice/skole");
    assert_eq!(record(&reg, SKOLE)["paths"]["win32"], "C:\\code\\skole");
    assert_eq!(record(&reg, PORTFOLIO)["paths"]["win32"], "C:\\code\\portfolio");
    assert_eq!(record(&reg, PORTFOLIO)["paths"]["darwin"], "/Users/alice/code/Portfolio");
    assert!(mac.plan(ReconcileMode::Live, at(10, 20)).is_empty());
    assert!(win.plan(ReconcileMode::Live, at(10, 20)).is_empty());
    assert!(mac.plan(ReconcileMode::Startup, at(10, 21)).is_empty());
    assert!(win.plan(ReconcileMode::Startup, at(10, 21)).is_empty());

    (mac, win, mobile)
}

#[test]
fn mac_and_windows_converge_on_the_same_tabs() {
    let (mac, win, _) = converged();
    // The Mac's Skole tab is the Windows skole project.
    assert_eq!(mac.tab(3).project_id.as_deref(), Some(SKOLE));
    // The Windows mnemos tab is tcbv2.
    assert_eq!(win.tab(3).project_id.as_deref(), Some(TCBV2));
    assert_eq!(mac.tab(2).project_id.as_deref(), Some(TCBV2));
}

#[test]
fn closing_on_one_machine_closes_on_the_other() {
    let (mut mac, mut win, _) = converged();
    let win_games_tab = win
        .tabs
        .iter()
        .find(|t| t.project_id.as_deref() == Some(GAMES))
        .unwrap()
        .key;

    // Windows closes games: the flag goes false, the record stays.
    assert!(win.update(|s| s.set_open(GAMES, false, at(11, 0))));
    assert!(win.store().get(GAMES).is_some());
    let p = win.plan(ReconcileMode::Live, at(11, 1));
    assert_eq!(p.close, vec![win_games_tab]);
    win.apply(&p);

    exchange(&mac, &win);
    assert!(!mac.open_ids().contains(GAMES));
    assert!(mac.store().get(GAMES).is_some(), "closing never removes the record");

    // A busy games tab on the Mac stays, even at startup. Startup used to
    // close it regardless (a restored tab was assumed to run nothing yet),
    // but agent-restore can resume a Claude session in a restored pane.
    mac.tab_mut(9).safe_to_close = false;
    let p = mac.plan(ReconcileMode::Startup, at(11, 5));
    assert!(p.close.is_empty(), "startup must not close a busy tab: {p:?}");
    assert_eq!(p.kept, vec![9]);
    assert!(p.open.is_empty() && p.relink.is_empty());

    // Live leaves it alone too, and does not reopen the project...
    let p = mac.plan(ReconcileMode::Live, at(11, 6));
    assert!(p.is_empty(), "live must not close a busy tab: {p:?}");
    assert_eq!(p.kept, vec![9]);
    assert!(!mac.open_ids().contains(GAMES));

    // ...and closes it once it is idle.
    mac.tab_mut(9).safe_to_close = true;
    let p = mac.plan(ReconcileMode::Live, at(11, 7));
    assert_eq!(p.close, vec![9]);
    mac.apply(&p);
    assert!(mac.plan(ReconcileMode::Live, at(11, 8)).is_empty());

    // Reopening on the Mac travels back the same way.
    let reopened = mac.update(|s| {
        s.register_or_claim_on("darwin", Path::new("/Users/alice/code/games"), "games", None, at(12, 0))
    });
    assert_eq!(reopened, GAMES);
    exchange(&mac, &win);
    let p = win.plan(ReconcileMode::Live, at(12, 1));
    assert_eq!(p.open.len(), 1);
    assert_eq!(p.open[0].project_id, GAMES);
    assert_eq!(p.open[0].cwd.as_deref(), Some(Path::new("C:\\code\\games")));
}

#[test]
fn tombstones_never_resurrect() {
    let (mut mac, win, _) = startups();

    // A replaced id: merging the pre-fold server copy (which still has the
    // mnemos record) back in never brings it back.
    let server = fixture_json("registry_server.json");
    let r_win = win.registry();
    for merged in [
        registry::merge(&r_win, &server, "win32"),
        registry::merge(&server, &r_win, "darwin"),
    ] {
        assert!(!live_ids(&merged).contains(MNEMOS));
        assert_eq!(registry::resolve_id(&merged, MNEMOS).as_deref(), Some(TCBV2));
    }

    // A deleted project: Windows deletes games after it has seen the Mac's.
    exchange(&mac, &win);
    let stale_mac = mac.registry();
    assert!(win.update(|s| s.delete_project(GAMES, at(12, 0))).is_some());
    let r_win = win.registry();
    for merged in [
        registry::merge(&r_win, &stale_mac, "win32"),
        registry::merge(&stale_mac, &r_win, "darwin"),
    ] {
        assert!(!live_ids(&merged).contains(GAMES));
        assert_eq!(registry::resolve_id(&merged, GAMES), None);
    }
    exchange(&mac, &win);
    // Merging the stale copy a second time still does not bring it back.
    win.pull(&stale_mac);
    assert!(!live_ids(&win.registry()).contains(GAMES));

    // The Mac still has a games tab. Reconcile may re-register the folder,
    // but never under the deleted id.
    let p = mac.plan(ReconcileMode::Startup, at(12, 5));
    let r = relinks(&p);
    let new_id = r[&9].clone().expect("games tab gets a fresh record");
    assert_ne!(new_id, GAMES);
    mac.apply(&p);
    assert!(!live_ids(&mac.registry()).contains(GAMES));
    assert!(mac.plan(ReconcileMode::Startup, at(12, 6)).is_empty());
}

#[test]
fn merge_is_commutative_and_lossless_on_fixture() {
    let mac0 = fixture_json("registry_mac.json");
    let server = fixture_json("registry_server.json");
    let union: BTreeSet<String> = live_ids(&mac0).union(&live_ids(&server)).cloned().collect();
    assert_eq!(union.len(), 15);
    for platform in ["darwin", "win32", "linux"] {
        let ab = registry::merge(&mac0, &server, platform);
        let ba = registry::merge(&server, &mac0, platform);
        assert!(registry::projects_semantically_equal(&ab, &ba), "{platform}");
        assert_eq!(live_ids(&ab), union, "{platform}: a record was dropped");
    }

    // Same after both startups, when the two registries really differ.
    let (mac, win, _) = startups();
    let (r_mac, r_win) = (mac.registry(), win.registry());
    let expected: BTreeSet<String> = live_ids(&r_mac).union(&live_ids(&r_win)).cloned().collect();
    for platform in ["darwin", "win32"] {
        let ab = registry::merge(&r_mac, &r_win, platform);
        let ba = registry::merge(&r_win, &r_mac, platform);
        assert!(registry::projects_semantically_equal(&ab, &ba), "{platform}");
        assert_eq!(live_ids(&ab), expected, "{platform}: a record was dropped");
        // Merging is idempotent too.
        let again = registry::merge(&ab, &r_win, platform);
        assert!(registry::projects_semantically_equal(&ab, &again), "{platform}");
    }
}
