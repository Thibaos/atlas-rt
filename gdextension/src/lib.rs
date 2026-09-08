use godot::init::{gdextension, ExtensionLibrary};

pub mod view;
pub mod worker;

struct AtlasExtension;

unsafe impl ExtensionLibrary for AtlasExtension {}

#[gdextension]
unsafe impl ExtensionLibrary for AtlasExtension {}
