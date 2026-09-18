//! Object-safe asynchronous boundaries. Call on the adapter's owning GLib main
//! context. Implementations must return promptly and dispatch every callback on
//! that context, never inline or on an I/O worker. Callbacks deliberately lack
//! `Send`, allowing application state to remain confined to the main context.
//!
//! Retain and eventually consume each one-shot callback exactly once, including
//! cancellation/failure. Operations emit ordered events with one terminal event;
//! a cancelled cancellable expresses intent, not backend acknowledgement. Never
//! enumerate synchronously, perform filesystem work, spawn commands, or format
//! backend messages in callers. All such work belongs behind these ports.

use super::contracts::*;

pub type DirectoryCallback = Box<dyn FnOnce(RequestCompletion<DirectorySnapshot>)>;
pub type CapabilityCallback = Box<dyn FnOnce(RequestCompletion<CapabilitySnapshot>)>;
pub type OperationSink = Box<dyn FnMut(OperationEvent)>;
pub type SearchCallback = Box<dyn FnOnce(SearchCompletion)>;

pub trait DirectoryRepository {
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback);
}

pub trait FileOperations {
    fn inspect_capabilities(&self, request: CapabilityRequest, done: CapabilityCallback);
    /// Implementations recheck capability immediately before work, never
    /// overwrite conflicts by default, and report partial/unknown effects.
    fn submit(&self, request: OperationRequest, sink: OperationSink);
    /// Unsupported explicit conflict decisions safely retain no-overwrite behavior.
    fn submit_with_decision(
        &self,
        request: OperationRequest,
        _decision: ConflictDecision,
        sink: OperationSink,
    ) {
        self.submit(request, sink);
    }
}

pub trait SearchRepository {
    fn search(&self, request: SearchRequest, done: SearchCallback);
}
