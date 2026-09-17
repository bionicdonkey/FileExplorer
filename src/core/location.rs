use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::{env, fmt};

use gio::prelude::*;

/// A validated, canonical URI and its GIO handle.
#[derive(Clone, Debug)]
pub struct Location {
    canonical_uri: String,
    file: gio::File,
}

/// URI input was not safe to retain as application state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocationError {
    UnsafeLocation,
}

impl fmt::Display for LocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unsafe location")
    }
}
impl std::error::Error for LocationError {}

/// A requested URI together with the URI ultimately reached by a provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLocation {
    pub requested: Location,
    pub resolved: Location,
}

impl PartialEq for Location {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_uri == other.canonical_uri
    }
}
impl Eq for Location {}
impl Hash for Location {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.canonical_uri.hash(state);
    }
}

impl Location {
    /// Returns the current user's home directory at the explicit local boundary.
    pub fn home() -> Self {
        let path = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self::from_local_path(&path)
    }

    /// Creates a location from validated URI input.
    pub fn from_uri(uri: &str) -> Result<Self, LocationError> {
        let canonical_uri = canonicalize_uri(uri)?;
        Ok(Self {
            file: gio::File::for_uri(&canonical_uri),
            canonical_uri,
        })
    }

    /// Creates a location from a GIO file supplied by an explicit adapter boundary.
    pub fn from_file(file: gio::File) -> Self {
        let canonical_uri = canonicalize_uri(file.uri().as_str())
            .expect("GIO file URI must be safe before becoming application identity");
        Self {
            canonical_uri,
            file,
        }
    }

    /// Creates a local `file:` location at an explicit filesystem boundary.
    pub fn from_local_path(path: &Path) -> Self {
        Self::from_file(gio::File::for_path(path))
    }
    pub fn uri(&self) -> &str {
        &self.canonical_uri
    }
    pub fn local_path(&self) -> Option<PathBuf> {
        self.file.path()
    }

    /// A concise label suitable for a tab, breadcrumb, or sidebar row.
    pub fn display_name(&self) -> String {
        self.local_path()
            .and_then(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned)
                    .or_else(|| Some(path.to_string_lossy().into_owned()))
            })
            .unwrap_or_else(|| self.redacted())
    }

    /// A user-safe URI description that never includes remote path details.
    pub fn redacted(&self) -> String {
        let parsed = glib::Uri::parse(&self.canonical_uri, glib::UriFlags::ENCODED)
            .expect("canonical URI remains valid");
        match parsed.host() {
            Some(host) => format!("{}://{}/…", parsed.scheme(), host),
            None => format!("{}:…", parsed.scheme()),
        }
    }
}

fn canonicalize_uri(input: &str) -> Result<String, LocationError> {
    if input.is_empty() || input.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(LocationError::UnsafeLocation);
    }
    validate_percent_escapes(input)?;
    let parsed = glib::Uri::parse(input, glib::UriFlags::ENCODED)
        .map_err(|_| LocationError::UnsafeLocation)?;
    if parsed.scheme().is_empty()
        || parsed.userinfo().is_some()
        || parsed.password().is_some()
        || parsed.auth_params().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(LocationError::UnsafeLocation);
    }
    let scheme = parsed.scheme().to_ascii_lowercase();
    let host = parsed.host().map(|host| host.to_ascii_lowercase());
    let path = normalize_percent_encoding(parsed.path().as_str())?;
    Ok(glib::Uri::join(
        glib::UriFlags::ENCODED,
        Some(&scheme),
        None,
        host.as_deref(),
        normalized_port(&scheme, parsed.port()),
        &path,
        None,
        None,
    )
    .to_string())
}

fn validate_percent_escapes(value: &str) -> Result<(), LocationError> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(LocationError::UnsafeLocation);
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn normalize_percent_encoding(path: &str) -> Result<String, LocationError> {
    let bytes = path.as_bytes();
    let mut normalized = String::with_capacity(path.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            let next_percent = bytes[index..]
                .iter()
                .position(|byte| *byte == b'%')
                .map(|offset| index + offset)
                .unwrap_or(bytes.len());
            normalized.push_str(&path[index..next_percent]);
            index = next_percent;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_hexdigit()
            || !bytes[index + 2].is_ascii_hexdigit()
        {
            return Err(LocationError::UnsafeLocation);
        }
        let byte = hex_byte(bytes[index + 1], bytes[index + 2]);
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            normalized.push(byte as char);
        } else {
            normalized.push('%');
            normalized.push((bytes[index + 1] as char).to_ascii_uppercase());
            normalized.push((bytes[index + 2] as char).to_ascii_uppercase());
        }
        index += 3;
    }
    Ok(normalized)
}

fn hex_byte(high: u8, low: u8) -> u8 {
    fn value(character: u8) -> u8 {
        match character {
            b'0'..=b'9' => character - b'0',
            b'a'..=b'f' => character - b'a' + 10,
            b'A'..=b'F' => character - b'A' + 10,
            _ => unreachable!("validated hexadecimal input"),
        }
    }
    value(high) * 16 + value(low)
}

fn normalized_port(scheme: &str, port: i32) -> i32 {
    match (scheme, port) {
        ("http", 80) | ("https", 443) | ("ftp", 21) | ("sftp", 22) | ("ssh", 22) | ("smb", 445) => {
            -1
        }
        _ => port,
    }
}
