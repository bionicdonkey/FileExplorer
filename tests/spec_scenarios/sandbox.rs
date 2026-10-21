//! The temporary directory a scenario runs in, built from `given.tree` (spec 3.2).

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{spec_dir, Check};

pub struct Sandbox {
    dir: tempfile::TempDir,
    restricted: Vec<PathBuf>,
}

impl Sandbox {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().expect("sandbox"), restricted: Vec::new() }
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    /// Builds entries. Shared by `given.tree` and the `mutate` action.
    pub fn build(&mut self, tree: &Value) -> Check {
        for entry in tree.as_array().into_iter().flatten() {
            self.build_entry(entry)?;
        }
        Ok(())
    }

    fn build_entry(&mut self, entry: &Value) -> Check {
        let io = |e: std::io::Error| format!("building {entry}: {e}");
        if let Some(dir) = entry["dir"].as_str() {
            let path = self.path(dir);
            std::fs::create_dir_all(&path).map_err(io)?;
            self.restrict(&path, entry["mode"].as_str())?;
        } else if let Some(file) = entry["file"].as_str() {
            let path = self.path(file);
            std::fs::create_dir_all(path.parent().unwrap()).map_err(io)?;
            let content = match entry["content"].as_str() {
                Some(text) => text.as_bytes().to_vec(),
                None => vec![b'x'; entry["bytes"].as_u64().unwrap_or(0) as usize],
            };
            std::fs::write(&path, content).map_err(io)?;
            self.restrict(&path, entry["mode"].as_str())?;
        } else if let Some(fixture) = entry["copy_fixture"].as_str() {
            let to = self.path(entry["to"].as_str().ok_or("copy_fixture needs `to`")?);
            std::fs::create_dir_all(to.parent().unwrap()).map_err(io)?;
            std::fs::copy(spec_dir().join("fixtures").join(fixture), &to).map_err(io)?;
        } else if let Some(link) = entry["symlink"].as_str() {
            let target = entry["target"].as_str().ok_or("symlink needs `target`")?;
            let path = self.path(link);
            std::fs::create_dir_all(path.parent().unwrap()).map_err(io)?;
            std::os::unix::fs::symlink(self.path(target), path).map_err(io)?;
        } else if let Some(dir) = entry["generate"].as_str() {
            let count = entry["count"].as_u64().ok_or("generate needs `count`")?;
            let pattern = entry["pattern"].as_str().ok_or("generate needs `pattern`")?;
            let base = self.path(dir);
            std::fs::create_dir_all(&base).map_err(io)?;
            for n in 0..count {
                std::fs::write(base.join(expand_pattern(pattern, n)), b"").map_err(io)?;
            }
        } else {
            return Err(format!("unknown tree entry {entry}"));
        }
        Ok(())
    }

    fn restrict(&mut self, path: &Path, mode: Option<&str>) -> Check {
        use std::os::unix::fs::PermissionsExt;
        let bits = match mode {
            None => return Ok(()),
            Some("readonly") => 0o555,
            Some("unreadable") => 0,
            Some(other) => return Err(format!("unknown mode {other:?}")),
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).map_err(|e| e.to_string())?;
        self.restricted.push(path.to_path_buf());
        Ok(())
    }

    pub fn remove(&self, relative: &str) -> Check {
        let path = self.path(relative);
        let result = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        result.map_err(|e| format!("removing {relative}: {e}"))
    }
}

impl Drop for Sandbox {
    /// Restores permissions so the temporary directory can be deleted.
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        for path in &self.restricted {
            let mode = if path.is_dir() { 0o755 } else { 0o644 };
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
    }
}

/// `file-{n:05}.txt` -> `file-00007.txt`.
fn expand_pattern(pattern: &str, n: u64) -> String {
    let Some(start) = pattern.find("{n") else { return pattern.to_string() };
    let end = pattern[start..].find('}').map_or(pattern.len(), |i| start + i + 1);
    let spec = &pattern[start + 2..end - 1];
    let width = spec.trim_start_matches(':').trim_start_matches('0').parse::<usize>().unwrap_or(0);
    format!("{}{n:0width$}{}", &pattern[..start], &pattern[end..])
}
