# Performance budgets

Per-platform p95 budgets for the macOS app, ported from the scenarios in the Linux
`tests/performance.rs`. Budgets are per platform by design (spec section 5.6): these start from the
Linux numbers, and the values below are what the reference Mac measures.

`PerformanceBudgetTests.swift` runs with the unit tests (`sh scripts/xcode-test.sh`) and prints a
`budget …` line per measurement.

| Scenario | Budget (p95) | Measured |
|---|---|---|
| local folder of 10,000 items: first content on screen | 500 ms | 88 ms |
| local folder of 10,000 items: navigation acknowledged | 100 ms | < 1 ms |
| 100 history records: cold load | 100 ms | 1 ms |
| 100 history records: warm projection | 100 ms | < 1 ms |
| sort and group 50,000 items | 1,000 ms | 176 ms |
| session and history stay bounded | (bound check, not a time) | passes |

Reference machine: a 2012 Mac mini (Intel x86_64) on macOS 15 with a **Debug** build, which is the
slowest configuration this project runs; the Apple silicon half of the universal binary is not
measured. The Linux scenarios that measure GTK frame times, fifty-tab layouts and the Linux
history file have no counterpart here because AppKit does not expose the same hooks; a frame-time
budget on the Mac would need Instruments.
