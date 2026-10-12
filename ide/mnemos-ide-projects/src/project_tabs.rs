// SPDX-License-Identifier: AGPL-3.0-only
//! `ProjectTabs` — the in-memory "which projects are open, which one is
//! active" model the workspace keeps alongside its native tabs.
//!
//! This used to be a rendered title-bar tab bar (`IdeProjectTabBar`) with
//! click / close / add handlers. In Mnemos IDE every native workspace tab *is* a
//! project, so the native tab strip is the thing the user clicks; the bar only
//! ever painted inside the vertical-tabs layout, which we never use. The
//! rendering was removed on 2026-09-02; what remains is the state the
//! workspace still reads (Agents-panel sort, Cmd+W fallback project id).

use mnemos_ide_core::types::Project;

#[derive(Debug, Clone, Default)]
pub struct ProjectTabs {
    projects: Vec<Project>,
    active_id: Option<String>,
}

impl ProjectTabs {
    /// `active_id` is validated against `projects`. The on-disk store may
    /// carry a stale id (v1 sync left `project-{ts}` ids pointing at projects
    /// since renamed or removed); falling back to the first project keeps
    /// `active_project()` consistent with what consumers (Agents-panel sort)
    /// expect instead of silently returning `None`.
    pub fn new(projects: Vec<Project>, active_id: Option<String>) -> Self {
        let active_id = match active_id {
            Some(id) if projects.iter().any(|p| p.id == id) => Some(id),
            _ => projects.first().map(|p| p.id.clone()),
        };
        Self {
            projects,
            active_id,
        }
    }

    /// Replace the project list. The active id is dropped if it points at a
    /// project that is no longer in the list.
    pub fn set_projects(&mut self, projects: Vec<Project>) {
        self.projects = projects;
        if let Some(id) = self.active_id.as_deref() {
            if !self.projects.iter().any(|p| p.id == id) {
                self.active_id = None;
            }
        }
    }

    /// Ignored when `id` is not one of the open projects.
    pub fn set_active(&mut self, id: &str) {
        if self.projects.iter().any(|p| p.id == id) {
            self.active_id = Some(id.to_string());
        }
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active_id.as_deref()
    }

    /// The active project, looked up against the live in-memory list.
    /// `None` if nothing is active or the id is stale. Prefer this over
    /// `active_project_id` from the on-disk store, which can hold stale
    /// v1-sync ids.
    pub fn active_project(&self) -> Option<&Project> {
        let id = self.active_id.as_deref()?;
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(id: &str) -> Project {
        Project {
            id: id.into(),
            name: format!("p-{id}"),
            path: PathBuf::from("/tmp"),
            created_at: chrono::Utc::now(),
            last_opened_at: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn new_falls_back_to_first_project_when_active_is_stale() {
        let tabs = ProjectTabs::new(vec![p("a"), p("b")], Some("gone".into()));
        assert_eq!(tabs.active_id(), Some("a"));
        let tabs = ProjectTabs::new(vec![p("a"), p("b")], Some("b".into()));
        assert_eq!(tabs.active_id(), Some("b"));
        let tabs = ProjectTabs::new(Vec::new(), Some("b".into()));
        assert_eq!(tabs.active_id(), None);
    }

    #[test]
    fn set_active_only_accepts_known_ids() {
        let mut tabs = ProjectTabs::new(vec![p("a"), p("b")], Some("a".into()));
        tabs.set_active("nonexistent");
        assert_eq!(tabs.active_id(), Some("a"));
        tabs.set_active("b");
        assert_eq!(tabs.active_id(), Some("b"));
    }

    #[test]
    fn set_projects_keeps_active_when_it_survives() {
        let mut tabs = ProjectTabs::new(vec![p("a"), p("b")], Some("a".into()));
        tabs.set_projects(vec![p("a"), p("c")]);
        assert_eq!(tabs.active_id(), Some("a"));
        assert_eq!(tabs.active_project().map(|p| p.name.as_str()), Some("p-a"));
        assert_eq!(tabs.projects().len(), 2);
    }

    #[test]
    fn set_projects_clears_active_when_active_removed() {
        let mut tabs = ProjectTabs::new(vec![p("a"), p("b")], Some("a".into()));
        tabs.set_projects(vec![p("b"), p("c")]);
        assert_eq!(tabs.active_id(), None);
        assert!(tabs.active_project().is_none());
    }
}
