pub mod api;
pub mod internal;

use atlas_rt::world::update::job::WorldSource;
use godot::prelude::*;

const REJECT: &str = "atlas_rt: rejected input: ";
const ATLAS_MODE_UNIFORM: &str = "mode";
const ATLAS_FRAME_UNIFORM: &str = "atlas_frame";

/// A world file on the real filesystem, read by the loader thread.
struct VoxFile {
    path: String,
    name: String,
}

impl WorldSource for VoxFile {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn read(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.path).map_err(|error| error.to_string())
    }
}

/// Godot's camera pose as the view matrix the renderer's Vulkan projection
/// expects.
///
/// A `Basis` stores matrix rows. Each camera axis takes one component from
/// each row, with row 0 supplying x, row 1 y and row 2 z. Godot's camera looks
/// along local -Z, so forward is the negated third axis.
///
/// Godot's basis is right-handed and the renderer's world is left-handed.
/// Mirroring the basis on world x prevents a left-right flip relative to the
/// standalone app, the reference view.
#[must_use]
pub fn camera_view(origin: Vector3, basis: Basis) -> glam::Mat4 {
    let [row_0, row_1, row_2] = basis.rows;

    let axes = [
        glam::Vec3::new(row_0.x, row_1.x, row_2.x),
        glam::Vec3::new(row_0.y, row_1.y, row_2.y),
        -glam::Vec3::new(row_0.z, row_1.z, row_2.z),
    ];

    atlas_rt::render::camera::camera_view(
        glam::Vec3::new(origin.x, origin.y, origin.z),
        atlas_rt::render::camera::mirror_right(axes),
    )
}
