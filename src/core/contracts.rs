//! Owned URI domain values exchanged across application/repository boundaries.
//! Snapshots contain no live enumerators, widgets, local path identities, or raw
//! backend error messages. Request IDs are allocated by the application and must
//! be unique within its lifetime; operation IDs live in a separate namespace.

use std::collections::BTreeSet;
use std::fmt;
use std::time::{Duration, SystemTime};

use glib::translate::ToGlibPtr;

use super::{Location, ResolvedLocation};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(pub u64);

impl RequestId {
    /// Shared namespace for directory, capability and search requests.
    pub(crate) fn allocate() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("request IDs exhausted"),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    List,
    Copy,
    Move,
    Create,
    Rename,
    Trash,
    Delete,
    Open,
    SearchFilename,
    SearchContent,
}

/// Absence from `supported` means unavailable, including unknown capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilitySnapshot {
    pub version: u64,
    pub supported: BTreeSet<Capability>,
    pub stable_order: bool,
    pub containment: Containment,
}

impl CapabilitySnapshot {
    pub fn supports(&self, capability: Capability) -> bool {
        self.supported.contains(&capability)
    }
}

impl Default for CapabilitySnapshot {
    fn default() -> Self {
        Self {
            version: 0,
            supported: BTreeSet::new(),
            stable_order: false,
            containment: Containment::Opaque,
        }
    }
}

/// Relations must be supplied by a trusted provider, never guessed from paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Containment {
    Opaque,
    KnownAncestors(Vec<Location>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationKind {
    Copy,
    Move,
    Create,
    Rename,
    Trash,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictDecision {
    Replace,
    KeepBoth,
    Skip,
    CancelRemaining,
}

impl MutationKind {
    pub fn capability(self) -> Capability {
        match self {
            Self::Copy => Capability::Copy,
            Self::Move => Capability::Move,
            Self::Create => Capability::Create,
            Self::Rename => Capability::Rename,
            Self::Trash => Capability::Trash,
            Self::Delete => Capability::Delete,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OperationRequest {
    pub id: OperationId,
    pub kind: MutationKind,
    pub sources: Vec<Location>,
    pub destination: Option<Location>,
    pub explicit_target: Option<Location>,
    pub capability_version: u64,
    pub cancellable: gio::Cancellable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationErrorKind {
    PermissionDenied,
    AuthenticationRequired,
    MountUnavailable,
    Conflict,
    NotFound,
    ReadOnly,
    Unsupported,
    CapabilityChanged,
    PartialCompletion,
    InvalidRequest,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryClass {
    Never,
    AfterRefresh,
    AfterUserAction,
    Transient,
}

/// Error construction belongs to adapters. No transport message is retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationError {
    pub kind: OperationErrorKind,
    pub gio_domain: Option<glib::Quark>,
    pub gio_code: Option<i32>,
    pub message_key: &'static str,
    pub source: Option<String>,
    pub destination: Option<String>,
    pub retry: RetryClass,
}

impl OperationError {
    pub fn new(
        kind: OperationErrorKind,
        source: Option<&Location>,
        destination: Option<&Location>,
    ) -> Self {
        use OperationErrorKind::*;
        let (message_key, retry) = match kind {
            PermissionDenied => ("operation.permission-denied", RetryClass::AfterUserAction),
            AuthenticationRequired => (
                "operation.authentication-required",
                RetryClass::AfterUserAction,
            ),
            MountUnavailable => ("operation.mount-unavailable", RetryClass::AfterUserAction),
            Conflict => ("operation.conflict", RetryClass::AfterUserAction),
            NotFound => ("operation.not-found", RetryClass::AfterRefresh),
            ReadOnly => ("operation.read-only", RetryClass::AfterUserAction),
            Unsupported => ("operation.unsupported", RetryClass::Never),
            CapabilityChanged => ("operation.capability-changed", RetryClass::AfterRefresh),
            PartialCompletion => ("operation.partial-completion", RetryClass::AfterRefresh),
            InvalidRequest => ("operation.invalid-request", RetryClass::Never),
            Unknown => ("operation.failed", RetryClass::AfterRefresh),
        };
        Self {
            kind,
            gio_domain: None,
            gio_code: None,
            message_key,
            source: source.map(Location::redacted),
            destination: destination.map(Location::redacted),
            retry,
        }
    }

    /// GIO cancellation is handled as a terminal cancellation by the adapter,
    /// not routed through this error constructor. Provider authentication errors
    /// that have no distinct GIO code should use `new(AuthenticationRequired, ..)`.
    pub fn from_gio(
        error: &glib::Error,
        source: Option<&Location>,
        destination: Option<&Location>,
    ) -> Self {
        let kind = match error.kind::<gio::IOErrorEnum>() {
            Some(gio::IOErrorEnum::PermissionDenied) => OperationErrorKind::PermissionDenied,
            Some(
                gio::IOErrorEnum::NotMounted
                | gio::IOErrorEnum::HostNotFound
                | gio::IOErrorEnum::NotConnected,
            ) => OperationErrorKind::MountUnavailable,
            Some(gio::IOErrorEnum::Exists | gio::IOErrorEnum::WouldMerge) => {
                OperationErrorKind::Conflict
            }
            Some(gio::IOErrorEnum::NotFound) => OperationErrorKind::NotFound,
            Some(gio::IOErrorEnum::ReadOnly) => OperationErrorKind::ReadOnly,
            Some(gio::IOErrorEnum::NotSupported) => OperationErrorKind::Unsupported,
            Some(gio::IOErrorEnum::InvalidArgument | gio::IOErrorEnum::InvalidFilename) => {
                OperationErrorKind::InvalidRequest
            }
            _ => OperationErrorKind::Unknown,
        };
        let mut result = Self::new(kind, source, destination);
        result.gio_domain = Some(error.domain());
        // glib 0.19 exposes the domain but no raw code accessor. GError is
        // immutable here and remains alive throughout this borrowed read.
        let raw: *const glib::ffi::GError = error.to_glib_none().0;
        result.gio_code = Some(unsafe { (*raw).code });
        result
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message_key)
    }
}
impl std::error::Error for OperationError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MutationEffect {
    #[default]
    Unchanged,
    Changed,
    PartiallyChanged,
    Unknown,
}

/// No rollback or atomicity is implied. `remaining` includes failed/unattempted
/// sources; an adapter with uncertain side effects must report `Unknown`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperationCompletion {
    pub effect: MutationEffect,
    pub completed: Vec<Location>,
    pub remaining: Vec<Location>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancellationOutcome {
    pub completion: OperationCompletion,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperationProgress {
    pub completed_items: u64,
    pub total_items: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationEvent {
    Started {
        id: OperationId,
        sequence: u64,
    },
    Progress {
        id: OperationId,
        sequence: u64,
        progress: OperationProgress,
    },
    CancelRequested {
        id: OperationId,
        sequence: u64,
    },
    Succeeded {
        id: OperationId,
        sequence: u64,
        completion: OperationCompletion,
    },
    Failed {
        id: OperationId,
        sequence: u64,
        error: OperationError,
        completion: OperationCompletion,
    },
    Cancelled {
        id: OperationId,
        sequence: u64,
        outcome: CancellationOutcome,
    },
}

impl OperationEvent {
    pub fn id(&self) -> OperationId {
        match self {
            Self::Started { id, .. }
            | Self::Progress { id, .. }
            | Self::CancelRequested { id, .. }
            | Self::Succeeded { id, .. }
            | Self::Failed { id, .. }
            | Self::Cancelled { id, .. } => *id,
        }
    }
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Started { sequence, .. }
            | Self::Progress { sequence, .. }
            | Self::CancelRequested { sequence, .. }
            | Self::Succeeded { sequence, .. }
            | Self::Failed { sequence, .. }
            | Self::Cancelled { sequence, .. } => *sequence,
        }
    }
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// Append-only acceptance gate. The event owner allocates sequences (including
/// CancelRequested); adapters must not allocate a competing sequence namespace.
/// Cancellation intent alone does not release resources or imply completion.
#[derive(Clone, Debug)]
pub struct OperationReducer {
    id: OperationId,
    events: Vec<OperationEvent>,
}

impl OperationReducer {
    pub fn new(id: OperationId) -> Self {
        Self {
            id,
            events: Vec::new(),
        }
    }
    pub fn apply(&mut self, event: OperationEvent) -> bool {
        if event.id() != self.id
            || self.is_terminal()
            || self
                .events
                .last()
                .is_some_and(|last| event.sequence() <= last.sequence())
        {
            return false;
        }
        self.events.push(event);
        true
    }
    pub fn events(&self) -> &[OperationEvent] {
        &self.events
    }
    pub fn is_terminal(&self) -> bool {
        self.events.last().is_some_and(OperationEvent::is_terminal)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Shortcut,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntrySnapshot {
    pub location: Location,
    pub display_name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub content_type: Option<String>,
    pub capabilities: CapabilitySnapshot,
}

/// Advisory attributes loaded after a row enters GTK's visible viewport.
/// Identity and core actions remain available while this optional data loads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisibleMetadata {
    pub location: Location,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub content_type: Option<String>,
}

/// Advisory row metadata is still an asynchronous provider request: it owns
/// cancellation and carries an identity so a recycled row cannot accept an old
/// response.
#[derive(Clone, Debug)]
pub struct VisibleMetadataRequest {
    pub id: RequestId,
    pub locations: Vec<Location>,
    pub cancellable: gio::Cancellable,
}

/// Offset paging requires stable provider order. Each page has a fresh request
/// ID and exactly one completion; the application merges pages by canonical URI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageRequest {
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct DirectoryRequest {
    pub id: RequestId,
    pub location: Location,
    pub page: PageRequest,
    pub cancellable: gio::Cancellable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectorySnapshot {
    pub location: ResolvedLocation,
    pub entries: Vec<EntrySnapshot>,
    pub capabilities: CapabilitySnapshot,
    pub next_page: Option<PageRequest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestOutcome<T> {
    Succeeded(T),
    Failed(OperationError),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestCompletion<T> {
    pub id: RequestId,
    pub outcome: RequestOutcome<T>,
}

#[derive(Clone, Debug)]
pub struct CapabilityRequest {
    pub id: RequestId,
    pub location: Location,
    pub cancellable: gio::Cancellable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchScope {
    pub root: Location,
    /// Root is depth 0; a value of 1 includes direct children.
    pub max_depth: u32,
    pub max_results: usize,
    pub timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMode {
    Filename,
    Content,
}

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub id: RequestId,
    pub scope: SearchScope,
    pub query: String,
    pub mode: SearchMode,
    pub cancellable: gio::Cancellable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchLimit {
    Depth,
    ResultCount,
    Time,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchIncompleteReason {
    Cancelled,
    ProviderFailure,
    MalformedPage,
    AccessLost,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchOutcome {
    Completed {
        matches: Vec<EntrySnapshot>,
        declared_scope: SearchScope,
        scope_limited: bool,
        limits: Vec<SearchLimit>,
    },
    Incomplete {
        matches: Vec<EntrySnapshot>,
        reason: SearchIncompleteReason,
        declared_scope: SearchScope,
        retry: RetryClass,
        error: Option<OperationError>,
    },
    Unsupported {
        declared_scope: SearchScope,
        error: OperationError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchCompletion {
    pub id: RequestId,
    pub outcome: SearchOutcome,
}
