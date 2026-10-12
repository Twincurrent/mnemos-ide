use mnemos_core::features::FeatureFlag;
use mnemos_ui::{elements::MouseStateHandle, AppContext, Element};

use crate::{
    appearance::Appearance,
    cloud_object::CloudObjectMetadata,
    drive::{
        cloud_object_styling::mnemos_drive_icon_color, folders::CloudFolder, index::DriveIndexAction,
        CloudObjectTypeAndId, DriveObjectType,
    },
    themes::theme::Fill,
    ui_components::icons::Icon,
};

use super::{MnemosDriveItem, MnemosDriveItemId};

#[derive(Clone)]
pub struct MnemosDriveFolder {
    id: CloudObjectTypeAndId,
    folder: CloudFolder,
}

impl MnemosDriveFolder {
    pub fn new(id: CloudObjectTypeAndId, folder: CloudFolder) -> Self {
        Self { id, folder }
    }
}

impl MnemosDriveItem for MnemosDriveFolder {
    fn display_name(&self) -> Option<String> {
        if self.folder.model().name.is_empty() {
            None
        } else {
            Some(self.folder.model().name.clone())
        }
    }

    fn metadata(&self) -> Option<&CloudObjectMetadata> {
        Some(&self.folder.metadata)
    }

    fn object_type(&self) -> Option<DriveObjectType> {
        Some(DriveObjectType::Folder)
    }

    fn icon(&self, appearance: &Appearance, color: Option<Fill>) -> Option<Box<dyn Element>> {
        let icon_fill =
            color.unwrap_or(mnemos_drive_icon_color(appearance, DriveObjectType::Folder).into());
        let icon = if FeatureFlag::MnemosPacks.is_enabled() && self.folder.model().is_mnemos_pack {
            Icon::PackageCheck
        } else {
            Icon::from(DriveObjectType::Folder)
        };

        Some(icon.to_mnemosui_icon(icon_fill).finish())
    }

    fn secondary_icon(&self, _color: Option<Fill>) -> Option<Box<dyn Element>> {
        None
    }

    fn is_folder_open(&self) -> Option<bool> {
        Some(self.folder.model().is_open)
    }

    fn click_action(&self) -> Option<DriveIndexAction> {
        Some(DriveIndexAction::ToggleFolderOpen(self.folder.id))
    }

    fn preview(&self, _: &Appearance) -> Option<Box<dyn Element>> {
        None
    }

    fn mnemos_drive_id(&self) -> MnemosDriveItemId {
        MnemosDriveItemId::Object(self.id)
    }

    fn sync_status_icon(
        &self,
        sync_queue_is_dequeueing: bool,
        hover_state: MouseStateHandle,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        self.folder.metadata.pending_changes_statuses.render_icon(
            sync_queue_is_dequeueing,
            hover_state,
            appearance,
        )
    }

    fn action_summary(&self, _app: &AppContext) -> Option<String> {
        None
    }

    fn clone_box(&self) -> Box<dyn MnemosDriveItem> {
        Box::new(self.clone())
    }
}
