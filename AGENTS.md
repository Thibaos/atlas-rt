# atlas-rt

## Mannered prose

Mannered prose substitutes metaphor and flourish for direct statement. Instead of "a parameter worth varying," the mannered writer produces "a dial worth turning." Instead of "this point still matters," they write "this point earns its keep." The phrases exist to display the writer, not to convey the idea, and readers can tell. That is why mannered prose irritates: it makes the reader work harder so the writer can perform. It is also imprecise. Metaphors drag in connotations the writer did not choose and cannot control. The fix is to say what you mean. When a literal phrase is available, use it.

## When writing prose

No em dashes. Use a period or a comma, and do not substitute parentheses. Add
a line here when the unslop pass removes a tell that keeps coming back.

## When writing Rust and glsl code

- Keep documentation as minimal as possible
- Do not write in-code (within functions) documentation nor per-field documentation
- Always prefer a self-descriptive code rather than documentation
- Documentation should be as semantically dense and clear as possible, use the unslop skill preferably
- Separate blocks with a new line, before and after a "for", "if", "match", etc.
- You may separate some lines with a new line, to group related code together
- Run `cargo fmt --all` before finishing, so the tree stays rustfmt-clean

## When running the Godot example

Do not launch it windowed to verify something unless you have asked first and been
told yes. Each run opens a window, brings it to the front, and takes input focus for as
long as it runs, so a loop of runs takes the desktop repeatedly. The user said so after
five in a row.

A render image needs no window now. Save one headlessly and look at it instead of
asking for a screenshot:

    ./scripts/capture-godot.ps1 -World sand-ball -Eye 0,-1780,320 -Target 0,-1880,0

The script prints the PNG path on its last line. `-World` takes `castle`, `bistro`,
`sponza`, `nuke`, `sand-ball` or `generated`; `-Eye` and `-Target` aim the scene
camera, which the fixed World types need because they build at the bottom of the
lattice. The standalone app has the same pair of commands, `--capture` on the binary
and `scripts/capture-standalone.ps1`, which take `-Seed`, `-WorldType` and `-Ticks`.

`--headless` alone still cannot run the example: the engine draws no frame with the
headless display server, and the zero-copy probe finds no Vulkan device. The capture
driver covers both by registering an additional output, which is what makes the engine
draw, and the view then renders on atlas-rt's own device. The probe failure is
expected in that log and appears once.

`--headless` also remains the right way to parse-check a script:

    <godot-fork>\bin\godot.windows.editor.x86_64.exe --headless --path <project> \
        --check-only --script res://<script>

## Agent skills

### Issue tracker

Issues, specs, and wayfinder maps are markdown files under `.scratch/`, one directory per feature. See `docs/agents/issue-tracker.md`.

### Triage states

Read `docs/agents/triage-labels.md` before changing an issue's `Status:` line. It distinguishes completed work (`resolved`) from rejected work (`wontfix`).

### Domain docs

One context: `GLOSSARY.md` holds the glossary, `docs/adr/` holds the decisions. See `docs/agents/domain.md`.
