use super::{MnemosDriveItem, MnemosDriveItemId};
use crate::{
    ai::mcp::CloudMCPServer,
    appearance::Appearance,
    cloud_object::CloudObjectMetadata,
    drive::{index::DriveIndexAction, CloudObjectTypeAndId, DriveObjectType},
    themes::theme::Fill,
};
use mnemos_ui::{elements::MouseStateHandle, AppContext, Element};

#[derive(Clone)]
pub struct MnemosDriveMCPServer {
    id: CloudObjectTypeAndId,
    mcp_server: CloudMCPServer,
}

impl MnemosDriveMCPServer {
    pub fn new(id: CloudObjectTypeAndId, mcp_server: CloudMCPServer) -> Self {
        Self { id, mcp_server }
    }
}

impl MnemosDriveItem for MnemosDriveMCPServer {
    fn display_name(&self) -> Option<String> {
        Some(self.mcp_server.model().string_model.name.clone())
    }
    fn metadata(&self) -> Option<&CloudObjectMetadata> {
        Some(&self.mcp_server.metadata)
    }

    fn object_type(&self) -> Option<DriveObjectType> {
        Some(DriveObjectType::MCPServer)
    }

    fn secondary_icon(&self, _color: Option<Fill>) -> Option<Box<dyn Element>> {
        None
    }

    fn click_action(&self) -> Option<DriveIndexAction> {
        Some(DriveIndexAction::OpenMCPServerCollection)
    }

    fn preview(&self, _appearance: &Appearance) -> Option<Box<dyn Element>> {
        // TODO
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
        self.mcp_server
            .metadata
            .pending_changes_statuses
            .render_icon(sync_queue_is_dequeueing, hover_state, appearance)
    }

    fn action_summary(&self, _app: &AppContext) -> Option<String> {
        None
    }

    fn clone_box(&self) -> Box<dyn MnemosDriveItem> {
        Box::new(self.clone())
    }
}
