# spec/

The behaviour shared by the Linux app (Rust, GTK 4) and the planned macOS app (Swift). The two
codebases share no code; they share this directory. See `docs/macos-port-plan.md` for why, and
`docs/macos-port-spec.md` for the formats.

| Path | Holds |
| --- | --- |
| `features.json` | The feature registry: one entry per README feature, with a state per platform |
| `readme-ignore.json` | README bullets that deliberately have no feature, each with a reason |
| `scenarios/` | Behaviour scenarios, executed by each platform's runner. Conventions in `scenarios/README.md` |
| `fixtures/` | Binary fixtures that scenarios refer to (real zip, 7z and tar.gz archives) |
| `contracts/` | Written semantics: `ports.md`, `conflicts.md`, `search.md`, and the tables `errors.json` and `file-types.json`, which scenarios check against the code |
| `schema/` | JSON Schemas for all of the above |
| `tools/` | `spec-tools`, a small crate that checks the registry and prints the status table |

## Running

```bash
cargo test --test spec_scenarios                          # the Linux runner
SPEC_FILTER=navigation cargo test --test spec_scenarios   # only scenarios whose id contains the text
SPEC_REPEAT=50 cargo test --test spec_scenarios           # soak: run each selected scenario 50 times
cargo run --manifest-path spec/tools/Cargo.toml -- check
cargo run --manifest-path spec/tools/Cargo.toml -- status
sh scripts/check-separation.sh
```

`check` fails when a file breaks its schema, a README feature has no registry entry, a registry
entry's `readme` text matches no README line, a feature names a scenario that does not exist (or a
scenario no feature names), an `approximate` or `excluded` state has no `note`, a contract cites a
scenario that does not exist, or a `ui_tests` entry names a function that is not there (a Linux entry is `path/to/file.rs::function` from the repository root, a macOS entry `path/to/File.swift::testFunction` from the macOS app's own folder). A
feature with neither scenarios nor `ui_tests` is reported as a warning.

## Adding a feature

1. Add the bullet to the README's `## Features` list.
2. Add an entry to `features.json`. `readme` is a substring of that bullet's first line. Set
   `linux` to `parity` and `macos` to `pending`.
3. Add its scenarios, and list them under `scenarios`. A UI feature lists its platform tests under
   `ui_tests` instead.
4. Run the scenarios and `check`.

When you write a scenario, break an expectation on purpose and confirm the scenario fails. A scenario
that cannot fail is worth nothing.

Changing what an existing scenario expects is a behaviour change for both apps: put a
`spec-change:` line in the commit message (see the spec, rule R2).
