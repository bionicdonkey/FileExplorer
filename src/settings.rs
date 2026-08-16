use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub startup_location: String,
    pub view_mode: String,
    pub show_sidebar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            startup_location: "home".into(),
            view_mode: "details".into(),
            show_sidebar: true,
        }
    }
}
