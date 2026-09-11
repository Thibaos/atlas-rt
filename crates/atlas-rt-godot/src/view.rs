use std::sync::{Arc, Mutex, mpsc};

use godot::classes::{
    Camera3D, Control, Engine, FileAccess, IControl, Material, RenderingServer, ShaderMaterial,
    Texture2Drd,
};
use godot::prelude::*;

use atlas_rt::render::{
    context::RenderContext,
    delivery::{DeviceMemory, SLOT_COUNT},
    embedded::{EmbeddedPipeline, PublishedSlot, WrapTimes},
    display_gate::DisplayGate,
    pipeline::{DEFAULT_FOV, FrameInput},
    region::task::RenderMode,
};
use atlas_rt::world::{
    batch::{self, TrackedCoords},
    format::{get_palette, open_bytes},
    grid::{LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH},
    snapshot::{MicroChunkSnapshot, emit_snapshots},
};

use crate::worker::{FrameRequest, Worker, lock};

const STATUS_LOADING: i32 = 0;
const STATUS_READY: i32 = 1;
const STATUS_FAILED: i32 = 2;
const REJECT: &str = "atlas_rt: rejected input: ";
const ATLAS_MODE_UNIFORM: &str = "mode";
const ATLAS_FRAME_UNIFORM: &str = "atlas_frame";

#[derive(GodotClass)]
#[class(base=Control)]
pub struct AtlasRtView {
    status: i32,
    fov: f32,
    origin: Vector3,
    basis: Basis,
    render_mode_index: i32,

    gpu: Option<Arc<Mutex<RenderContext>>>,
    pipeline: Option<Arc<Mutex<EmbeddedPipeline>>>,
    worker: Option<Worker>,

    worker_publish_in: Option<mpsc::Receiver<PublishedSlot>>,
    display: DisplayGate,
    composite_material: Option<Gd<Material>>,
    material_detached: bool,
    wrapped_texture: Option<Gd<Texture2Drd>>,
    wrapped_at: [Option<u64>; SLOT_COUNT],
    world_chunks: TrackedCoords,
    tick: u64,
    camera: Option<Gd<Camera3D>>,

    base: Base<Control>,
}

#[godot_api]
impl IControl for AtlasRtView {
    fn init(base: Base<Control>) -> Self {
        Self {
            status: STATUS_LOADING,
            fov: DEFAULT_FOV,
            origin: Vector3::ZERO,
            basis: Basis::IDENTITY,
            render_mode_index: 0,
            gpu: None,
            pipeline: None,
            worker: None,
            worker_publish_in: None,
            display: DisplayGate::new(),
            composite_material: None,
            material_detached: false,
            wrapped_texture: None,
            wrapped_at: [None; SLOT_COUNT],
            world_chunks: TrackedCoords::default(),
            tick: 0,
            camera: None,
            base,
        }
    }

    fn ready(&mut self) {
        self.to_gd()
            .set_anchors_and_offsets_preset(godot::classes::control::LayoutPreset::FULL_RECT);

        self.composite_material = self.to_gd().get_material();

        self.match_viewport_size();

        match RenderContext::new_headless() {
            Ok(context) => {
                let gpu = Arc::new(Mutex::new(context));

                let built = {
                    let gpu_guard = lock(&gpu);
                    let extent = self.viewport_extent();

                    EmbeddedPipeline::new(
                        &gpu_guard,
                        if extent == [0, 0] {
                            [1280, 720]
                        } else {
                            extent
                        },
                    )
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

                        self.pipeline = Some(shared_pipeline.clone());
                        self.worker_publish_in = Some(published_rx);
                        self.status = STATUS_READY;

                        if let Err(probe) = Self::probe_backend(&gpu, &shared_pipeline) {
                            godot_error!("atlas_rt: init probe failed: {}", probe);
                            self.status = STATUS_FAILED;
                        } else {
                            Signal::from_object_signal(
                                &RenderingServer::singleton(),
                                "frame_post_draw",
                            )
                            .connect(&Callable::from_object_method(
                                &self.to_gd(),
                                "on_frame_post_draw",
                            ));
                        }
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

        self.sync_composite_mode();
    }

    fn process(&mut self, delta: f64) {
        if self.status != STATUS_READY {
            return;
        }

        self.match_viewport_size();
        self.sync_camera();

        let Some(worker) = &self.worker else {
            return;
        };

        worker.submit(FrameRequest {
            input: FrameInput {
                view: self.view_matrix(),
                extent: self.viewport_extent(),
                fov: self.fov,
                resized: false,
                render_mode: self.render_mode(),
                delta_time: delta as f32,
            },
            wrap_times: WrapTimes {
                wrapped_at: self.wrapped_at,
                tick: self.tick,
            },
        });
    }

    fn draw(&mut self) {
        let Some(wrapped) = &self.wrapped_texture else {
            self.to_gd().draw_rect(
                Rect2::new(Vector2::ZERO, self.to_gd().get_size()),
                Color::from_rgba(0.02, 0.02, 0.03, 1.0),
            );

            return;
        };

        self.to_gd().draw_texture_rect(
            wrapped,
            Rect2::new(Vector2::ZERO, self.to_gd().get_size()),
            false,
        );
    }
}

#[godot_api]
impl AtlasRtView {
    #[func]
    pub fn on_frame_post_draw(&mut self) {
        if self.status != STATUS_READY {
            return;
        }

        self.tick += 1;

        let Some(published_in) = &self.worker_publish_in else {
            return;
        };

        let mut newest: Option<PublishedSlot> = None;

        while let Ok(slot) = published_in.try_recv() {
            newest = Some(slot);
        }

        if let Some(PublishedSlot { slot, version, .. }) = newest
            && self.display.admits(version)
            && let Some(entry) = self.wrapped_at.get_mut(slot)
            && entry.is_none_or(|prev| prev < self.tick)
        {
            *entry = Some(self.tick);
            self.hand_off_zero_copy(slot);
            self.to_gd().queue_redraw();
        }
    }

    #[func]
    pub fn set_camera(&mut self, camera: Gd<Camera3D>) {
        self.camera = Some(camera);
        self.sync_camera();
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

        self.render_mode_index = mode;
        self.sync_composite_mode();
    }

    fn sync_composite_mode(&self) {
        let Some(material) = self.base().get_material() else {
            return;
        };

        let Ok(mut shader) = material.try_cast::<ShaderMaterial>() else {
            return;
        };

        shader.set_shader_parameter(ATLAS_MODE_UNIFORM, &self.render_mode_index.to_variant());
    }

    #[func]
    pub fn load_world(&mut self, path: GString) -> bool {
        if self.status != STATUS_READY {
            return false;
        }

        let vox_bytes = FileAccess::get_file_as_bytes(&path);

        if vox_bytes.is_empty() {
            godot_error!("atlas_rt: could not open {path}");

            return false;
        }

        let Ok(voxel_data) = open_bytes(vox_bytes.as_slice()) else {
            godot_error!("atlas_rt: could not parse {path}");

            return false;
        };

        let (world, clipped) = atlas_rt::world::World::new_clipped(&voxel_data);

        let Some(pipeline) = self.pipeline.clone() else {
            return false;
        };

        if clipped > 0 {
            godot_print!("atlas_rt: clipped {clipped} voxels outside the lattice",);
        }

        let snapshots = match emit_snapshots(&world) {
            Ok(snapshots) => snapshots,
            Err(err) => {
                godot_error!("atlas_rt: snapshot emit failed: {}", err);

                return false;
            }
        };

        let planned = batch::plan_load(snapshots, &self.world_chunks);

        if let Some(gpu_shared) = self.gpu.as_ref() {
            let gpu = lock(gpu_shared);
            let pipeline = lock(&pipeline);

            if let Err(err) = pipeline.upload_palette(
                &gpu,
                get_palette(&voxel_data).map(|color| [color.x, color.y, color.z, 1.0]),
            ) {
                godot_error!("atlas_rt: palette upload failed: {}", err);

                return false;
            }
        }

        if !self.submit_world_change(planned) {
            godot_error!("atlas_rt: load_world failed: edit queue rejected the world");

            return false;
        }

        true
    }

    #[func]
    pub fn clear_world(&mut self) -> bool {
        if self.status != STATUS_READY {
            return false;
        }

        if self.world_chunks.is_empty() {
            self.close_display();

            return true;
        }

        let planned = batch::plan_clear(&self.world_chunks);

        if !self.submit_world_change(planned) {
            godot_error!("atlas_rt: clear_world failed: edit queue rejected the clear");

            return false;
        }

        let Some(pipeline) = self.pipeline.clone() else {
            return false;
        };

        if let Err(err) = lock(&pipeline).input().wait_until_idle() {
            godot_error!("atlas_rt: clear_world failed: {}", err);

            return false;
        }

        true
    }

    #[func]
    pub fn submit_microchunk(
        &mut self,
        coords: Vector3i,
        mask: PackedByteArray,
        materials: PackedByteArray,
    ) -> bool {
        let edit = Self::validate_edit(coords, &mask, &materials);

        self.submit_edits(edit.map(|snapshot| vec![snapshot]))
    }

    #[func]
    pub fn submit_batch(&mut self, edits: Array<Variant>) -> bool {
        self.submit_edits(Self::validate_edits(&edits))
    }
}

impl AtlasRtView {
    fn sync_camera(&mut self) {
        let Some(camera) = &self.camera else {
            return;
        };

        if !camera.is_instance_valid() {
            self.camera = None;

            return;
        }

        let transform = camera.get_global_transform();
        self.origin = transform.origin;
        self.basis = transform.basis;
    }

    #[allow(clippy::needless_pass_by_ref_mut)]
    fn match_viewport_size(&mut self) {
        self.to_gd().set_size(self.to_gd().get_viewport_rect().size);
    }

    const fn render_mode(&self) -> RenderMode {
        match self.render_mode_index {
            1 => RenderMode::Hull,
            2 => RenderMode::Normal,
            _ => RenderMode::Voxel,
        }
    }

    fn view_matrix(&self) -> glam::Mat4 {
        camera_view(self.origin, self.basis)
    }

    fn viewport_extent(&self) -> [u32; 2] {
        self.to_gd().get_viewport().map_or([0, 0], |viewport| {
            let size = viewport.get_visible_rect().size;

            [size.x.max(0.0) as u32, size.y.max(0.0) as u32]
        })
    }

    /// Blanks the viewport on this turn. The control's material samples the
    /// delivery image, so it has to come off for the placeholder rect to stand
    /// in for the world.
    fn suppress_display(&mut self) {
        if self.composite_material.is_some() {
            self.to_gd().set_material(None::<&Gd<Material>>);

            self.material_detached = true;
        }

        self.wrapped_texture = None;
        self.to_gd().queue_redraw();
    }

    /// Submits a planned batch and closes the gate on the content it replaces,
    /// so the only frames admitted afterwards are ones the renderer built from
    /// the new content. Callers must hold no pipeline lock: this takes it, and
    /// taking it twice on one thread wedges the client.
    fn submit_world_change(&mut self, planned: batch::Batch) -> bool {
        if !self.apply_batch(planned) {
            return false;
        }

        self.close_display();

        true
    }

    /// Records the version the renderer is stamping on the frames it has
    /// produced so far and blanks the viewport. Callers must hold no pipeline
    /// lock.
    fn close_display(&mut self) {
        let Some(pipeline) = &self.pipeline else {
            return;
        };

        self.display.suppress(lock(pipeline).content_version());

        self.suppress_display();
    }

    fn probe_backend(
        gpu: &Arc<Mutex<RenderContext>>,
        pipeline: &Arc<Mutex<EmbeddedPipeline>>,
    ) -> Result<(), String> {
        let mut bridge = Self::bridge()?;

        let memory = {
            let pipeline_guard = lock(pipeline);

            let gpu_guard = gpu.lock().map_err(|_| String::from("gpu mutex poisoned"))?;

            pipeline_guard
                .slot_memory(&gpu_guard, 0)
                .map_err(|err| format!("slot memory failed: {err:#}"))
        }?;

        let extent = lock(pipeline).extent();

        Self::create_bridge_image(&mut bridge, SLOT_COUNT, &memory, extent)?;

        bridge.call("release_all", &[]);

        Ok(())
    }

    fn bridge() -> Result<Gd<Object>, String> {
        Engine::singleton()
            .get_singleton("VulkanHooksBridge")
            .ok_or_else(|| {
                String::from("VulkanHooksBridge singleton missing (engine module not loaded?)")
            })
    }

    fn create_bridge_image(
        bridge: &mut Gd<Object>,
        slot: usize,
        memory: &Arc<DeviceMemory>,
        extent: [u32; 2],
    ) -> Result<Rid, String> {
        let handle = atlas_rt::render::delivery::export_win32_handle(memory)
            .map_err(|error| format!("memory export failed: {error:#}"))?;

        let alloc_size = memory.allocation_size();
        let type_index = memory.memory_type_index();

        let variant = bridge.call(
            "create_image",
            &[
                Variant::from(slot as i64),
                Variant::from(handle as i64),
                Variant::from(alloc_size as i64),
                Variant::from(i64::from(type_index)),
                Variant::from(i64::from(extent[0])),
                Variant::from(i64::from(extent[1])),
            ],
        );

        let rid = variant
            .try_to::<Rid>()
            .map_err(|_| String::from("bridge returned no texture rid"))?;

        rid.is_valid()
            .then_some(rid)
            .ok_or_else(|| String::from("bridge texture rid invalid"))
    }

    fn hand_off_zero_copy(&mut self, slot: usize) {
        let Some(gpu) = &self.gpu else {
            return;
        };

        let Some(pipeline) = &self.pipeline else {
            return;
        };

        let (memory, extent) = {
            let gpu_guard = lock(gpu);
            let pipeline_guard = lock(pipeline);

            match pipeline_guard.slot_memory(&gpu_guard, slot) {
                Ok(memory) => (memory, pipeline_guard.extent()),
                Err(err) => {
                    godot_error!("atlas_rt: slot memory failed: {}", err);
                    return;
                }
            }
        };

        let Some(bridge) = Engine::singleton().get_singleton("VulkanHooksBridge") else {
            godot_error!(
                "atlas_rt: VulkanHooksBridge singleton missing (engine module not loaded?)"
            );
            return;
        };

        let mut bridge = bridge;

        let rid = match Self::create_bridge_image(&mut bridge, slot, &memory, extent) {
            Ok(rid) => rid,
            Err(err) => {
                godot_error!("atlas_rt: {err}");

                return;
            }
        };

        let wrapped = self.wrapped_texture.get_or_insert_with(Texture2Drd::new_gd);

        wrapped.set_texture_rd_rid(rid);

        let frame = wrapped.clone();

        self.restore_control_material();
        self.sync_composite_frame(&frame);
    }

    fn restore_control_material(&mut self) {
        if !self.material_detached {
            return;
        }

        self.material_detached = false;

        if let Some(material) = self.composite_material.clone() {
            self.to_gd().set_material(Some(&material));
        }
    }

    fn sync_composite_frame(&self, frame: &Gd<Texture2Drd>) {
        let Some(material) = self.base().get_material() else {
            return;
        };

        let Ok(mut shader) = material.try_cast::<ShaderMaterial>() else {
            return;
        };

        shader.set_shader_parameter(ATLAS_FRAME_UNIFORM, &frame.to_variant());
    }

    /// Plans validated edits into one batch. A rejected boundary read is logged
    /// here, once.
    fn submit_edits(&mut self, edits: Result<Vec<MicroChunkSnapshot>, String>) -> bool {
        match edits {
            Ok(snapshots) => self.apply_batch(batch::plan_edit(snapshots, &self.world_chunks)),
            Err(reason) => {
                godot_error!("{}{}", REJECT, reason);
                false
            }
        }
    }

    /// Callers must hold no pipeline lock: this takes it, and taking it twice on
    /// one thread wedges the client.
    fn apply_batch(&mut self, planned: batch::Batch) -> bool {
        let Some(pipeline) = &self.pipeline else {
            return false;
        };

        let submitted = lock(pipeline)
            .input()
            .submit_batch(planned.snapshots)
            .is_ok();

        if submitted {
            self.world_chunks = planned.tracked;
        }

        submitted
    }

    fn validate_edits(edits: &Array<Variant>) -> Result<Vec<MicroChunkSnapshot>, String> {
        let mut snapshots = Vec::with_capacity(edits.len());

        for (index, edit) in edits.iter_shared().enumerate() {
            let fields = edit
                .try_to::<VarDictionary>()
                .map_err(|_| format!("edit {index} is not a Dictionary"))?;

            let coords = Self::read_field::<Vector3i>(&fields, index, "coords")?;
            let mask = Self::read_field::<PackedByteArray>(&fields, index, "mask")?;
            let materials = Self::read_field::<PackedByteArray>(&fields, index, "materials")?;

            snapshots.push(Self::validate_edit(coords, &mask, &materials)?);
        }

        Ok(snapshots)
    }

    fn read_field<T: FromGodot>(
        fields: &VarDictionary,
        index: usize,
        name: &str,
    ) -> Result<T, String> {
        fields
            .get(name)
            .ok_or_else(|| format!("edit {index} has no {name}"))?
            .try_to::<T>()
            .map_err(|_| format!("edit {index} field {name} has the wrong type"))
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
        let multiples =
            coords.x % chunk_step == 0 && coords.y % chunk_step == 0 && coords.z % chunk_step == 0;

        if !multiples {
            return Err(format!("coords {coords} not a multiple of 8"));
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

#[must_use]
pub fn camera_view(origin: Vector3, basis: Basis) -> glam::Mat4 {
    let [basis_x, basis_y, basis_z] = basis.rows;

    let world = glam::Mat4::from_cols_array_2d(&[
        [basis_x.x, basis_y.x, basis_z.x, 0.0],
        [basis_x.y, basis_y.y, basis_z.y, 0.0],
        [basis_x.z, basis_y.z, basis_z.z, 0.0],
        [origin.x, origin.y, origin.z, 1.0],
    ]);

    let forward_flip = glam::Mat4::from_scale(glam::Vec3::new(1.0, 1.0, -1.0));

    forward_flip * world.inverse()
}

#[cfg(test)]
mod tests {
    use super::camera_view;
    use godot::prelude::*;

    fn near(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 1.0e-4
    }

    fn basis_axes(basis: Basis) -> [glam::Vec3; 3] {
        let [x, y, z] = basis.rows;

        [
            glam::Vec3::new(x.x, y.x, z.x),
            glam::Vec3::new(x.y, y.y, z.y),
            glam::Vec3::new(x.z, y.z, z.z),
        ]
    }

    #[test]
    fn the_identity_camera_faces_neg_z() {
        let view = camera_view(Vector3::ZERO, Basis::IDENTITY).inverse();
        let forward = view.transform_vector3(glam::Vec3::Z);

        assert!(near(forward.z, -1.0));
    }

    #[test]
    fn the_camera_rays_follow_the_godot_facing() {
        let origin = Vector3::new(-2.0, 5.0, 7.0);
        let basis = Basis::IDENTITY
            .rotated(Vector3::UP, 0.9)
            .rotated(Vector3::RIGHT, -0.4);

        let view_inverse = camera_view(origin, basis).inverse();
        let [axis_x, axis_y, axis_z] = basis_axes(basis);

        let forward = view_inverse.transform_vector3(glam::Vec3::Z);
        assert!(near(forward.x, -axis_z.x) && near(forward.z, -axis_z.z));

        let right = view_inverse.transform_vector3(glam::Vec3::X);
        assert!(near(right.x, axis_x.x) && near(right.z, axis_x.z));

        let up = view_inverse.transform_vector3(glam::Vec3::Y);
        assert!(near(up.y, axis_y.y));
    }

    #[test]
    fn the_view_keeps_the_camera_pose() {
        let origin = Vector3::new(3.0, 8.0, 40.0);
        let basis = Basis::IDENTITY.rotated(Vector3::UP, 0.7);

        let view_inverse = camera_view(origin, basis).inverse();

        assert!(near(view_inverse.w_axis.x, origin.x));
        assert!(near(view_inverse.w_axis.y, origin.y));
        assert!(near(view_inverse.w_axis.z, origin.z));
        assert!(near(view_inverse.w_axis.w, 1.0));
    }
}
