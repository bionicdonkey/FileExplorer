# Scenario conventions

`docs/macos-port-spec.md` section 3 defines the format. This file records what the first runner
settled where that section left a choice. A runner on another platform follows it.

## Both kinds

- Every scenario is bound by its path: `scenarios/<kind>/<area>/<name>.json` has
  `"id": "<kind>/<area>/<name>"`. `spec-tools check` enforces it.
- Expected values come from what the platform's existing tests assert, never from the examples
  in the spec. Production code is not changed to make a scenario pass: a failing scenario means
  the scenario or the runner is wrong, or it is a real bug to report.
- A step has exactly one of `do`, `expect`, or a port script (`port` + `request` + `respond` or
  `emit`). The runner checks, without a scenario saying so, that every event stream has exactly
  one terminal event, that it is last, and that sequence numbers strictly increase.
- Error kinds and retry classes are written in snake_case (`permission_denied`,
  `after_user_action`). Effects are `unchanged`, `changed`, `partially_changed`, `unknown`.

## Adapter scenarios

- Paths are relative to the sandbox root.
- `operate` runs the adapter's synchronous executor entry (Linux: `gio_operations::execute` and
  `execute_with_decision`) and records every event. The request's `capability_version` is the
  adapter's current one, taken implicitly, unless the step names one. A scenario names a version
  only to test `capability_changed`.
- `operate.target` is the request's explicit target. Without `target`, the copy or move goes into
  `destination`.
- `expect: tree` also accepts `contents`, mapping a path to the exact text it must hold (an
  addition to the spec). `contains` and `absent` test existence without following symlinks.
- Implemented so far: `operate`, `mutate`, and the expectations `outcome`, `effect`, `events`,
  `tree`. Cancellation (`cancel`, `wait`) arrives with the asynchronous actions (`list`, `search`,
  `watch`): `operate` is synchronous, so a cancelling scenario needs `operate` to name when to cancel.

## Domain scenarios

- Locations are URIs, used as written. There is no sandbox.
- `given.location` is the tab's starting location.
- Every `do` step names its `subject` (`navigation`, `search`, `operations`, `clipboard`,
  `clock`), so one scenario can drive several. Expectations about a subject (`view`, `record`,
  `clipboard`) name it too.
- Port scripting answers a recorded request: `request` is the 1-based index of requests the port
  has received, or `"latest"`. For the directory port, `respond` is `"cancelled"`,
  `{"failed": {"kind": ...}}` or `{"succeeded": {"entries": [uri, ...], "resolved": uri}}`.
- `expect: view` for `navigation` projects `location`, `status` (`idle`, `loading`, `ready`,
  `failed`, `closed`), `back` and `forward` (the target URI or `null`), and `error` (a kind).
  This replaces the spec's `history` array: the Linux history type does not expose its entries,
  and the back and forward targets are the behaviour that matters.
- `request_cancelled` checks the request's cancellation token. `port_requests` checks the count
  and the location of the last request.
- Implemented so far: the directory port and the `navigation` subject.
