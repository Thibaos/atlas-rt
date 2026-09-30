# Material behavior defaults to solid, and a malformed override is discarded whole

A physical material record has two fields. `rule` is the simulation behavior,
and `solid` says whether the occupied voxel blocks the player. The built-in
table defaults every material index to `solid` and player-blocking. Occupancy
still decides whether a voxel exists, so the table never invents a cell.

The table is separate from the Palette and may be replaced for a World load. An
override only changes the material indices it names, and it never touches the
Palette, so a world looks the same whether its override loads or falls back.

## Override format

The override is the optional sibling of a loaded world file, so `castle.vox` reads
`castle.vox_mat`, in a sparse line format that allows comments and blank lines,
one record per line.

```text
material <index> <rule> solid=<true-or-false>
```

The index runs from 0 through 255.

Any malformed record, unknown rule, invalid boolean, or out-of-range index
discards the entire override file and uses the built-in table. The file is
never partially applied, so behavior can never depend on a half-read file.

## Reporting

One terminal `warn!` per rejected override, and nothing else. The world still
loads with the built-in table, the status still ends Ready, and no report
reaches the view, because the player sees the fallback as the world loading
normally and has nothing to act on.

- The parse walks the whole file and collects every invalid record, so one load reports all of an author's typos rather than the first.
- The warning carries `file` and `rejected` as structured fields, with a message joining each rejection and ending by naming the built-in-table fallback. It follows the material-alpha warning in `world/palette.rs`, and lives in `world/material.rs` beside the loader that decides to reject.
- It fires unconditionally on every load where a present file was rejected. No flag.
- Presence decides it. An absent sibling is the normal case and produces nothing. A present file that fails for any reason, an unreadable file included, produces one warning.

## Considered options

- **A load-time report on the view**, beside the existing job status. Rejected: the fallback is not a load failure, and a report in the view would present an authoring mistake as something the player did wrong.
- **Nothing at all**. Rejected because silence is indistinguishable from an absent file. A typo in `castle.vox_mat` would produce no symptom at all, and the world would behave as though the file were not there.
- **A standalone validator that reads one override and prints its failures**. Deferred. The warning already names every bad line, and a separate tool earns its place when real authoring begins rather than at the first typo.
- **Build-time or editor linting**. Deferred with the validator.
- **A warning that only says the override was dropped**. Rejected. Since one invalid record discards the whole file, an aid that does not name the line and the reason leaves the author hunting line by line, which is the work the warning exists to remove.

## Lattice edges

Outside the lattice counts as unavailable for a rule destination. A grain whose
downward or diagonal target leaves the lattice settles exactly as it would
against a solid cell, so boundary columns pile up against the lattice the way
they pile up against a built wall. No read path exists that can leave the
lattice.

## Consequences

- A rejected override is a one-line terminal warning naming every bad line, not a silent behavior change.
- Every world loads, whether or not its override is present or valid, so an authoring mistake can never block a load.
- The built-in table is the only fallback target, which keeps the number of code paths a world can take at two.
- Fallback is total rather than partial by design. An author fixing one line at a time sees no change until the last bad line is gone.

## Status

accepted (2026-09-30). Promoted from the physics wayfinder's decisions 01, 10,
and 14, which were resolved against a prototype and had no earlier ADR. The
first prototype's standing decision was that every file problem falls back
silently; the reporting decision superseded the silent half and kept the
fallback.
