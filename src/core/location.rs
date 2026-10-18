use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
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

/// The sole text-entry boundary: absolute local paths or validated URIs.
pub struct LocationInput;
impl LocationInput {
    pub fn parse(input: &str) -> Result<Location, LocationError> {
        if input.is_empty() || input.chars().any(char::is_control) {
            return Err(LocationError::UnsafeLocation);
        }
        if Path::new(input).is_absolute() {
            Ok(Location::from_local_path(Path::new(input)))
        } else {
            Location::from_uri(input)
        }
    }
}

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
        pin_remote_mount(&canonical_uri);
        Ok(Self {
            file: gio::File::for_uri(&canonical_uri),
            canonical_uri,
        })
    }

    /// Creates a location from a GIO file supplied by an explicit adapter boundary.
    ///
    /// This trusted-boundary convenience method retains the required infallible API.
    /// Call [`Self::try_from_file`] for values originating outside the application.
    pub fn from_file(file: gio::File) -> Self {
        Self::try_from_file(file).unwrap_or_else(|_| Self::home())
    }

    /// Validates a GIO file URI before it becomes application identity.
    ///
    /// Adapters receiving untrusted GIO files must use this method rather than
    /// the infallible compatibility constructor, so a credential-bearing URI
    /// cannot panic the process or enter persistent state.
    pub fn try_from_file(file: gio::File) -> Result<Self, LocationError> {
        let canonical_uri = canonicalize_uri(file.uri().as_str())?;
        pin_remote_mount(&canonical_uri);
        Ok(Self {
            canonical_uri,
            file,
        })
    }

    /// Creates a local `file:` location at an explicit filesystem boundary.
    pub fn from_local_path(path: &Path) -> Self {
        Self::from_file(gio::File::for_path(path))
    }
    pub fn uri(&self) -> &str {
        &self.canonical_uri
    }
    pub fn local_path(&self) -> Option<PathBuf> {
        // GVfs may offer a FUSE projection for a remote URI. That projection
        // is not local identity and must not grant Terminal/local-adapter
        // privileges or migrate remote pins into file: locations.
        self.canonical_uri
            .starts_with("file:")
            .then(|| self.file.path())
            .flatten()
    }

    pub fn parent(&self) -> Option<Self> {
        self.file
            .parent()
            .and_then(|file| Self::try_from_file(file).ok())
            // GIO stops at the root of a mount, which inside an archive is the
            // archive itself. Going up from there means going back to the
            // folder the archive is in, the way leaving any other folder does.
            .or_else(|| self.archive_file().and_then(|archive| archive.parent()))
    }

    /// The archive a location at an archive root was opened from.
    fn archive_file(&self) -> Option<Self> {
        let parsed = glib::Uri::parse(&self.canonical_uri, glib::UriFlags::ENCODED).ok()?;
        if !self.is_inside_archive() || !parsed.path().trim_matches('/').is_empty() {
            return None;
        }
        self.archive_source()
    }

    /// The archive file a location inside an archive was opened from, at any
    /// depth. `archive_contents` puts the archive's own URI in the authority,
    /// escaped once to fit and again because parsing unescapes it; this undoes
    /// that so the real file can be addressed.
    pub fn archive_source(&self) -> Option<Self> {
        if !self.is_inside_archive() {
            return None;
        }
        let parsed = glib::Uri::parse(&self.canonical_uri, glib::UriFlags::ENCODED).ok()?;
        let archive = glib::Uri::unescape_string(&parsed.host()?, None)?;
        Self::from_uri(&archive).ok()
    }

    /// The decoded path of this location inside its archive, empty at the root.
    /// This is the name libarchive reports for the entry, not the URI encoding.
    pub fn archive_inner_path(&self) -> Option<String> {
        if !self.is_inside_archive() {
            return None;
        }
        let parsed = glib::Uri::parse(&self.canonical_uri, glib::UriFlags::ENCODED).ok()?;
        let path = glib::Uri::unescape_string(&parsed.path(), None)?;
        Some(path.trim_matches('/').to_owned())
    }

    pub fn is_within(&self, root: &Self) -> bool {
        self == root || self.file.has_prefix(&root.file)
    }

    /// Where this archive's contents live, as gvfs addresses them.
    ///
    /// `gvfsd-archive` names a mount by the archive's own URI, escaped twice:
    /// once to put a URI inside an authority, and again because parsing the
    /// result unescapes it. Any file can be asked for -- whether gvfs can read
    /// it is answered by the mount, not here.
    pub fn archive_contents(&self) -> Result<Self, LocationError> {
        let escaped = glib::Uri::escape_string(&self.canonical_uri, None, false);
        let host = glib::Uri::escape_string(&escaped, None, false);
        Self::from_uri(&format!("archive://{host}/"))
    }

    /// Whether this location is inside an archive gvfs has mounted.
    pub fn is_inside_archive(&self) -> bool {
        self.canonical_uri.starts_with("archive://")
    }

    /// A concise label suitable for a tab, breadcrumb, or sidebar row.
    pub fn display_name(&self) -> String {
        if let Some(name) = self.archive_display_name() {
            return name;
        }
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

    /// The label for a location inside an archive.
    ///
    /// Redaction exists to keep a remote share's path out of the interface, but
    /// an archive's authority is a file the user picked themselves and its
    /// segments are its own contents, so the usual `scheme://host/…` would hide
    /// the only part that means anything. Named after the archive at the root,
    /// and after the current entry below it.
    fn archive_display_name(&self) -> Option<String> {
        if !self.is_inside_archive() {
            return None;
        }
        let parsed = glib::Uri::parse(&self.canonical_uri, glib::UriFlags::ENCODED).ok()?;
        let path = parsed.path();
        let inner = path
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .map(|segment| glib::Uri::unescape_string(segment, None));
        if let Some(Some(segment)) = inner {
            return Some(segment.to_string());
        }
        // At the root the name of the archive is the name of the location.
        let host = parsed.host()?;
        let archive = glib::Uri::unescape_string(&host, None)?;
        Self::from_uri(&archive).ok().map(|archive| {
            let name = archive.display_name();
            if name.is_empty() {
                archive.redacted()
            } else {
                name
            }
        })
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

/// Retains one `gio::File` per non-local mount for the process lifetime.
///
/// gvfs (1.54.4) frees a `GMountSpec` once its refcount reaches zero, but a
/// concurrent lookup can still resurrect that spec from the library's global
/// `unique_hash` first. Constructing a non-`file:` `GFile` while another thread
/// finalizes one for the same mount is therefore a use-after-free, which shows
/// up as `SIGSEGV` or heap corruption inside `g_mount_spec_unref`. Serializing
/// only construction does not help: both ends of the race have to be excluded.
///
/// Holding one file per mount keeps that refcount above zero, so no spec is
/// ever finalized and nothing is left for a construction to race. Every path
/// under a mount shares its spec, so one retained file covers the whole mount.
/// The cost is one small object per distinct non-local mount reached in a
/// session. Remove this once the host gvfs carries the upstream fix.
fn pin_remote_mount(canonical_uri: &str) {
    let Some(mount) = remote_mount_key(canonical_uri) else {
        return;
    };
    let mut pinned = PINNED_REMOTE_MOUNTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pinned
        .entry(mount)
        .or_insert_with(|| gio::File::for_uri(canonical_uri));
}

static PINNED_REMOTE_MOUNTS: OnceLock<Mutex<HashMap<String, gio::File>>> = OnceLock::new();

/// The mount a canonical URI belongs to, or `None` for local paths.
///
/// A gvfs mount spec never distinguishes paths below its mount point, so
/// scheme, host and the first path segment are never finer than the real spec.
/// Being coarser would under-pin; being finer only pins a few extra files.
fn remote_mount_key(canonical_uri: &str) -> Option<String> {
    if canonical_uri.starts_with("file:") {
        return None;
    }
    let parsed = glib::Uri::parse(canonical_uri, glib::UriFlags::ENCODED).ok()?;
    let scheme = parsed.scheme();
    let host = parsed.host().unwrap_or_default();
    let mount = parsed
        .path()
        .split('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or_default()
        .to_owned();
    Some(format!("{scheme}://{host}/{mount}"))
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
    let path = normalize_percent_encoding(parsed.path().as_str())?;
    // An authority that carries percent-encoding is identity, not a host name:
    // gvfs addresses an archive as `archive://<the archive's escaped URI>/`.
    // Lowercasing that loses the case of the archive's own path, and rebuilding
    // it through `Uri::join` unescapes one layer, so either one addresses a
    // mount that does not exist. Hostnames are still folded, because DNS does
    // not distinguish case and the same share must not become two locations.
    if let Some(authority) = opaque_authority(input) {
        let authority = normalize_percent_encoding(authority)?;
        return Ok(format!("{scheme}://{authority}{path}"));
    }
    let host = parsed.host().map(|host| host.to_ascii_lowercase());
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

/// The authority of a URI that names something other than a host.
///
/// Only percent-encoding marks one, since a real authority has none: a host
/// name cannot contain `%`, and userinfo and ports are rejected before this is
/// reached. Returning `None` leaves the ordinary host path untouched.
fn opaque_authority(input: &str) -> Option<&str> {
    let start = input.find("://")? + 3;
    let rest = &input[start..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let opaque = authority.contains('%')
        && !authority.contains(':')
        && !authority.contains('@')
        && !authority.contains('[');
    opaque.then_some(authority)
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
