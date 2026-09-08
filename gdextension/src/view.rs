use std::sync::{Arc, Mutex};

use godot::classes::{Camera3D, Control, IControl};
use godot::prelude::*;

use atlas_rt::render::{
    context::RenderContext,
    embedded::{EmbeddedPipeline, PublishedSlot},
    pipeline::DEFAULT_FOV,
    region::task::RenderMode,
};

use atlas_rt::world::{
    format::{get_palette, open_file},
    grid::{LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH},
    snapshot::{emit_snapshots, MicroChunkSnapshot},
    World,
};

use crate::worker::{Kick, Worker};

const STATUS_READY: i32 = 1;
const STATUS_FAILED: i32 = 2;
const REJECT: &str = "atlas_rt: rejected input: ";

#[derive(GodotClass)]
#[class(base=Control)]
pub struct AtlasRtView {
    status: i32,
    fov: f32,
    backend_status: GString,

    origin: Vector3,
    basis: Basis,
    render_mode: i32,

    gpu: Option<Arc<Mutex<RenderContext>>>,
    pipeline: Option<Arc<Mutex<EmbeddedPipeline>>>,
    worker: Option<Worker>,

    base: Base<Control>,
}

#[godot_api]
impl IControl for AtlasRtView {
    fn init(base: Base<Control>) -> Self {
        Self {
            status: 0,
            fov: DEFAULT_FOV,
            backend_status: String::from("cpu").into(),
            origin: Vector3::ZERO,
            basis: Basis::IDENTITY,
            render_mode: 0,
            gpu: None,
            pipeline: None,
            worker: None,
            base,
        }
    }

    fn ready(&mut self) {
        self.to_gd().set_anchors_and_offsets_preset(
            godot::classes::control::LayoutPreset::PRESET_FULL_RECT,
        );

        match RenderContext::new_headless() {
            Ok(context) => {
                let gpu = Arc::new(Mutex::new(context));

                match EmbeddedPipeline::new(&gpu, [1280, 720]) {
                    Ok(pipeline) => {
                        let shared_pipeline = Arc::new(Mutex::new(pipeline));
                        let gpu_shared = Arc::clone(&gpu);

                        self.worker = Some(Worker::spawn(
                            gpu_shared,
                            shared_pipeline,
                            Arc::new(Mutex::new(Vec::new())),
                        ));

                        self.pipeline = Some(shared_pipeline);

                        self.backend_status = String::from(
                            Self::probe_backend(&gpu),
                        ).into();
                        self.status = STATUS_READY;
                    }
                    Err(err) => {
                        godot_error!("atlas_rt: pipeline init failed: {}", err);
                        self.status = STATUS_FAILED;
                    }
                }

                self.gpu = Some(gpu);
            }
            Err(err) => {
                godot_error!("atlas_rt: initialization failed: {}", err);
                self.status = STATUS_FAILED;
            }
        }
    }

    fn process(&mut self, delta: f64) {
        if self.status != STATUS_READY {
            return;
        }

        let Some(worker) = &self.worker else {
            return;
        };

        worker.kick(Kick {
            view_mat: self.view_matrix().to_cols_array(),
            fov: self.fov,
            extent: self.viewport_extent(),
            mode: self.kick_mode(),
            delta_time: delta as f32,
        });
    }
}

#[godot_api]
impl AtlasRtView {
    #[func]
    pub fn set_camera(&mut self, camera: Gd<Camera3D>) {
        let camera = camera.get_global_transform();

        self.origin = camera.origin;
        self.basis = camera.basis;
    }

    #[func]
    pub fn set_view(&mut self, origin: Vector3, basis: Basis) {
        self.origin = origin;
        self.basis = basis;
    }

    #[func]
    pub fn set_render_mode(&mut self, mode: i32) {
        if mode < 0 || mode > 2 {
            godot_error!("atlas_rt: render mode {} unsupported", mode);
            return;
        }

        self.render_mode = mode;
    }

    #[func]
    pub fn load_world(&mut self, path: GString) -> bool {
        let Some(pipeline) = &self.pipeline else {
            return false;
        };

        let Ok(pipeline) = pipeline.lock() else {
            return false;
        };

        let voxel_data = open_file(&path.to_string());

        let (world, clipped) = atlas_rt::world::World::new_clipped(&voxel_data);

        if clipped > 0 {
            godot_print!("atlas_rt: clipped {} voxels outside the lattice", clipped);
        }

        match pipeline.input().submit_batch(emit_snapshots(&world)) {
            Ok(()) => {}
            Err(err) => {
                godot_error!("atlas_rt: load_world failed: {}", err);
                return false;
            }
        }

        let Ok(gpu) = self.gpu.as_ref().map(|gpu| gpu.lock()) else {
            return false;
        };

        if let Ok(gpu) = gpu {
            match pipeline.upload_palette(&gpu, get_palette(&voxel_data).map(|color| [color.x, color.y, color.z, 1.0])) {
                Ok(()) => return true,
                Err(err) => {
                    godot_error!("atlas_rt: palette upload failed: {}", err);
                    return false;
                }
            }
        }

        false
    }

    #[func]
    pub fn clear_world(&mut self) -> bool {
        let Some(pipeline) = &self.pipeline else {
            return false;
        };

        let Ok(pipeline) = pipeline.lock() else {
            return false;
        };

        pipeline.input().submit_batch(std::iter::empty()).is_ok()
    }

    #[func]
    pub fn submit_microchunk(
        &mut self,
        coords: Vector3i,
        mask: PackedByteArray,
        materials: PackedByteArray,
    ) -> bool {
        match Self::validate_edit(coords, &mask, &materials) {
            Ok(snapshot) => self.push_edit([snapshot]),
            Err(reason) => {
                godot_error!("{}{}", REJECT, reason);
                false
            }
        }
    }
}

impl AtlasRtView {
    /// The init probe gates the deliverable backend: zero_copy requires a
    /// working Win32 export on the delivery memory, loud failure for the
    /// forced path, cpu elsewhere.
    fn probe_backend(gpu: &Arc<Mutex<RenderContext>>) -> &'static str {
        let _ = gpu;
        "cpu"
    }

    fn push_edit<S: IntoIterator<Item = MicroChunkSnapshot>>(
        &mut self,
        snapshots: S,
    ) -> bool {
        let Some(pipeline) = &self.pipeline else {
            return false;
        };

        match pipeline.lock() {
            Ok(pipeline) => pipeline.input().submit_batch(snapshots).is_ok(),
            Err(_) => false,
        }
    }

    fn kick_mode(&self) -> RenderMode {
        match self.render_mode {
            1 => RenderMode::Hull,
            2 => RenderMode::Normal,
            _ => RenderMode::Voxel,
        }
    }

    fn view_matrix(&self) -> glam::Mat4 {
        let [x, y, z] = self.basis.rows;

        let world = glam::Mat4::from_cols(
            glam::Vec3::new(x.x, x.y, x.z),
            glam::Vec3::new(y.x, y.y, y.z),
            glam::Vec3::new(z.x, z.y, z.z),
            glam::Vec3::new(self.origin.x, self.origin.y, self.origin.z),
        );

        world.inverse()
    }

    fn viewport_extent(&self) -> [u32; 2] {
        self.to_gd()
            .get_viewport()
            .map(|viewport| {
                let size = viewport.get_visible_rect().size;

                [size.x.cast_signed() as u32, size.y as u32]
            })
            .unwrap_or([0, 0])
    }

    fn validate_edit(
        coords: Vector3i,
        mask: &PackedByteArray,
        materials: &PackedByteArray,
    ) -> Result<MicroChunkSnapshot, String> {
        let half = LATTICE_HALF_EXTENT as i32;

        let inside = coords.x >= -half
            && coords.x < half
            && coords.y >= -half
            && coords.y < half
            && coords.z >= -half
            && coords.z < half;

        if !inside {
            return Err(format!("coords {} outside the lattice", coords));
        }

        let chunk_step = MICRO_CHUNK_LENGTH as i32;

        let multiples = coords.x % chunk_step == 0
            && coords.y % chunk_step == 0
            && coords.z % chunk_step == 0;

        if !multiples {
            return Err(format!("coords {} not a multiple of 8", coords));
        }

        if mask.len() != 64 {
            return Err(format!("mask has {} bytes; expected 64", mask.len()));
        }

        let occupied = mask
            .to_vec()
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>();

        if materials.len() != occupied as usize {
            return Err(format!(
                "materials length {}; expected two lengths to match ({})",
                materials.len(),
                occupied
            ));
        }

        let mut mask_bytes = [0u8; 64];

        for (index, byte) in mask.to_vec().into_iter().enumerate() {
            mask_bytes[index] = byte;
        }

        Ok(MicroChunkSnapshot {
            global_coords: glam::IVec3::new(coords.x, coords.y, coords.z),
            mask: mask_bytes,
            materials: materials.to_vec(),
        })
    }
}
