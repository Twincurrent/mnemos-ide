ALTER TABLE windows ADD mnemos_ai_width FLOAT CHECK (mnemos_ai_width >= 0);
ALTER TABLE windows ADD voltron_width FLOAT CHECK (voltron_width >= 0);
ALTER TABLE windows ADD mnemos_drive_index_width FLOAT CHECK (mnemos_drive_index_width >= 0);
ALTER TABLE windows ADD mnemos_drive_asset_width FLOAT CHECK (mnemos_drive_asset_width >= 0);
