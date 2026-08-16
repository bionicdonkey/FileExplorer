use std::env;
use std::path::{Path, PathBuf};

/// A filesystem location that can be displayed and passed to local file APIs.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Location {
    path: PathBuf,
}

impl Location {
    /// Returns the current user's home directory.
    pub fn home() -> Self {
        let path = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self::from_path(path)
    }

    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A concise label suitable for a tab, breadcrumb, or sidebar row.
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }
}
