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

## When running the Godot example

Do not launch it windowed to verify something unless you have asked first and been
told yes. Each run opens a window, brings it to the front, and takes input focus for as
long as it runs, so a loop of runs takes the desktop repeatedly. The user said so after
five in a row. If what you need to check is node state or appearance, ask for a
screenshot instead: it answers that without a window, and the user supplies them
readily.

`--headless` cannot run the example past the API. With no window there is no Vulkan
device, so `VulkanHooksBridge` logs `no Vulkan device captured during engine device
creation`, the init probe fails, no frame is delivered, and every load stalls at 0.999.
Headless is still the right way to parse-check a script:

    <godot-fork>\bin\godot.windows.editor.x86_64.exe --headless --path <project> \
        --check-only --script res://<script>

