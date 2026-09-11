use godot::init::{gdextension, ExtensionLibrary};

pub mod view;
pub mod worker;

struct AtlasExtension;

#[gdextension]
unsafe impl ExtensionLibrary for AtlasExtension {}
