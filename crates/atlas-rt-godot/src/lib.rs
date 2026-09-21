pub mod view;
pub mod worker;

use godot::init::{ExtensionLibrary, gdextension};

struct AtlasExtension;

#[gdextension]
unsafe impl ExtensionLibrary for AtlasExtension {}
