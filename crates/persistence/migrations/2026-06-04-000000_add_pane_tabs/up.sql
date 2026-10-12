-- Browser-style terminal tabs (root-level only).
--
-- Each project tab (`tabs` row) can now hold several terminal *tabs*, each its
-- own independent pane tree. `pane_tabs` is the per-project-tab list of those
-- terminal tabs; the existing `pane_nodes` tree rows gain a nullable
-- `pane_tab_id` so each terminal tab's tree is grouped under one `pane_tabs`
-- row. `pane_nodes.tab_id` is kept so the existing save/restore traversal
-- (keyed by project tab) and foreign keys continue to work unchanged.
--
-- Backward compatibility: pre-migration pane trees have no `pane_tabs` row and
-- their `pane_nodes.pane_tab_id` is NULL. The restore path treats a project tab
-- with no `pane_tabs` rows as a single terminal tab built from the legacy root
-- (`parent_pane_node_id IS NULL`).
CREATE TABLE pane_tabs (
  id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
  tab_id INTEGER NOT NULL,
  position INTEGER NOT NULL,
  title TEXT,
  is_active BOOLEAN NOT NULL DEFAULT 0,
  -- The in-app PaneTabId (a UUID) as raw bytes, so a terminal tab keeps the same
  -- stable id across restarts (used for click/close routing). NULL is tolerated
  -- and reconstructed as a fresh id on restore.
  tab_uuid BLOB,

  FOREIGN KEY (tab_id) REFERENCES tabs (id) ON DELETE CASCADE,
  UNIQUE (tab_id, position)
);

ALTER TABLE pane_nodes ADD COLUMN pane_tab_id INTEGER REFERENCES pane_tabs (id) ON DELETE CASCADE;
