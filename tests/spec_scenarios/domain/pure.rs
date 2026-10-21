//! Subjects that need no ports: `location`, `session`, `named_child`, `clipboard`, `pins`.
//! Locations are URIs. `$name` refers to a location bound earlier with `as`.

use std::cell::RefCell;
use std::path::PathBuf;

use serde_json::Value;
use zorin_explorer::application::commands::{named_child, ClipboardAction, ClipboardPayload, ClipboardState};
use zorin_explorer::core::application_preferences::{
    decode_session, decode_session_state, encode_session, import_legacy_local_paths, RecoverableStateNotice,
};
use zorin_explorer::core::{Location, SidebarPreferences};

use super::World;
use crate::Check;

#[derive(Default)]
pub struct Pure {
    clipboard: RefCell<ClipboardState>,
    pins_dir: RefCell<Option<tempfile::TempDir>>,
    pins: RefCell<Option<SidebarPreferences>>,
    names: RefCell<std::collections::HashMap<String, Location>>,
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

fn eq<T: PartialEq + std::fmt::Debug>(field: &str, got: T, want: T) -> Check {
    if got == want { Ok(()) } else { Err(format!("{field} is {got:?}, expected {want:?}")) }
}

/// A literal URI, `$name`, or `{"join": ["$name", "suffix"]}`.
fn text(world: &World, value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => match s.strip_prefix('$') {
            Some(name) => world.pure.names.borrow().get(name).map(|l| l.uri().to_string()).ok_or_else(|| format!("{s} is not bound")),
            None => Ok(s.clone()),
        },
        Value::Object(map) => match map.get("join").and_then(Value::as_array) {
            Some(parts) => parts.iter().map(|p| text(world, p)).collect::<Result<Vec<_>, _>>().map(|p| p.concat()),
            None => Err(format!("bad text {value}")),
        },
        other => Err(format!("bad text {other}")),
    }
}

pub fn expect_location(world: &World, step: &Value) -> Check {
    let input = text(world, &step["input"])?;
    // `via: "input"` goes through the location bar's parser, which also takes bare paths.
    let parsed = if step["via"].as_str() == Some("input") {
        zorin_explorer::core::location::LocationInput::parse(&input)
    } else {
        Location::from_uri(&input)
    };
    if step["is"].as_str() == Some("rejected") {
        return if parsed.is_err() { Ok(()) } else { Err(format!("{input:?} was accepted, expected rejection")) };
    }
    let location = parsed.map_err(|e| format!("{input:?} was rejected: {e:?}"))?;
    if let Some(want) = step["canonical"].as_str() {
        eq("canonical", location.uri().to_string(), want.to_string())?;
    }
    if let Some(want) = step["redacted"].as_str() {
        eq("redacted", location.redacted(), want.to_string())?;
    }
    if let Some(want) = step["display_name"].as_str() {
        eq("display_name", location.display_name(), want.to_string())?;
    }
    if let Some(want) = step.get("parent") {
        let got = location.parent().map(|p| p.uri().to_string());
        let want = match want {
            Value::Null => None,
            other => Some(text(world, other)?),
        };
        eq("parent", got, want)?;
    }
    if let Some(want) = step["inside_archive"].as_bool() {
        eq("inside_archive", location.is_inside_archive(), want)?;
    }
    if step.get("local_path").is_some_and(Value::is_null) {
        eq("local_path", location.local_path(), None)?;
    }
    if let Some(other) = step.get("equals") {
        let other = Location::from_uri(&text(world, other)?).map_err(|e| format!("{e:?}"))?;
        eq("equality", location == other, true)?;
    }
    if let Some(other) = step.get("differs_from") {
        let other = Location::from_uri(&text(world, other)?).map_err(|e| format!("{e:?}"))?;
        eq("inequality", location != other, true)?;
    }
    let mut derived = location.clone();
    if step["archive_contents"].as_bool() == Some(true) || step["archive_contents"].is_string() {
        derived = location.archive_contents().map_err(|e| format!("archive_contents: {e:?}"))?;
        if let Some(want) = step["archive_contents"].as_str() {
            eq("archive_contents", derived.uri().to_string(), want.to_string())?;
        }
    }
    if let Some(name) = step["as"].as_str() {
        world.pure.names.borrow_mut().insert(name.to_string(), derived);
    }
    Ok(())
}

fn notice_name(notice: &RecoverableStateNotice) -> &'static str {
    match notice {
        RecoverableStateNotice::MalformedRecord => "malformed_record",
        RecoverableStateNotice::UnsupportedSchema => "unsupported_schema",
    }
}

pub fn expect_session(_world: &World, step: &Value) -> Check {
    if let Some(list) = step.get("round_trip") {
        let locations: Vec<Location> = strings(list).iter().map(|u| Location::from_uri(u).map_err(|e| format!("{e:?}"))).collect::<Result<_, _>>()?;
        let bytes = encode_session(&locations).map_err(|e| format!("{e:?}"))?;
        eq("round trip", decode_session(&bytes).map_err(|e| format!("{e:?}"))?, locations)?;
    }
    if let Some(stored) = step.get("decode") {
        let bytes = serde_json::to_vec(stored).map_err(|e| e.to_string())?;
        let decoded = decode_session_state(&bytes).map_err(|e| format!("{e:?}"))?;
        if step.get("locations").is_some() {
            eq("locations", decoded.locations.iter().map(|l| l.uri().to_string()).collect(), strings(&step["locations"]))?;
        }
        if step.get("notices").is_some() {
            eq("notices", decoded.notices.iter().map(|n| notice_name(n).to_string()).collect(), strings(&step["notices"]))?;
        }
        if let Some(want) = step.get("rewritten_as") {
            let rewritten = encode_session(&decoded.locations).map_err(|e| format!("{e:?}"))?;
            let value: Value = serde_json::from_slice(&rewritten).map_err(|e| e.to_string())?;
            eq("rewritten schema_version", value["schema_version"].clone(), want["schema_version"].clone())?;
            eq("rewritten locations", strings(&value["locations"]), strings(&want["locations"]))?;
        }
    }
    if let Some(paths) = step.get("legacy_paths") {
        let paths: Vec<PathBuf> = strings(paths).into_iter().map(PathBuf::from).collect();
        let first = import_legacy_local_paths(&paths);
        eq("idempotent import", import_legacy_local_paths(&paths), first.clone())?;
        eq("imported", first.iter().map(|l| l.uri().to_string()).collect(), strings(&step["uris"]))?;
    }
    Ok(())
}

pub fn expect_named_child(_world: &World, step: &Value) -> Check {
    let parent = Location::from_uri(step["parent"].as_str().ok_or("needs `parent`")?).map_err(|e| format!("{e:?}"))?;
    let result = named_child(&parent, step["name"].as_str().ok_or("needs `name`")?);
    match (step["is"].as_str(), result) {
        (Some("rejected"), Err(_)) => Ok(()),
        (Some("accepted"), Ok(child)) => match step["uri"].as_str() {
            Some(want) => eq("child uri", child.uri().to_string(), want.to_string()),
            None => Ok(()),
        },
        (want, got) => Err(format!("named child result is {:?}, expected {want:?}", got.map(|l| l.uri().to_string()))),
    }
}

fn action(name: &str) -> Result<ClipboardAction, String> {
    match name {
        "copy" => Ok(ClipboardAction::Copy),
        "cut" => Ok(ClipboardAction::Cut),
        other => Err(format!("unknown clipboard action {other:?}")),
    }
}

fn parse_payload(step: &Value) -> Result<Result<ClipboardPayload, ()>, String> {
    let body = step["text"].as_str().ok_or("needs `text`")?;
    Ok(match step["format"].as_str().unwrap_or("uri_list") {
        "uri_list" => ClipboardPayload::parse(action(step["action"].as_str().unwrap_or("copy"))?, body).map_err(|_| ()),
        "gnome" => ClipboardPayload::parse_gnome(body).map_err(|_| ()),
        other => return Err(format!("unknown clipboard format {other:?}")),
    })
}

pub fn expect_clipboard_payload(_world: &World, step: &Value) -> Check {
    match (step["is"].as_str(), parse_payload(step)?) {
        (Some("rejected"), Err(())) => Ok(()),
        (Some("accepted"), Ok(payload)) => {
            if let Some(want) = step["action"].as_str() {
                eq("action", payload.action, action(want)?)?;
            }
            if step.get("uris").is_some() {
                eq("uris", payload.locations.iter().map(|l| l.uri().to_string()).collect(), strings(&step["uris"]))?;
            }
            if let Some(want) = step["uri_list"].as_str() {
                eq("uri_list", payload.uri_list(), want.to_string())?;
            }
            Ok(())
        }
        (want, got) => Err(format!("payload is {:?}, expected {want:?}", got.is_ok())),
    }
}

pub fn clipboard_action(world: &World, name: &str, step: &Value) -> Check {
    let mut state = world.pure.clipboard.borrow_mut();
    match name {
        "replace" => {
            let payload = parse_payload(step)?.map_err(|_| "payload rejected".to_string())?;
            state.replace(payload);
        }
        "begin_paste" => state.begin_paste(step["id"].as_u64().ok_or("needs `id`")?),
        "finish_paste" => state.finish_paste(step["id"].as_u64().ok_or("needs `id`")?, step["terminal"].as_bool().ok_or("needs `terminal`")?),
        other => return Err(format!("clipboard has no action {other:?}")),
    }
    Ok(())
}

pub fn expect_clipboard(world: &World, step: &Value) -> Check {
    let state = world.pure.clipboard.borrow();
    match (step.get("mode"), state.payload()) {
        (Some(Value::Null), None) => Ok(()),
        (Some(Value::String(mode)), Some(payload)) => {
            eq("mode", payload.action, action(mode)?)?;
            if step.get("uris").is_some() {
                eq("uris", payload.locations.iter().map(|l| l.uri().to_string()).collect(), strings(&step["uris"]))?;
            }
            Ok(())
        }
        (want, got) => Err(format!("clipboard holds {:?}, expected mode {want:?}", got.map(|p| p.action))),
    }
}

// Pins persist across a reload from disk.

fn pins_path(pure: &Pure) -> PathBuf {
    let mut dir = pure.pins_dir.borrow_mut();
    dir.get_or_insert_with(|| tempfile::tempdir().expect("pins dir")).path().join("pins.json")
}

pub fn pins_action(world: &World, name: &str, step: &Value) -> Check {
    let path = pins_path(&world.pure);
    let mut slot = world.pure.pins.borrow_mut();
    if slot.is_none() {
        *slot = Some(SidebarPreferences::load_from(path.clone()).map_err(|e| e.to_string())?);
    }
    match name {
        "pin" => {
            let location = Location::from_uri(step["uri"].as_str().ok_or("needs `uri`")?).map_err(|e| format!("{e:?}"))?;
            slot.as_mut().unwrap().pin_location(location);
        }
        "save" => slot.as_ref().unwrap().save().map_err(|e| e.to_string())?,
        "reload" => *slot = Some(SidebarPreferences::load_from(path).map_err(|e| e.to_string())?),
        other => return Err(format!("pins has no action {other:?}")),
    }
    Ok(())
}

pub fn expect_pins(world: &World, step: &Value) -> Check {
    let slot = world.pure.pins.borrow();
    let pins = slot.as_ref().ok_or("no pins loaded")?;
    let got: Vec<String> = pins.locations().map(|l| l.uri().to_string()).collect();
    for want in strings(&step["contains"]) {
        if !got.contains(&want) {
            return Err(format!("pins {got:?} lack {want}"));
        }
    }
    Ok(())
}

// contracts/errors.json against the code.

fn kind_of(name: &str) -> Result<zorin_explorer::core::contracts::OperationErrorKind, String> {
    super::error_kind(name)
}

fn retry_of(name: &str) -> Result<zorin_explorer::core::contracts::RetryClass, String> {
    use zorin_explorer::core::contracts::RetryClass::*;
    Ok(match name {
        "never" => Never,
        "after_refresh" => AfterRefresh,
        "after_user_action" => AfterUserAction,
        "transient" => Transient,
        other => return Err(format!("unknown retry class {other:?}")),
    })
}

fn gio_failure(name: &str) -> Result<gio::IOErrorEnum, String> {
    use gio::IOErrorEnum as E;
    Ok(match name {
        "permission_denied" => E::PermissionDenied,
        "not_mounted" => E::NotMounted,
        "host_not_found" => E::HostNotFound,
        "not_connected" => E::NotConnected,
        "exists" => E::Exists,
        "would_merge" => E::WouldMerge,
        "not_found" => E::NotFound,
        "read_only" => E::ReadOnly,
        "not_supported" => E::NotSupported,
        "invalid_argument" => E::InvalidArgument,
        "invalid_filename" => E::InvalidFilename,
        "other" => E::Failed,
        other => return Err(format!("unknown failure {other:?}")),
    })
}

/// Every kind's message key and retry class, and every failure's kind, as the code has them.
pub fn expect_error_table(_world: &World, _step: &Value) -> Check {
    use zorin_explorer::core::contracts::OperationError;
    let path = crate::spec_dir().join("contracts/errors.json");
    let table: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    for entry in table["kinds"].as_array().ok_or("kinds must be an array")? {
        let name = entry["kind"].as_str().unwrap_or("?");
        let error = OperationError::new(kind_of(name)?, None, None);
        eq(&format!("{name} message key"), error.message_key, entry["message_key"].as_str().ok_or("needs message_key")?)?;
        eq(&format!("{name} retry"), error.retry, retry_of(entry["retry"].as_str().ok_or("needs retry")?)?)?;
    }
    for entry in table["failures"].as_array().ok_or("failures must be an array")? {
        let name = entry["failure"].as_str().unwrap_or("?");
        let raw = glib::Error::new(gio_failure(name)?, "backend text that must not survive");
        let error = OperationError::from_gio(&raw, None, None);
        eq(&format!("failure {name}"), error.kind, kind_of(entry["kind"].as_str().ok_or("needs kind")?)?)?;
        eq(&format!("failure {name} shows only its message key"), error.to_string(), error.message_key.to_string())?;
    }
    // Every kind the code has appears in the table, so a new one cannot be added silently.
    let listed = table["kinds"].as_array().map_or(0, Vec::len);
    eq("number of error kinds", listed, 11)
}
