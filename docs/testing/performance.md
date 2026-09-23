# Scale and performance acceptance

Run the non-GTK state and repository gate first, then the GTK rendering gate
on a real X11 or Xvfb display:

```sh
cargo test --test performance
xvfb-run -a cargo test --test headless_gtk slice5_ -- --nocapture
```

Every benchmark discards run 1 as warm-up, measures 30 cold and 30 warm runs,
and calculates nearest-rank p95. The `performance` integration binary serializes
its independent cases because GIO and GLib main-context ownership are process
global; benchmark code still exercises the production GIO directory adapter.

## Fixtures and budgets

| Fixture | Measurement | Budget |
| --- | --- | --- |
| 10,000 local files | first rendered content, cold/warm | 500 ms |
| 10,000 simulated paged remote items | first rendered content after backend response, cold/warm | 1 s |
| Local and remote navigation | acknowledgement | 100 ms |
| 10,000-entry page delivery | one main-context turn | 100 ms |
| 50 tabs | creation acknowledgement and warm selection/event turn | 100 ms |
| 100 persisted operation records | cold load and warm projection | 100 ms |

The remote fixture emits an ordered 128-entry page once per millisecond and
panics if a caller requests its full-list API. The local fixture is a temporary
directory of 10,000 files. Session and operation fixtures exercise 50 saved
tabs and 100 retained terminal records. “Cold” means a new application model
and empty thumbnail cache; it does not attempt to flush the operating-system
filesystem cache. “Warm” reuses the fixture after a discarded completed run.

## Recorded run: 2026-09-23

Runner: AMD Ryzen 7 9800X3D (8 cores), 63,374,476 kB RAM, Zorin OS 18.1,
GTK 4.14.5. The GTK run used Xvfb and GTK's default renderer. Xvfb reported a
non-fatal DRI3 software-rendering warning.

| Gate | n | p95 | Budget | Result |
| --- | ---: | ---: | ---: | --- |
| Local first content | 30 | 3.32 ms | 500 ms | pass |
| Local navigation acknowledgement | 30 | 43.02 us | 100 ms | pass |
| 100-record cold history load | 30 | 2.04 ms | 100 ms | pass |
| 100-record warm history projection | 30 | 25.29 us | 100 ms | pass |
| GTK local cold first frame | 30 | 41.77 ms | 500 ms | pass |
| GTK local warm first frame | 30 | 59.66 ms | 500 ms | pass |
| GTK remote cold first frame | 30 | 43.63 ms | 1 s | pass |
| GTK remote warm first frame | 30 | 47.05 ms | 1 s | pass |
| GTK local acknowledgement | 60 | 428.80 us | 100 ms | pass |
| GTK remote acknowledgement | 60 | 485.76 us | 100 ms | pass |
| GTK local page main-context turn | 35,892 | 246.20 us | 100 ms | pass |
| GTK remote page main-context turn | 35,131 | 425.68 us | 100 ms | pass |

| 50-tab creation acknowledgement | 1,500 | 136.28 us | 100 ms | pass |
| 50-tab warm selection/event turn | 1,500 | 2.18 ms | 100 ms | pass |

Keep the full raw gate output with the CI job or task report because p95 values are inherently
runner-specific. Do not compare the values above across machines; only their
budgets are acceptance criteria.

## What the gate covers

The directory port streams ordered pages with one request generation and a
two-page producer queue. The tab state rejects stale generations and malformed
continuations, commits the first valid page before completion, and preserves
later-page selection. Large GTK list/grid views use `ListStore` plus GTK's
incremental sort/filter wrappers and recycled cells. Thumbnail decoding is
queued and bounded to live cells, and directory-monitor changes invalidate its
URI-keyed cache before a debounced refresh. Saved tabs, navigation/search
history, operation projections, and persisted files are bounded; oversized or
torn state recovers as an empty safe state with a recoverable notice.
