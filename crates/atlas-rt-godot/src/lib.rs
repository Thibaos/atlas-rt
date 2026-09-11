use godot::init::{ExtensionLibrary, gdextension};

pub mod view;
pub mod worker;

struct AtlasExtension;

#[gdextension]
unsafe impl ExtensionLibrary for AtlasExtension {}
