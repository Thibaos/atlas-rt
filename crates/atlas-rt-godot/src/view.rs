use std::sync::{Arc, Mutex, mpsc};

use godot::classes::{
    Camera3D, Control, Engine, IControl, Material, ProjectSettings, RenderingServer,
    ShaderMaterial, Texture2Drd,
};
use godot::prelude::*;

use atlas_rt::render::{
    context::RenderContext,
    delivery::{DeviceMemory, SLOT_COUNT},
    display_gate::DisplayGate,
    embedded::{EmbeddedPipeline, PublishedSlot, WrapTimes},
    pipeline::{DEFAULT_FOV, FrameInput},
    region::task::RenderMode,
};
use atlas_rt::world::{
    batch::{self, TrackedCoords},
    grid::{LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH},
    job::{Finished, Refusal, Residency, Status, WorldJob, WorldSource},
    snapshot::MicroChunkSnapshot,
};

use crate::worker::{FrameRequest, Worker, lock};

const REJECT: &str = "atlas_rt: rejected input: ";
const ATLAS_MODE_UNIFORM: &str = "mode";
const ATLAS_FRAME_UNIFORM: &str = "atlas_frame";

#[derive(GodotClass)]
#[class(base=Control)]
pub struct AtlasRtView {
    fov: f32,
    origin: Vector3,
    basis: Basis,
    render_mode_index: i32,

    gpu: Option<Arc<Mutex<RenderContext>>>,
    pipeline: Option<Arc<Mutex<EmbeddedPipeline>>>,
    worker: Option<Worker>,
    job: Option<WorldJob>,

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
            fov: DEFAULT_FOV,
            origin: Vector3::ZERO,
            basis: Basis::IDENTITY,
            render_mode_index: 0,
            gpu: None,
            pipeline: None,
            worker: None,
            job: None,
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

                        let mut job = WorldJob::new();
                        job.world_resident();

                        self.pipeline = Some(shared_pipeline.clone());
                        self.worker_publish_in = Some(published_rx);
                        self.job = Some(job);

                        if let Err(probe) = Self::probe_backend(&gpu, &shared_pipeline) {
                            godot_error!("atlas_rt: init probe failed: {}", probe);
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
                    }
                }

                self.gpu = Some(gpu);
            }
            Err(err) => {
                godot_error!("atlas_rt: initialization failed: {}", err);
            }
        }

        self.sync_composite_mode();
    }

    fn process(&mut self, delta: f64) {
        self.poll_job();

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
        self.poll_job();

        self.tick += 1;

        let Some(published_in) = &self.worker_publish_in else {
            return;
        };

        let mut newest: Option<PublishedSlot> = None;

        while let Ok(slot) = published_in.try_recv() {
            newest = Some(slot);
        }

        let Some(PublishedSlot {
            slot,
            version,
            generation,
            ..
        }) = newest
        else {
            return;
        };

        let Some(entry) = self.wrapped_at.get_mut(slot) else {
            return;
        };

        if !self.display.admits(version) || !entry.is_none_or(|prev| prev < self.tick) {
            return;
        }

        *entry = Some(self.tick);
        self.hand_off_zero_copy(slot);
        self.settle_job(version, generation);
        self.to_gd().queue_redraw();
    }

    /// The status the loading overlay and the load buttons read: no world and
    /// nothing in flight, a job in flight, a world resident, or a failure. A
    /// view whose pipeline never came up has no world and no job, which is a
    /// failure.
    #[func]
    pub fn job_status(&self) -> i32 {
        self.job
            .as_ref()
            .map_or(Status::Failed, WorldJob::status)
            .code()
            .into()
    }

    /// The status spelled out, so a host reads the state by name rather than by
    /// code.
    #[func]
    pub fn job_status_name(&self) -> GString {
        GString::from(
            self.job
                .as_ref()
                .map_or(Status::Failed, WorldJob::status)
                .name(),
        )
    }

    /// Why the last job failed, for display. Empty while the last job did not
    /// fail.
    #[func]
    pub fn job_error(&self) -> GString {
        self.job
            .as_ref()
            .and_then(WorldJob::error)
            .map_or_else(GString::new, |reason| GString::from(reason.as_str()))
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

    /// Returns at once. The read, the parse, the world build, and snapshot
    /// emission run on a thread with no renderer access, and the finished
    /// snapshots reach the renderer a few frames later. The old world stops
    /// being displayed on this call.
    #[func]
    pub fn load_world(&mut self, path: GString) -> bool {
        if self.job.is_none() {
            return false;
        }

        let Some(job) = self.job.as_mut() else {
            return false;
        };

        let version = Self::batch_version(&self.pipeline);

        if let Err(refusal) = job.load(Self::source(&path), version) {
            Self::report_refusal("load_world", refusal);

            return false;
        }

        self.display.suppress(version);
        self.suppress_display();

        true
    }

    #[func]
    pub fn clear_world(&mut self) -> bool {
        if self.job.is_none() {
            return false;
        }

        let version = Self::batch_version(&self.pipeline);

        let accepted = match self.job.as_mut() {
            Some(job) => match job.clear(version) {
                Ok(()) => true,
                Err(refusal) => {
                    Self::report_refusal("clear_world", refusal);
                    false
                }
            },
            None => false,
        };

        if accepted {
            self.display.suppress(version);
            self.suppress_display();
        }

        accepted
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

    /// Blanks the viewport on this turn. The view's material samples the
    /// delivery image, so it has to come off for the placeholder to stand in for
    /// the world.
    fn suppress_display(&mut self) {
        if self.material_detached {
            return;
        }

        self.composite_material = self.base().get_material();
        self.material_detached = true;

        if self.composite_material.is_some() {
            self.to_gd().set_material(None::<&Gd<Material>>);
        }

        self.wrapped_texture = None;
        self.to_gd().queue_redraw();
    }

    /// Reads a world file off the Godot path scheme. The background thread
    /// cannot reach Godot, so `res://` and `user://` are resolved to a real
    /// filesystem path here, on the main thread, before the job starts.
    fn source(path: &GString) -> Box<dyn WorldSource> {
        Box::new(VoxFile {
            path: ProjectSettings::singleton()
                .globalize_path(path)
                .to_string(),
            name: path.to_string(),
        })
    }

    /// The version the frames produced from now on carry. Recorded as the host
    /// asks for a world to go away. Callers must hold no pipeline lock.
    fn batch_version(pipeline: &Option<Arc<Mutex<EmbeddedPipeline>>>) -> u64 {
        pipeline
            .as_ref()
            .map_or(0, |pipeline| lock(pipeline).batch_version())
    }

    /// A refusal is the in-flight guard doing its job against a direct call or
    /// a stale press, so it is reported and changes nothing.
    fn report_refusal(entry: &str, refusal: Refusal) {
        match refusal {
            Refusal::Busy => {
                godot_error!("atlas_rt: {entry} refused: a job is already in flight");
            }
            Refusal::Failed(reason) => {
                godot_error!("atlas_rt: {entry} refused: {reason}");
            }
        }
    }

    /// Takes a finished job's work on the main thread: plans the ordered batch,
    /// uploads the palette, submits it, and holds the display back until the
    /// renderer has taken it.
    fn poll_job(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };

        match job.poll() {
            Some(Finished::Loaded) => self.plan_load(),
            Some(Finished::Cleared) => self.plan_clear(),
            Some(Finished::Failed) | None => {}
        }
    }

    /// Clears for the outgoing world ahead of the incoming snapshots, uploads
    /// the palette, and submits the whole thing as one batch.
    fn plan_load(&mut self) {
        let Some(loaded) = self.job.as_ref().and_then(WorldJob::take_loaded) else {
            return;
        };

        let planned = batch::plan_load(loaded.snapshots, &self.world_chunks);

        if let Err(err) = self.upload_palette(loaded.palette) {
            self.fail_job(format!("palette upload failed: {err}"));

            return;
        }

        if !self.submit_world_change(planned) {
            self.fail_job(String::from("the edit queue rejected the world"));
        }
    }

    /// A clear of a world that is already gone leaves the job complete and the
    /// view with nothing to show.
    fn plan_clear(&mut self) {
        if self.world_chunks.is_empty() {
            if let Some(job) = self.job.as_mut() {
                job.no_world();
            }

            return;
        }

        let planned = batch::plan_clear(&self.world_chunks);

        if !self.submit_world_change(planned) {
            self.fail_job(String::from("the edit queue rejected the clear"));
        }
    }

    /// Gives up on the load in flight, for a failure the background thread
    /// cannot see.
    fn fail_job(&mut self, reason: String) {
        godot_error!("atlas_rt: load failed: {reason}");

        if let Some(job) = self.job.as_mut() {
            job.fail(reason);
        }
    }

    fn upload_palette(&self, palette: [glam::Vec3; 256]) -> Result<(), String> {
        let (Some(gpu), Some(pipeline)) = (&self.gpu, &self.pipeline) else {
            return Ok(());
        };

        let gpu = lock(gpu);

        lock(pipeline)
            .upload_palette(&gpu, palette.map(|color| [color.x, color.y, color.z, 1.0]))
            .map_err(|error| format!("{error:#}"))
    }

    /// Submits a planned batch and records where the renderer has to get to for
    /// a frame to carry it. Callers must hold no pipeline lock: this takes it,
    /// and taking it twice on one thread wedges the client.
    fn submit_world_change(&mut self, planned: batch::Batch) -> bool {
        let Some(pipeline) = &self.pipeline else {
            return false;
        };

        let (residency, version) = {
            let pipeline = lock(pipeline);

            if pipeline.input().submit_batch(planned.snapshots).is_err() {
                return false;
            }

            let generation = pipeline.applied_generation();

            (Residency::new(generation), pipeline.batch_version())
        };

        self.world_chunks = planned.tracked;
        self.display.suppress(version);

        if let Some(job) = &self.job {
            job.record(residency);
        }

        true
    }

    /// Completes the job on the frame the renderer built after taking its
    /// batch, which is the frame the world it asked for is resident in. Only
    /// then does the status say a world is resident.
    fn settle_job(&mut self, version: u64, generation: u64) {
        let settled = self
            .job
            .as_ref()
            .is_some_and(|job| job.resident(generation) && job.admitted(version));

        if settled && let Some(job) = self.job.as_mut() {
            job.world_resident();
        }
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
/// A `Basis` stores matrix rows, so the camera's axes are one component from
/// each row: row 0 holds the x of all three axes, and so on. Godot's camera
/// looks along its local -Z, so its forward is the negated third axis.
///
/// Godot's basis is right handed and the world the renderer holds is left
/// handed, so the basis is mirrored on world x. Without that mirror the scene
/// reads flipped left to right against the standalone app, which is the
/// reference for how a world is meant to look.
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

#[cfg(test)]
mod tests {
    use super::camera_view;
    use godot::prelude::*;

    fn near(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 1.0e-4
    }

    fn screen_x(view: glam::Mat4, point: glam::Vec3) -> f32 {
        view.transform_point3(point).x
    }

    #[test]
    fn the_identity_camera_faces_neg_z() {
        let view = camera_view(Vector3::ZERO, Basis::IDENTITY).inverse();
        let forward = view.transform_vector3(glam::Vec3::Z);

        assert!(near(forward.z, -1.0));
    }

    #[test]
    fn the_identity_camera_rays_run_right_to_left_along_world_x() {
        let view = camera_view(Vector3::new(0.0, 300.0, 500.0), Basis::IDENTITY);

        assert!(screen_x(view, glam::Vec3::new(100.0, 300.0, 400.0)) < 0.0);
    }

    #[test]
    fn the_camera_rays_follow_the_godot_facing() {
        let origin = Vector3::new(-2.0, 5.0, 7.0);
        let basis = Basis::IDENTITY
            .rotated(Vector3::UP, 0.9)
            .rotated(Vector3::RIGHT, -0.4);

        let view_inverse = camera_view(origin, basis).inverse();
        let forward = view_inverse.transform_vector3(glam::Vec3::Z);
        let godot_forward = -glam::Vec3::new(
            basis.col_c().x.into(),
            basis.col_c().y.into(),
            basis.col_c().z.into(),
        );

        assert!(near(forward.x, godot_forward.x) && near(forward.z, godot_forward.z));

        let godot_up = glam::Vec3::new(
            basis.col_b().x.into(),
            basis.col_b().y.into(),
            basis.col_b().z.into(),
        );
        let up = view_inverse.transform_vector3(glam::Vec3::Y);

        assert!(near(up.y, godot_up.y));
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
