use crate::{
    object::{CloudObject, ObjectMetadata},
    object_permissions::ObjectPermissions,
    schema,
};

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Folder {
    pub name: String,
    pub metadata: ObjectMetadata,
    pub permissions: ObjectPermissions,
    #[cynic(rename = "isMnemosPack")]
    pub is_mnemos_pack: bool,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct FolderWithDescendants {
    pub descendants: Vec<CloudObject>,
    pub folder: Folder,
}
