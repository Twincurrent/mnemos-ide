use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::terminal::model::session::Session;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MnemosAiOsContext {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub category: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub distribution: Option<String>,
}

/// The execution context of the active session. This struct
/// is sent as a JSON blob in our AI prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MnemosAiExecutionContext {
    pub os: MnemosAiOsContext,
    pub shell_name: String,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shell_version: Option<String>,
}

impl MnemosAiExecutionContext {
    pub fn new(session: &Arc<Session>) -> Self {
        MnemosAiExecutionContext {
            os: MnemosAiOsContext {
                category: session.host_info().os_category.clone(),
                distribution: session.host_info().linux_distribution.clone(),
            },
            shell_name: session.shell().shell_type().name().to_owned(),
            shell_version: session.shell().version().clone(),
        }
    }
}
impl MnemosAiExecutionContext {
    pub fn to_json_string(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }
}
