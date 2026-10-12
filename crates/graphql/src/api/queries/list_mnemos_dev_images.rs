use crate::{error::UserFacingError, schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct ListMnemosDevImagesVariables {}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "RootQuery", variables = "ListMnemosDevImagesVariables")]
pub struct ListMnemosDevImages {
    #[cynic(rename = "listMnemosDevImages")]
    pub list_mnemos_dev_images: ListMnemosDevImagesResult,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ListMnemosDevImagesOutput {
    pub images: Vec<ImageTag>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ImageTag {
    pub image: String,
    pub repository: String,
    pub tag: String,
}

#[derive(cynic::InlineFragments, Debug)]
pub enum ListMnemosDevImagesResult {
    ListMnemosDevImagesOutput(ListMnemosDevImagesOutput),
    UserFacingError(UserFacingError),
    #[cynic(fallback)]
    Unknown,
}

crate::client::define_operation! {
    ListMnemosDevImages(ListMnemosDevImagesVariables) -> ListMnemosDevImages;
}
