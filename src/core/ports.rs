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
//! Cancellation is intent only: terminal callbacks follow executor-owned native release.

use super::contracts::*;
use gio::prelude::CancellableExt;

pub type DirectoryCallback = Box<dyn FnOnce(RequestCompletion<DirectorySnapshot>)>;
pub type VisibleMetadataCallback = Box<dyn FnOnce(RequestCompletion<Vec<VisibleMetadata>>)>;
/// Ordered pages from one enumeration cursor, ending in exactly one page with
/// no continuation or a terminal failure/cancellation. No tree pre-count.
pub type DirectoryPageSink = Box<dyn FnMut(RequestCompletion<DirectorySnapshot>)>;
pub type CapabilityCallback = Box<dyn FnOnce(RequestCompletion<CapabilitySnapshot>)>;
pub type OperationSink = Box<dyn FnMut(OperationEvent)>;
pub type SearchCallback = Box<dyn FnOnce(SearchCompletion)>;

pub trait DirectoryRepository {
    fn list(&self, request: DirectoryRequest, done: DirectoryCallback);
    fn list_pages(&self, request: DirectoryRequest, mut sink: DirectoryPageSink) {
        self.list(request, Box::new(move |result| sink(result)));
    }
    /// Requests advisory row metadata only for rows currently visible in a view.
    /// It must not delay listing identity or capability checks.
    fn inspect_visible_metadata(
        &self,
        request: VisibleMetadataRequest,
        done: VisibleMetadataCallback,
    ) {
        let outcome = if request.cancellable.is_cancelled() {
            RequestOutcome::Cancelled
        } else {
            RequestOutcome::Succeeded(Vec::new())
        };
        done(RequestCompletion {
            id: request.id,
            outcome,
        });
    }
    /// Watch only this directory; never recursively walk a remote tree.
    fn watch(
        &self,
        _location: &crate::core::Location,
        _changed: Box<dyn Fn()>,
    ) -> Option<gio::FileMonitor> {
        None
    }
}

pub trait FileOperations {
    fn inspect_capabilities(&self, request: CapabilityRequest, done: CapabilityCallback);
    /// Operation-scoped inspection preserves a denied-access reason separately
    /// from an unsupported capability or an outdated capability version.
    fn inspect_operation_capabilities(
        &self,
        request: CapabilityRequest,
        _capability: Capability,
        done: CapabilityCallback,
    ) {
        self.inspect_capabilities(request, done);
    }
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
