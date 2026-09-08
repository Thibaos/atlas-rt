//! The view node: main-thread coordinator for the embedded pipeline.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::uninlined_format_args,
    clippy::map_unwrap_or,
    clippy::arithmetic_side_effects,
    clippy::needless_pass_by_value,
    clippy::missing_const_for_fn,
    clippy::option_if_let_else,
)]

use std::sync::{Arc, Mutex, mpsc};

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
};

use crate::worker::{Kick, Worker, lock};

const STATUS_LOADING: i32 = 0;
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
    worker_publish_in: Option<mpsc::Receiver<PublishedSlot>>,

    base: Base<Control>,
}

#[godot_api]
impl IControl for AtlasRtView {
    fn init(base: Base<Control>) -> Self {
        Self {
            status: STATUS_LOADING,
            fov: DEFAULT_FOV,
            backend_status: GString::from("cpu"),
            origin: Vector3::ZERO,
            basis: Basis::IDENTITY,
            render_mode: 0,
            gpu: None,
            pipeline: None,
            worker: None,
            worker_publish_in: None,
            base,
        }
    }

    fn ready(&mut self) {
        self.to_gd().set_anchors_and_offsets_preset(
            godot::classes::control::LayoutPreset::FULL_RECT,
        );

        match RenderContext::new_headless() {
            Ok(context) => {
                let gpu = Arc::new(Mutex::new(context));

                let built = {
                    let gpu_guard = crate::worker::lock(&gpu);
                    EmbeddedPipeline::new(&gpu_guard, [1280, 720])
                };

                match built {
                    Ok(pipeline) => {
                        let shared_pipeline = Arc::new(Mutex::new(pipeline));

                        let (published_tx, published_rx) = mpsc::channel();

                        self.worker = Some(Worker::spawn(
                            Arc::clone(&gpu),
                            shared_pipeline.clone(),
                            published_tx,
                        ));

                        self.pipeline = Some(shared_pipeline);
                        self.worker_publish_in = Some(published_rx);
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

        if let Some(worker) = &self.worker {
            worker.kick(Kick {
                view_mat: self.view_matrix().to_cols_array(),
                fov: self.fov,
                extent: self.viewport_extent(),
                mode: self.kick_mode(),
                delta_time: (delta as f32)
            });
        }
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
        if !(0..=2).contains(&mode) {
            godot_error!("atlas_rt: render mode {mode} unsupported");
            return;
        }

        self.render_mode = mode;
    }

    #[func]
    pub fn load_world(&mut self, path: GString) -> bool {
        if self.status != STATUS_READY {
            return false;
        }

        let Some(pipeline) = &self.pipeline else {
            return false;
        };

        let pipeline = lock(pipeline);

        let voxel_data = open_file(&path.to_string());

        let (world, clipped) = atlas_rt::world::World::new_clipped(&voxel_data);

        if clipped > 0 {
            godot_print!("atlas_rt: clipped {clipped} voxels outside the lattice",);
        }

        match emit_snapshots(&world) {
            Ok(snapshots) => {
                if let Err(err) = pipeline.input().submit_batch(snapshots) {
                    godot_error!("atlas_rt: load_world failed: {}", err);

                    return false;
                }
            }
            Err(err) => {
                godot_error!("atlas_rt: snapshot emit failed: {}", err);

                return false;
            }
        }

        if let Some(gpu_shared) = self.gpu.as_ref() {
            let gpu = lock(gpu_shared);

            match pipeline.upload_palette(
                &gpu,
                get_palette(&voxel_data).map(|color| [color.x, color.y, color.z, 1.0]),
            ) {
                Ok(()) => {}
                Err(err) => {
                    godot_error!("atlas_rt: palette upload failed: {}", err);
                    return false;
                }
            }
        }

        true
    }

    #[func]
    pub fn clear_world(&mut self) -> bool {
        self.status == STATUS_READY
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
    fn kick_mode(&self) -> RenderMode {
        match self.render_mode {
            1 => RenderMode::Hull,
            2 => RenderMode::Normal,
            _ => RenderMode::Voxel,
        }
    }

    fn view_matrix(&self) -> glam::Mat4 {
        let [x, y, z] = self.basis.rows;

        let world = glam::Mat4::from_cols_array_2d(&[
            [x.x, y.x, z.x, self.origin.x],
            [x.y, y.y, z.y, self.origin.y],
            [x.z, y.z, z.z, self.origin.z],
            [0.0, 0.0, 0.0, 1.0],
        ]);

        world.inverse()
    }

    fn viewport_extent(&self) -> [u32; 2] {
        self.to_gd()
            .get_viewport()
            .map_or([0, 0], |viewport| {
                let size = viewport.get_visible_rect().size;

                [size.x.max(0.0) as u32, size.y.max(0.0) as u32]
            })
    }

    fn push_edit(
        &self,
        snapshots: impl IntoIterator<Item = MicroChunkSnapshot>,
    ) -> bool {
        let Some(pipeline) = &self.pipeline else {
            return false;
        };

        lock(pipeline).input().submit_batch(snapshots).is_ok()
    }

    fn validate_edit(
        coords: Vector3i,
        mask: &PackedByteArray,
        materials: &PackedByteArray,
    ) -> Result<MicroChunkSnapshot, String> {
        let half = LATTICE_HALF_EXTENT.cast_signed();
        let inside = coords.x >= -half
            && coords.x < half
            && coords.y >= -half
            && coords.y < half
            && coords.z >= -half
            && coords.z < half;

        if !inside {
            return Err(format!("coords {coords} outside the lattice"));
        }

        let chunk_step = MICRO_CHUNK_LENGTH.cast_signed();
        let multiples = coords.x % chunk_step == 0
            && coords.y % chunk_step == 0
            && coords.z % chunk_step == 0;

        if !multiples {
            return Err(format!("coords {coords} not a multiple of 8"));
        }

        if mask.len() != 64 {
            return Err(format!(
                "mask has {} bytes; expected 64",
                mask.len()
            ));
        }

        let occupied = mask
            .to_vec()
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>();

        if materials.len() != occupied as usize {
            return Err(format!(
                "materials length {} does not match mask popcount {occupied}",
                materials.len()
            ));
        }

        let mut mask_bytes = [0u8; 64];

        for (index, byte) in mask.to_vec().iter().copied().enumerate() {
            if let Some(entry) = mask_bytes.get_mut(index) {
                *entry = byte;
            }
        }

        Ok(MicroChunkSnapshot {
            global_coords: glam::IVec3::new(coords.x, coords.y, coords.z),
            mask: mask_bytes,
            materials: materials.to_vec(),
        })
    }
}
