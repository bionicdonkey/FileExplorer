# Conflicts, effects and scheduling

## Decisions

A transfer (copy or move) onto a name that already exists fails with `conflict`, changes nothing,
and reports `unchanged`. A caller may resubmit with one explicit decision:

| Decision | Effect on the existing target | The source |
| --- | --- | --- |
| `replace` | Overwritten | Copied or moved |
| `keep_both` | Left alone; the new item takes the first free numbered name, counting up from 2: `report (2).txt`, `report (3).txt`, and so on. The name is split at its last dot, so `a.tar.gz` becomes `a.tar (2).gz`; a name with no dot becomes `name (2)` | Copied or moved |
| `skip` | Left alone | Stays where it is and is reported as remaining; nothing reaches the backend |
| `cancel_remaining` | Left alone | Stays; this item and every later one in the batch is reported as remaining; nothing reaches the backend |

A backend that does not implement a decision keeps the no-overwrite behaviour. Which decision a
user chose, and whether to ask each time, is a preference of the application, not of the port.
[`adapter/operations/copy-conflict-*`, `adapter/operations/multi-item-conflict-decisions`,
`domain/operations/decision-*-never-reaches-the-backend`]

## Effects

Every terminal answer carries an effect and two lists of locations:

| Effect | Meaning |
| --- | --- |
| `unchanged` | Nothing on disk changed. |
| `changed` | Everything asked for was done. |
| `partially_changed` | Some items were done; `completed` and `remaining` say which. |
| `unknown` | The adapter cannot tell. It must say so rather than guess. |

`remaining` includes the item that failed and every item not yet attempted. There is no rollback
and no atomicity across items. A batch that fails after its first item is `partially_changed` with
that item in `completed`. The coordinator projects this as a change extent (`unchanged`, `complete`,
`partial`, `unknown`) and keeps it with the record. A cancellation reports the effects that actually
happened by the time the backend acknowledged it, and a cancellation after the last item reports
everything `changed`. [`adapter/operations/local-copy-move-conflict-and-delete-effects`,
`domain/operations/partial-failure-keeps-completed-items-and-refuses-unconfirmed-delete`]

## Cancelling

* Cancelling a **queued** operation cancels it before it starts: nothing reaches the backend.
* Cancelling a **running** operation only requests cancellation. The record shows
  `cancel_requested` until the backend answers, and the backend's answer wins. A backend that had
  already committed reports success, not cancellation, and that is accepted.
* Locks are held until the terminal event, not until cancellation is requested.
  [`domain/operations/cancel-race-preserves-the-backend-outcome`]

## Delete

A permanent delete is never submitted through the ordinary path. The caller must have obtained
explicit confirmation and uses the confirmed-delete entry; the ordinary submit refuses it.

## Scheduling and locks

Operations are serialised by the locations they touch. Each request derives a set of lock keys; a
request waits while any key is held by a running one.

* For local locations the keys are every ancestor of every source and destination, plus the
  destination joined with each source's name, written as canonical URIs and listed sorted and
  without duplicates. Two requests conflict when their key sets intersect.
* For any other location no relationship between locations is inferred from the text of the URI. A
  provider whose containment is **opaque** serialises against everything, trusted or not. A provider
  may supply proven ancestors, and only those are used.
* A transfer into a descendant of one of its own sources is refused at submit, and again after the
  destination's capabilities are refreshed. Where containment is unknown the transfer is refused too.
  [`domain/operations/remote-descendant-and-unproven-targets-are-refused`,
  `domain/operations/refreshed-target-ancestry-blocks-remote-descendant`,
  `domain/operations/opaque-request-blocks-a-trusted-request-and-local-lock-keys-are-sorted`]
* Every source and the destination are inspected, one after another, before a transfer is
  dispatched: a copy of three sources makes four inspections.
* Create and rename share locks with transfers, and are inspected again when a queued request
  becomes ready. [`domain/operations/create-and-rename-share-locks-and-reinspect-before-mutation`]

## Elevated retry

A request that failed with `permission_denied` may be retried with elevation only when all of the
following hold, and under a **new** operation id that is linked to the original:

* the original failed with `permission_denied` and its effect is `unchanged` (an uncertain or
  partial effect is never retried);
* it has exactly one source;
* every location involved is a local file location, not a remote one and not a mounted-remote
  projection;
* the retry's own capability checks still pass. During a linked retry a `permission_denied`
  inspection is tolerated (the elevated process may succeed where this one cannot), but any other
  missing capability fails the retry as `capability_changed`.

The original record is never altered by its retry.
[`domain/operations/elevated-retry-*`, `domain/operations/linked-elevation-keeps-permission-denied-exception`]
