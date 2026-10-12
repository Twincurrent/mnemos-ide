use mnemos_ui::{elements::MouseStateHandle, Element};

use crate::{
    appearance::Appearance,
    cloud_object::{CloudObjectMetadata, Space},
    drive::{index::DriveIndexAction, DriveObjectType},
    themes::theme::Fill,
};

use super::{MnemosDriveItem, MnemosDriveItemId};

#[derive(Clone)]
pub struct MnemosDriveSpace {
    space: Space,
}

impl MnemosDriveSpace {
    #[allow(dead_code)]
    pub fn new(space: Space) -> Self {
        Self { space }
    }
}

impl MnemosDriveItem for MnemosDriveSpace {
    fn display_name(&self) -> Option<String> {
        None
    }

    fn metadata(&self) -> Option<&CloudObjectMetadata> {
        None
    }

    fn object_type(&self) -> Option<DriveObjectType> {
        None
    }

    fn secondary_icon(&self, _color: Option<Fill>) -> Option<Box<dyn Element>> {
        None
    }

    fn click_action(&self) -> Option<DriveIndexAction> {
        None
    }

    fn preview(&self, _appearance: &Appearance) -> Option<Box<dyn Element>> {
        None
    }

    fn mnemos_drive_id(&self) -> MnemosDriveItemId {
        MnemosDriveItemId::Space(self.space)
    }

    fn sync_status_icon(
        &self,
        _sync_queue_is_dequeueing: bool,
        _hover_state: MouseStateHandle,
        _appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        None
    }

    fn clone_box(&self) -> Box<dyn MnemosDriveItem> {
        Box::new(self.clone())
    }

    fn action_summary(&self, _app: &mnemos_ui::AppContext) -> Option<String> {
        None
    }
}
