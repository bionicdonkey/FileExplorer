# Port rules

The three ports every platform implements: **directory** (list, page, visible metadata, watch),
**operations** (inspect capabilities, submit a mutation), **search**. This restates the rules in
`src/core/ports.rs` without any GLib vocabulary. Scenario ids in brackets show where a rule is
checked.

## Calling and answering

1. **Answers arrive on the caller's own context, never inline.** A port call returns promptly and
   its result is delivered later, on the context that made the call. It is never delivered before
   the call returns, and never on an I/O worker. The adapter scenarios assert this for every
   listing, search and inspection. [`adapter/directory/*`, `adapter/search/*`]
2. **Never block the caller.** All filesystem work, command spawning and message formatting belongs
   behind a port. Callers never enumerate, stat or spawn.
3. **Each request has an id, and an answer is only accepted for the request it names.** An answer
   carrying another request's id is ignored. Request ids are unique for the life of the process;
   operation ids are a separate namespace. [`domain/operations/callbacks-cannot-impersonate-another-request`,
   `domain/navigation/wrong-duplicate-and-mismatched-answers-are-rejected`]
4. **Exactly one terminal answer.** A one-shot request ends in one success, failure or
   cancellation. A stream (pages, operation events) ends in exactly one terminal element, and
   nothing follows it. A duplicate answer is ignored. [`domain/operations/reducer-rejects-*`]
5. **Operation events are ordered.** Each event carries a sequence number; a duplicate, an
   out-of-order, a wrong-id or a post-terminal event is rejected. The runner checks this itself for
   every operation, so scenarios do not repeat it.

## Cancellation

6. **Cancellation is intent.** Cancelling a request asks the adapter to stop; it does not remove the
   request. The adapter still delivers its terminal answer (`cancelled`, or the real outcome if the
   work had already committed), after it has released what it was using. A caller must never assume
   a cancelled request stays silent, and must ignore the answers of requests it has superseded.
   [`domain/navigation/latest-request-wins`, `domain/operations/cancel-race-preserves-the-backend-outcome`]
7. **The newest request wins.** A tab, a search and a picker each keep only their latest request.
   Starting another cancels the previous one, and an answer to a superseded request changes
   nothing. [`domain/navigation/*`, `domain/search/newest-query-wins-*`]

## Directory

8. **Pages.** A listing is a sequence of ordered pages from one cursor, ending in one page with no
   continuation, or a failure or cancellation. The first page is small enough to paint before the
   rest is read. There is no pre-count of the tree. [`adapter/directory/a-large-folder-streams-in-bounded-pages`]
9. **Offset paging needs a stable order.** A folder whose order is not stable (the capability
   snapshot says `stable_order: false`) answers an offset request with `unsupported`. [`adapter/directory/cancelled-missing-and-offset-listings-are-explicit`]
10. **Identity first, metadata later.** A listing carries each entry's identity, name, kind and access
    rights. Size, dates and content type are fetched only for rows currently visible, and must never
    delay the listing or the capability check. Access rights are not advisory: every command that
    changes or copies an entry is gated on them. [`adapter/directory/listing-returns-identity-without-eager-metadata`]
11. **Watching is per directory.** A watch covers one directory, never a tree.

## Operations

12. **Capabilities are checked, and checked again.** A request carries the capability version it was
    made against. The adapter rechecks before doing any work and answers `capability_changed`,
    reporting no effect, if the version or the action no longer holds. Absence from the snapshot
    means unsupported. [`domain/operations/create-and-rename-share-locks-*`]
13. **No overwrite by default.** A transfer onto an existing name fails with `conflict` and changes
    nothing, until an explicit decision says otherwise (see `conflicts.md`).
14. **Effects are reported truthfully.** Every terminal answer says whether anything changed, which
    items completed and which remain. An adapter that cannot be sure reports `unknown`.
15. **A snapshot contains no live handles, no raw paths and no backend messages.** Errors carry a
    kind and a message key (see `errors.json`), nothing else.

## Search

16. See `search.md`. A search never reaches a port while a navigation is pending, and an answer for a
    scope other than the requested one is not trusted.
