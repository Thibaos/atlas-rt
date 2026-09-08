use godot::init::ExtensionLibrary;

struct AtlasExtension;

impl ExtensionLibrary for AtlasExtension {}

#[gdextension]
unsafe impl ExtensionLibrary for AtlasExtension {}

pub mod view;
