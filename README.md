# Zorin Explorer

A native Linux file manager with a Windows 11 Explorer-inspired interaction model, built with Rust,
GTK 4, libadwaita and GIO. Aimed at Zorin OS and GNOME.

Status: pre-release (`0.3.3`). The core browsing, file-operation, search and preferences paths are
implemented and covered by tests, and tagged versions are published as a Debian package (the primary package) and as a Flatpak
bundle.

## Features

**Browsing**
- Tabs with overflow handling and an adaptive titlebar
- Four view modes — detailed list, compact list, icon grid, and a hybrid mode — with grid density
  options, sorting, grouping and configurable list columns
- Breadcrumb location bar with editable path entry
- Explorer-style sidebar: quick access, pinned locations, drives and network shares
- Incremental directory streaming with bounded paging, so large directories paint a first frame
  before enumeration finishes
- Thumbnails and file metadata fetched lazily for visible rows only, and cancelled when you
  navigate away
- Archives (`.zip`, `.tar` and its compressed forms, `.7z`, `.rar`, …) open in the window like
  folders, through the GVfs archive backend. Zip-based documents such as `.docx` and `.epub` are
  not archives for this purpose and still open in the application that owns them.

**Files**
- Copy, move, rename, delete, trash and create, with conflict resolution (replace, keep both, skip)
  and per-item outcome reporting
- An operations page showing in-flight and completed work, with persisted history
- Drag and drop, including between windows over X11
- A properties dialog with tabbed detail
- Permission-denied operations can be retried with elevation. The app never runs elevated: it
  re-executes itself through `pkexec` with `--elevated-op` to perform exactly one validated
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
| `src/application/` | Wiring and use-case coordination: lifecycle, navigation, operations, search, commands. |
| `src/ui/` | GTK 4 / libadwaita widgets: window, tabs, file view, sidebar, header, dialogs. |

### The GIO executor

All native GIO work runs on `infrastructure::gio_executor::GioExecutor`, a process-wide pool of
worker threads. Each worker has its own private `glib::MainContext` pushed as thread-default, and a
job runs entirely on the worker that took it — so every GIO handle is created, used and released on
one context. The pool shares a single job queue and a worker only takes a job while it is otherwise
idle, because jobs make blocking native calls and a single worker would let one slow transfer stall
every listing, thumbnail and search.

Two rules make this safe, and both are load-bearing:

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

Both rules above were learned the hard way. The second one in particular came from a use-after-free
that a single test run never caught: the sender that publishes DTOs to the requesting context holds
a GLib waker for a source that context owns, and glib requires that source to be finalized on the
context's own thread. Changing the bridges, the shutdown ordering or the adapter lifetimes means
re-verifying with repeated parallel runs, not one pass.

## Building

### Native

Needs a Rust toolchain plus GTK 4, libadwaita and GIO development packages.

```bash
# Debian / Ubuntu / Zorin OS
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev

cargo run            # debug
cargo build --release
```

### Debian package

The primary package. It uses the host's applications, GVfs, polkit and terminal directly, which a
Flatpak sandbox cannot offer a file manager. Built for Ubuntu 24.04 and newer, including Zorin OS 18.

```bash
packaging/deb/build-deb.sh          # writes dist/zorin-explorer_<version>_<arch>.deb
sudo apt install ./dist/zorin-explorer_*.deb
```

The `Depends` line is computed from the libraries the binary links, so build on the oldest release
the package should install on. `.gitea/workflows/deb.yml` builds it in an Ubuntu 24.04 container for
every push to `main` and attaches it to the release for every `v*` tag.

### Flatpak

Secondary. The sandbox hides the host's applications, so Open With has to go through the desktop
portal, and features that launch host programs are limited.

```bash
flatpak install flathub org.gnome.Platform//50 org.gnome.Sdk//50 \
                        org.freedesktop.Sdk.Extension.rust-stable//25.08

flatpak-builder --force-clean \
    --state-dir=/tmp/ze-state --repo=/tmp/ze-repo /tmp/ze-build \
    packaging/flatpak/com.zorin.Explorer.yml

flatpak build-bundle /tmp/ze-repo ZorinExplorer-0.1.0.flatpak com.zorin.Explorer master
flatpak install --user ZorinExplorer-0.1.0.flatpak
```

Keep the build and state directories outside the checkout. The manifest's source is the checkout
itself, so anything written inside it gets copied into the next build.

The Rust extension version tracks the runtime's freedesktop base, so it moves with
`runtime-version` in the manifest: GNOME 50 is built on 25.08. The CI workflow pins the same three
versions and has to be updated alongside it.

Every push to `main` builds the same bundle in CI — see `.gitea/workflows/flatpak.yml` — and
attaches it as a run artifact, kept for 30 days.

A release is cut by tagging. The tag must match the crate version, or the job fails before creating
anything:

```bash
# with Cargo.toml at version = "0.1.0"
git tag v0.1.0 && git push origin v0.1.0
```

That runs the same build and publishes the bundle as a release asset, which is the permanent copy.
Re-running a tag attaches to the existing release and replaces the asset rather than duplicating it.

That job needs two things from its runner, because flatpak-builder sandboxes each module with
bubblewrap, which has to create a user namespace and mount a fresh procfs inside it:

- `kernel.apparmor_restrict_unprivileged_userns=0` on the runner host. It defaults to 1 on
  Ubuntu 24.04 and later, and at 1 the build fails with `bwrap: No permissions to create a new
  namespace`.
- `container.privileged: true` in that runner's act_runner config. Docker masks 12 paths under
  `/proc` and makes 5 read-only, and the kernel refuses to mount a fresh procfs in a nested
  namespace when that would unmask them: `bwrap: Can't mount proc on /newroot/proc`. Privileged
  mode is the only way to clear those masks through act_runner —
  `--security-opt systempaths=unconfined` is translated by the Docker CLI into empty
  `MaskedPaths`/`ReadonlyPaths` rather than being sent as a security option, so it never reaches a
  daemon driven through the API.

The job is therefore pinned to the `flatpak` label rather than `ubuntu-latest`, so it only runs on
the one runner configured that way. The bundle is uploaded with `actions/upload-artifact@v3`, not
v4: v4 refuses any GitHub Enterprise-style server client-side, which is how Gitea presents itself. The workflow's first step reports the sysctl, the effective
capabilities and the seccomp mode, so a failing run says which requirement is missing.

Running the job in host mode instead does not help when act_runner is itself containerised: the
"host" is then the runner's own image, which carries none of the toolchain.

## Testing

```bash
xvfb-run -a cargo test
```

Around 300 tests across the library and eleven integration binaries: contract tests for locations,
navigation, search and operations; GIO integration and executor lifecycle tests; headless GTK tests
that drive real widgets; accessibility and performance checks.

Two things to know before you trust a green run:

- **Never run the GTK or X11 tests against your live display.** They create, present and close real
  windows and drive X11 drag-and-drop. Use `xvfb-run -a`, always. Some library tests are marked
  `#[ignore = "requires a GTK desktop session"]` and stay out of the default run.
- **A single green run does not establish concurrency correctness.** Two native use-after-free bugs
  in this codebase only showed up across repeated parallel runs — at roughly 25% and 6% of runs —
  and libtest's output capture reported the aborts as unrelated timeouts. To verify work that
  touches threads, contexts or GIO lifetimes, run each test binary directly, dozens of times, and
  check for signals separately from assertion failures:

  ```bash
  cargo build --tests
  binary=$(find target/debug/deps -name 'gio_integration-*' -type f -executable | head -1)
  for i in $(seq 1 100); do
    xvfb-run -a "$binary" >/dev/null 2>&1 || echo "run $i exited $?"
  done
  ```

  Exit codes at or above 128 are signals — a crash, not a failed assertion.

There is no gdb or valgrind requirement, but `eu-stack --core=core -e <binary>` (elfutils) on a core
dump and `LD_PRELOAD=libasan.so.8` with `ASAN_OPTIONS=verify_asan_link_order=0` both work for
diagnosing native faults.

## Known limitations

- **GVfs mount-spec use-after-free (upstream).** gvfs 1.54.4 can free a `GMountSpec` while another
  thread resurrects it from the library's global `unique_hash`, so constructing a non-`file:` GIO
  file while another thread finalizes one for the same mount corrupts the heap. `Location` works
  around it by retaining one file per non-local mount for the process lifetime, which costs one
  small object per remote share visited. Remove the workaround once the host gvfs carries the fix;
  `parallel_non_local_locations_survive_repeated_construction_and_teardown` is the check that it is
  still needed.
- `resources/com.zorin.Explorer.gschema.xml` is present but unused — preferences persist as JSON
  under `$XDG_CONFIG_HOME`, not GSettings.
- Session restore is implemented in `core::application_preferences` (`encode_session` /
  `decode_session`, schema-versioned, with a V1 import path) but nothing calls it, so open tabs do
  not survive a restart.
- Opening a file *inside* an archive hands its `archive://` URI to the desktop. Outside the sandbox
  GVfs offers a FUSE path and this works; the Flatpak portal takes `file:` and `http(s):` only, so
  it is expected to do nothing there. Extract the archive first.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
