# AtlasRtView example project wiring

1. Build the extension library:

        cd gdextension
        cargo build --release

2. Copy the built library into the project as Godot expects it next to
   atlas_rt.gdextension:

        mkdir lib
        copy ..\..\target\release\atlas_rt_godot.dll lib\

3. Open the project with the custom template binary:

        <godot-cloned>\bin\godot.windows.template_release.x86_64.exe --path .

   The stock editor binary runs cpu delivery per the plan; zero-copy is
   exercised through the export-run loop.

4. The init log line names the backend. When zero-copy misses the probe the
   log records the reason and delivery degrades per ADR 0005; no retry in v1.

Scene layout. main.tscn hosts a Camera3D; main.gd adds an AtlasRtView
full-rect that reads the camera each tick and loads a .vox world.
