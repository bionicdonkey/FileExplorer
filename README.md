# Zorin Explorer

A native Linux file manager with a Windows 11 Explorer-inspired interaction model, built with Rust,
GTK 4, libadwaita and GIO. It targets Zorin OS and GNOME.

The project is a pre-release (`0.5.0`). Core browsing, file operations, search and preferences are
implemented and covered by tests. Tagged versions are published as a Debian package.

## Features

**Browsing**
- Tabs with overflow handling and an adaptive titlebar
- Four view modes — detailed list, compact list, icon grid, and a hybrid mode — with grid density
  options, sorting, grouping and configurable list columns
- Breadcrumb location bar with editable path entry
- Explorer-style sidebar: quick access, pinned locations, drives and network shares
- Incremental directory streaming with bounded paging, so large directories paint a first frame
  before enumeration finishes
- Thumbnails and file metadata fetched lazily for visible rows only, and cancelled when the view
  navigates away
- Archives (`.zip`, `.tar` and its compressed forms, `.rar`, …) open in the window like folders,
  through the GVfs archive backend. Zip-based documents such as `.docx` and `.epub` are not
  archives for this purpose and still open in the application that owns them. `.7z` is the
  exception: GVfs cannot read it — its archive backend hands libarchive a non-seekable stream, and
  the 7z header layout needs random access — so a `.7z` opens in the default application and
  **Extract** unpacks it instead.
- **Extract** puts an archive's contents in a new folder named after it, unless the archive is a
  single folder, which lands beside the archive as it is.
- **Compress** packs selected local files and folders into a `.zip`, `.7z` or `.tar.gz` beside
  them.
- Both directions run in-process. Extraction hands a local archive to libarchive — the same library
  GVfs uses underneath, but opened directly so it can seek — which is why a `.7z` or `.rar` extracts
  exactly like a `.zip`, and why no archiver program has to be installed. Compression uses pure-Rust
  writers (`zip`, `sevenz-rust2`, `tar`/`flate2`). A remote archive still goes through GVfs: there
  is no local file to hand to libarchive, so it is mounted and copied like any other remote folder.
- Everything running — compressing, extracting, copies and moves — is shown in the status bar as a
  label and bar beside the spinner. The label expands into the list of running operations, each
  with its own progress and Cancel button, and the Operations tab keeps the full history. A local
  extraction reports the bytes libarchive reads; a remote one shows "Preparing…" first, while GVfs
  opens the archive.

**Files**
- Copy, move, rename, delete, trash and create, with conflict resolution (replace, keep both, skip)
  and per-item outcome reporting
- An operations page showing in-flight and completed work, with persisted history
- Drag and drop, including between windows over X11
- A properties dialog with tabbed detail
- Permission-denied operations can be retried with elevation. The app itself never runs elevated:
  it re-executes itself through `pkexec` with `--elevated-op` to perform exactly one validated
  operation, then exits.

**Search**
- Tracker-backed search where available, with a GIO filesystem fallback and an explicit
  "Tracker unavailable" outcome rather than silent degradation
- Filename and content modes, depth and time bounds, symlinks never followed
- Search history is subject to a privacy preference, and disabling it removes stored terms

**Remote locations**
- Non-local URIs (`smb://`, `sftp://`, `trash://`, `network://`, …) are first-class. A remote
  location is never silently treated as a local path, so local-only actions such as "Open in
  Terminal" stay unavailable even when GVfs offers a FUSE projection.
- GTK-parented mount authentication, with mount retry on the requesting context

**Session**
- Tabs, pinned locations and view preferences persist across restarts, with schema-versioned state
  and migration from the older format
- Persisted operation history redacts path detail

## Architecture

The crate is a hexagonal (ports and adapters) design. The dependency direction is
`ui` / `application` → `core` ← `infrastructure`.

| Module | Role |
| --- | --- |
| `src/core/` | Domain model and pure logic: `Location` (canonical URI identity), navigation history, view options, sorting and grouping, operation coordination, session state. No GTK. |
| `src/core/ports.rs` | The three traits the domain depends on: `DirectoryRepository`, `FileOperations`, `SearchRepository`. Callback/event APIs, never blocking. |
| `src/core/contracts.rs` | The `Send` DTOs that cross those ports — snapshots, outcomes, errors. No GIO handles. |
| `src/infrastructure/` | GIO/GVfs and Tracker adapters implementing the ports, plus the GIO executor. |
| `src/application/` | Wiring and use-case coordination: lifecycle, navigation, operations, search, commands, and the archive engine (libarchive for reading, pure-Rust writers for writing). |
| `src/ui/` | GTK 4 / libadwaita widgets: window, tabs, file view, sidebar, header, dialogs. |

### The GIO executor

All native GIO work runs on `infrastructure::gio_executor::GioExecutor`, a process-wide pool of
worker threads. Each worker has its own private `glib::MainContext` pushed as thread-default, and a
job runs entirely on the worker that took it, so every GIO handle is created, used and released on
one context. The pool shares a single job queue, and a worker only takes a job while it is otherwise
idle, because jobs make blocking native calls and a single worker would let one slow transfer stall
every listing, thumbnail and search.

Two invariants keep this safe, and both are load-bearing:

- **A terminal result is published only after the native completion callback has released its GIO
  values.** Requests stay registered with the executor until then, and cancellation does not remove
  that registration early.
- **A requesting context may not release its `glib::MainContext` while the executor still holds a
  bridge into it.** The DTO senders carry GLib wakers owned by the receiving context, so
  `OwnerContextGuard` leases them and the owner always performs the final unref itself. Dropping the
  last adapter handle drains, so ordinary teardown needs no special handling.

Shutdown cancels live requests, keeps the GTK loop running while owner-context mount callbacks
drain, then joins off the UI thread. Blocking the UI thread on the join instead deadlocks, because a
pending GTK mount dialog needs that same context to run before it can release its lease.

Both invariants come from real failures in this codebase. The second one in particular came from a
use-after-free that a single test run never caught: the sender that publishes DTOs to the requesting
context holds a GLib waker for a source that context owns, and glib requires that source to be
finalized on the context's own thread. Changes to the bridges, the shutdown ordering or the adapter
lifetimes therefore need repeated parallel runs to validate, not one pass.

## Building

### Native

A Rust toolchain plus the GTK 4, libadwaita, GIO and libarchive development packages are required.

```bash
# Debian / Ubuntu / Zorin OS
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev libarchive-dev

cargo run            # debug
cargo build --release
```

### Debian package

The shipping package. It uses the host's applications, GVfs, polkit and terminal directly, which a
sandboxed package format cannot offer a file manager. It is built for Ubuntu 24.04 and newer,
including Zorin OS 18.

```bash
packaging/deb/build-deb.sh          # writes dist/zorin-explorer_<version>_<arch>.deb
sudo apt install ./dist/zorin-explorer_*.deb
```

The `Depends` line is computed from the libraries the binary links, so the package is built on the
oldest release it should install on. `.gitea/workflows/deb.yml` builds it in an Ubuntu 24.04
container for every push to `main`, and attaches it to the release for every `v*` tag.

A release is cut by tagging. The tag must match the crate version, or the job fails before creating
anything:

```bash
# with Cargo.toml at version = "0.5.0"
git tag v0.5.0 && git push origin v0.5.0
```

Re-running a tag attaches to the existing release and replaces the asset rather than duplicating it.

## Testing

```bash
xvfb-run -a cargo test
```

Around 300 tests span the library and twelve integration binaries: contract tests for locations,
navigation, search and operations; GIO integration and executor lifecycle tests; headless GTK tests
that drive real widgets; and accessibility and performance checks.

Two caveats apply to interpreting a green run:

- **The GTK and X11 tests run under a virtual display, never against a live one.** They create,
  present and close real windows and drive X11 drag-and-drop, so a virtual display is required.
  Some library tests are marked `#[ignore = "requires a GTK desktop session"]` and stay out of the
  default run.
- **A single green run is not evidence of concurrency correctness.** Two native use-after-free bugs
  in this codebase appeared only across repeated parallel runs — at roughly 25% and 6% of runs —
  and libtest's output capture reported the aborts as unrelated timeouts. Work that touches threads,
  contexts or GIO lifetimes is verified by running each test binary directly, repeatedly, and
  checking for signals separately from assertion failures:

  ```bash
  cargo build --tests
  binary=$(find target/debug/deps -name 'gio_integration-*' -type f -executable | head -1)
  for i in $(seq 1 100); do
    xvfb-run -a "$binary" >/dev/null 2>&1 || echo "run $i exited $?"
  done
  ```

  Exit codes at or above 128 indicate a signal — a crash, not a failed assertion.

No gdb or valgrind is required, but `eu-stack --core=core -e <binary>` (elfutils) on a core dump and
`LD_PRELOAD=libasan.so.8` with `ASAN_OPTIONS=verify_asan_link_order=0` both work for diagnosing
native faults.

## Known limitations

- **GVfs mount-spec use-after-free (upstream).** gvfs 1.54.4 can free a `GMountSpec` while another
  thread resurrects it from the library's global `unique_hash`, so constructing a non-`file:` GIO
  file while another thread finalizes one for the same mount corrupts the heap. `Location` works
  around it by retaining one file per non-local mount for the process lifetime, at the cost of one
  small object per remote share visited. The workaround can be dropped once the host gvfs carries
  the fix; `parallel_non_local_locations_survive_repeated_construction_and_teardown` is the check
  that it is still needed.
- A remote `.7z` cannot be extracted. GVfs cannot browse it and there is no local file for
  libarchive to open, so **Extract** is unavailable for one on an `smb://` or `sftp://` share.
- `resources/com.zorin.Explorer.gschema.xml` is present but unused — preferences persist as JSON
  under `$XDG_CONFIG_HOME`, not GSettings.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
