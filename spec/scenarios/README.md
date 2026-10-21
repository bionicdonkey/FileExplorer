# Scenario conventions

`docs/macos-port-spec.md` section 3 defines the format. This file records what the first runner
settled where that section left a choice, and where it differs from the section. A runner on
another platform follows this file.

## Both kinds

* A scenario's `id` is its path: `scenarios/<kind>/<area>/<name>.json` has
  `"id": "<kind>/<area>/<name>"`. Areas and names use lower-case letters, digits and hyphens.
  `spec-tools check` enforces it.
* Expected values come from what the platform's existing tests assert, never from the examples in
  the spec. Production code is not changed to make a scenario pass: a failing scenario means the
  scenario or the runner is wrong, or it is a real bug to report.
* A step has exactly one of `do`, `expect`, or a port script (`port` + `request` + `respond` or
  `emit`). The runner checks, without a scenario saying so, that every event stream has exactly one
  terminal event, that it is last, and that sequence numbers strictly increase.
* Error kinds and retry classes are snake_case (`permission_denied`, `after_user_action`). Effects
  are `unchanged`, `changed`, `partially_changed`, `unknown`. The full list is in
  `contracts/errors.json`.
* `platforms.<p>.replace_step` / `skip_step` follow the spec. A scenario that only one platform can
  run says so in its `note`, and the other platform overrides the steps it cannot run.
* `given.requires: ["unprivileged"]` makes the runner skip (and pass) a scenario when the process
  can read a mode-000 file, since permission failures cannot be produced there.

## Adapter scenarios

Paths are relative to the sandbox root. A path containing `://` is a URI used as written, and
`in-archive:ARCHIVE!/INNER` names an entry inside a local archive (INNER is URI-escaped).

Implemented actions: `operate`, `mutate`, `list`, `list_pages`, `inspect`, `inspect_visible`,
`search`, `manage`, `extract`, `compress`.

* `operate` runs the adapter's executor entry and records every event. `capability_version` is the
  adapter's current one, taken implicitly, unless the step names one; a scenario names a version only
  to test `capability_changed`. `target` is the explicit target; without it a transfer goes into
  `destination`. Entries inside an archive go through the archive adapter.
* `list`, `list_pages`, `inspect`, `inspect_visible` and `search` run through the production adapters
  and complete inside their own step. They differ from the spec, which has separate `cancel` and
  `wait` steps: a step names when to cancel with `"cancel": "immediately"` (or, for `list_pages`,
  `"after_first_page"`). The runner asserts that no callback ran before the call returned.
* `manage` submits operations through the operation manager with history saved to a file outside the
  sandbox, and waits for each to finish. `extract` and `compress` call the application's own entry
  points. `compress` takes `format`: `zip`, `7z` or `tar_gz`.
* Expectations: `outcome`, `effect` (with `completed` and `remaining` as sandbox paths), `events`,
  `tree`, `entries`, `entry`, `pages`, `capability`, `search`, `metadata`, `history`.
  * `tree` accepts `contents`, mapping a path to the exact text it must hold (an addition).
  * `entry` checks one listed entry: `kind`, `path`, `has` / `lacks` capabilities, and `null` for
    `size`, `modified` and `content_type` to assert they were not loaded eagerly.
  * `search.limits` is **inclusion**: the limits listed must be among those reported. The reported
    limits are the bounds in force, not only those hit (see `contracts/search.md`).
  * `outcome` for `compress` also accepts `archive`, the new archive's path.

## Domain scenarios

Locations are URIs, used as written. There is no sandbox. `given.location` is the tab's starting
location and `given.capabilities` is the default capability snapshot for the `operations` subject.

Every `do` step names its `subject`; expectations about a subject name it too. Subjects:

| Subject | Does |
| --- | --- |
| `navigation` | `navigate`, `back`, `forward`, `refresh`, `retry`, `close`, `select`, `unmount`, `show_section` |
| `operations` | `submit`, `retry_elevated`, `cancel` (the real coordinator over a scripted backend) |
| `reducer` | `start`, `apply` (the event reducer) |
| `search` | `submit`, `type` (debounced), `retry`, `expand_scope`, `clear` |
| `clock` | `advance` (virtual time behind the debounce; nothing sleeps) |
| `search_history` | `record_submitted` |
| `clipboard` | `replace`, `begin_paste`, `finish_paste` |
| `pins` | `pin`, `save`, `reload` |

Pure expectations that need no subject: `location`, `session`, `named_child`, `clipboard_payload`,
`activation`, and the two table checks `file_types_table` and `error_table`, which compare
`contracts/file-types.json` and `contracts/errors.json` with the code.

Scripted ports: `directory`, `capabilities` (the coordinator's capability inspections), `operations`
(dispatched mutations) and `search`. `request` is the 1-based index of requests the port has
received, or `"latest"`. A port step may add `as_id` to answer with a different request or operation
id (impersonation tests). Responses:

* directory: `"cancelled"`, `{"failed": {"kind"}}`, `{"succeeded": {"entries": [uri], "resolved",
  "snapshot_for"}}`
* capabilities: the same, with a capability snapshot in `succeeded` (`with`, `without`, `containment`)
* operations: `emit` takes `"started"`, `{"progress": [done, total]}`, `"cancel_requested"`,
  `{"succeeded"|"failed"|"cancelled": {effect, completed, remaining, kind}}`; sequence numbers are
  assigned in order
* search: `{"completed"|"incomplete"|"unsupported": {...}}`

`expect: view` for navigation projects `location`, `status` (`idle`, `loading`, `ready`, `failed`,
`closed`), `back`, `forward` (a target URI or `null`), `current`, `content`
(`directory:<uri>`, `unavailable:<uri>`, `section`, `search`), `has_directory`, `available`,
`pending`, `can_go_back`, `can_go_forward`, `selection`, `failed_location` and `error`. This replaces
the spec's `history` array: the Linux history type does not expose its entries, and the back and
forward targets are the behaviour that matters. For search it projects `status` (`none`,
`completed`, `incomplete`, `unsupported`), `reason`, `content`, `matches`, `selection` and
`can_go_back`.

`request_cancelled` takes `is: false` to assert a request was **not** cancelled. `record` (operations)
checks `state`, `error`, `change`, `completed`, `remaining`, `events`, `events_count`,
`lacks_events`, `terminal_events` and `retry_of`. `lock_keys` checks the active keys, or computes the
keys for a request.

`submit` and the other actions accept `accepted: true|false` to assert whether the coordinator took
them, where it matters.

## UI features

A feature with no scenarios lists the platform's own tests in `ui_tests.<platform>`. For Linux each
entry is `path/to/file.rs::function`, and `spec-tools check` verifies that the function exists.
