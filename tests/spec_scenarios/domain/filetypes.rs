//! The `file_types` subject: what opening an entry does (`core::activation`), and a check
//! that `contracts/file-types.json` agrees with the code.

use serde_json::Value;
use zorin_explorer::core::activation::{activation_for, is_extractable, Activation};
use zorin_explorer::core::contracts::*;
use zorin_explorer::core::{FileItem, Location};

use super::World;
use crate::{spec_dir, Check};

fn entry(step: &Value, name: &str, uri: Option<&str>, content_type: Option<&str>) -> Result<FileItem, String> {
    let kind = match step["kind"].as_str().unwrap_or("file") {
        "file" => EntryKind::File,
        "directory" => EntryKind::Directory,
        other => return Err(format!("unknown entry kind {other:?}")),
    };
    let uri = uri.map(str::to_string).unwrap_or_else(|| format!("file:///home/user/{name}"));
    Ok(FileItem::from_snapshot(&EntrySnapshot {
        location: Location::from_uri(&uri).map_err(|e| format!("bad uri {uri:?}: {e:?}"))?,
        display_name: name.to_owned(),
        kind,
        size: None,
        modified: None,
        content_type: content_type.map(str::to_owned),
        capabilities: CapabilitySnapshot::default(),
    }))
}

fn eq<T: PartialEq + std::fmt::Debug>(field: &str, got: T, want: T) -> Check {
    if got == want { Ok(()) } else { Err(format!("{field} is {got:?}, expected {want:?}")) }
}

/// `{"expect":"activation", "name", "uri"?, "kind"?, "content_type"?, "is", "extractable"?}`.
pub fn expect_activation(_world: &World, step: &Value) -> Check {
    let name = step["name"].as_str().ok_or("needs `name`")?;
    let item = entry(step, name, step["uri"].as_str(), step["content_type"].as_str())?;
    let activation = activation_for(&item);
    let got = match &activation {
        Activation::Browse(_) => "browse",
        Activation::LaunchExternally => "launch_externally",
    };
    eq("activation", got, step["is"].as_str().ok_or("needs `is`")?)?;
    if let Some(want) = step["browses_archive"].as_bool() {
        let inside = matches!(&activation, Activation::Browse(l) if l.is_inside_archive());
        eq("browses an archive", inside, want)?;
        if let (true, Activation::Browse(l)) = (want, &activation) {
            eq("archive display name", l.display_name(), name.to_string())?;
        }
    }
    if let Some(want) = step["extractable"].as_bool() {
        eq("extractable", is_extractable(&item), want)?;
    }
    Ok(())
}

/// Every type and extension in the table must behave as its rule says. A local file is used
/// for `browsable: true` and `local_only`; a remote one checks that `local_only` leaves.
pub fn expect_table(_world: &World, _step: &Value) -> Check {
    let path = spec_dir().join("contracts/file-types.json");
    let table: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut checked = 0;
    for rule in table["rules"].as_array().ok_or("rules must be an array")? {
        let id = rule["id"].as_str().unwrap_or("?");
        let mut subjects: Vec<(String, Option<String>)> = Vec::new();
        for mime in rule["match"]["mime"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            subjects.push(("sample.bin".to_string(), Some(mime.to_string())));
        }
        for ext in rule["match"]["extensions"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            subjects.push((format!("sample.{ext}"), None));
        }
        for (name, content_type) in subjects {
            let local = entry(&Value::Null, &name, None, content_type.as_deref())?;
            let remote = entry(&Value::Null, &name, Some(&format!("smb://server/{name}")), content_type.as_deref())?;
            let (want_local, want_remote) = match &rule["browsable"] {
                Value::Bool(true) => (true, true),
                Value::Bool(false) => (false, false),
                Value::String(s) if s == "local_only" => (true, false),
                other => return Err(format!("{id}: bad browsable {other}")),
            };
            let browses = |item: &FileItem| matches!(activation_for(item), Activation::Browse(_));
            eq(&format!("{id}: {name} {content_type:?} local browse"), browses(&local), want_local)?;
            eq(&format!("{id}: {name} {content_type:?} remote browse"), browses(&remote), want_remote)?;
            if let Some(extractable) = rule["extractable"].as_bool() {
                eq(&format!("{id}: {name} {content_type:?} extractable"), is_extractable(&local), extractable)?;
            }
            checked += 1;
        }
    }
    if checked == 0 {
        return Err("the file-types table has no mime types or extensions to check".into());
    }
    Ok(())
}
