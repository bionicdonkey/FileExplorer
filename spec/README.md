# spec/

The behaviour shared by the Linux app (Rust, GTK 4) and the planned macOS app (Swift). The two
codebases share no code; they share this directory. See `docs/macos-port-plan.md` for why, and
`docs/macos-port-spec.md` for the formats.

| Path | Holds |
| --- | --- |
| `features.json` | The feature registry: one entry per README feature, with a state per platform |
| `readme-ignore.json` | README bullets that deliberately have no feature, each with a reason |
| `scenarios/` | Behaviour scenarios, executed by each platform's runner (arrive with the Linux runner) |
| `fixtures/` | Binary fixtures that scenarios refer to |
| `contracts/` | Written semantics a scenario cannot express: ports, errors, conflicts, file types, search |
| `schema/` | JSON Schemas for all of the above |
| `tools/` | `spec-tools`, a small crate that checks the registry and prints the status table |

## Checking

```bash
cargo run --manifest-path spec/tools/Cargo.toml -- check
cargo run --manifest-path spec/tools/Cargo.toml -- status
sh scripts/check-separation.sh
```

`check` fails when a file breaks its schema, a README feature has no registry entry, a registry
entry's `readme` text matches no README line, a feature names a scenario that does not exist (or a
scenario no feature names), or an `approximate` or `excluded` state has no `note`. A feature with
neither scenarios nor `ui_tests` is reported as a warning until the scenario set is written.

## Adding a feature

1. Add the bullet to the README's `## Features` list.
2. Add an entry to `features.json`. `readme` is a substring of that bullet's first line. Set
   `linux` to `parity` and `macos` to `pending`.
3. Add its scenarios, and list them under `scenarios`.
4. Run `check`.

Changing what an existing scenario expects is a behaviour change for both apps: put a
`spec-change:` line in the commit message (see the spec, rule R2).
