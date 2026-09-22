use std::sync::{Arc, Mutex};

use atlas_rt::world::update::batch::{self};
use atlas_rt::world::update::edit::{VoxelChange, VoxelEdit, edit_world};
use atlas_rt::world::update::job::{Finished, Refusal, Residency, WorldSource, WorldUpdateJob};
use godot::classes::{Engine, Material, ProjectSettings, ShaderMaterial, Texture2Drd};
use godot::prelude::*;

use atlas_rt::render::{
    context::RenderContext,
    embedded::EmbeddedPipeline,
    image::delivery::{DeviceMemory, SLOT_COUNT},
    pipeline::task::RenderMode,
};
use atlas_rt::world::World;
use atlas_rt::world::grid::{LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH};
use glam::IVec3;

use crate::view::api::AtlasRtView;
use crate::view::{ATLAS_FRAME_UNIFORM, REJECT, VoxFile, camera_view};
use crate::worker::lock;

impl AtlasRtView {
    pub(super) fn sync_camera(&mut self) {
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

    pub(super) fn match_viewport_size(&mut self) {
        self.to_gd().set_size(self.to_gd().get_viewport_rect().size);
    }

    pub(super) const fn render_mode(&self) -> RenderMode {
        match self.render_mode_index {
            1 => RenderMode::Hull,
            2 => RenderMode::Normal,
            _ => RenderMode::Voxel,
        }
    }

    pub(super) fn view_matrix(&self) -> glam::Mat4 {
        camera_view(self.origin, self.basis)
    }

    pub(super) fn viewport_extent(&self) -> [u32; 2] {
        self.to_gd().get_viewport().map_or([0, 0], |viewport| {
            let size = viewport.get_visible_rect().size;

            [size.x.max(0.0) as u32, size.y.max(0.0) as u32]
        })
    }

    /// Blanks the viewport on this turn. Detaches the material, which samples
    /// the delivery image, so the placeholder can replace the world.
    pub(super) fn suppress_display(&mut self) {
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
    pub(super) fn source(path: &GString) -> Box<dyn WorldSource> {
        Box::new(VoxFile {
            path: ProjectSettings::singleton()
                .globalize_path(path)
                .to_string(),
            name: path.to_string(),
        })
    }

    /// The version assigned to new frames, recorded when the host requests a
    /// world change. Callers must not hold the pipeline lock.
    pub(super) fn batch_version(pipeline: &Option<Arc<Mutex<EmbeddedPipeline>>>) -> u64 {
        pipeline
            .as_ref()
            .map_or(0, |pipeline| lock(pipeline).batch_version())
    }

    /// Reports a refused direct call or stale button press without changing state.
    pub(super) fn report_refusal(entry: &str, refusal: Refusal) {
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
    pub(super) fn poll_job(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };

        match job.poll() {
            Some(Finished::Loaded) => self.plan_load(),
            Some(Finished::Cleared) => self.plan_clear(),
            Some(Finished::Failed) | None => {}
        }
    }

    /// Plans the finished load's batch, uploads the palette, submits it, and
    /// stores the world the batch describes.
    fn plan_load(&mut self) {
        let Some(loaded) = self.job.as_ref().and_then(WorldUpdateJob::take_loaded) else {
            return;
        };

        let planned = batch::plan_load(loaded.snapshots, &self.world_chunks);

        if let Err(err) = self.upload_palette(loaded.palette) {
            self.fail_job(format!("palette upload failed: {err}"));

            return;
        }

        if !self.submit_world_change(planned) {
            self.fail_job(String::from("the edit queue rejected the world"));

            return;
        }

        self.world = Some(loaded.world);
    }

    /// A clear completes into empty-and-idle and drops the stored world.
    fn plan_clear(&mut self) {
        if self.world_chunks.is_empty() {
            self.world = None;

            if let Some(job) = self.job.as_mut() {
                job.no_world();
            }

            return;
        }

        let planned = batch::plan_clear(&self.world_chunks);

        if !self.submit_world_change(planned) {
            self.fail_job(String::from("the edit queue rejected the clear"));

            return;
        }

        self.world = None;
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

    /// Submits a planned batch and records the renderer generation required for
    /// a frame to include it. Callers must not hold the pipeline lock. This
    /// method acquires it, and reacquiring it on the same thread deadlocks.
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
            job.taken();
        }

        true
    }

    /// Completes the job when an admitted frame includes its batch. Only then
    /// does the status report the resulting world state and progress reach 1.
    pub(super) fn settle_job(&mut self, version: u64, generation: u64) {
        let settled = self
            .job
            .as_ref()
            .is_some_and(|job| job.resident(generation) && job.admitted(version));

        if settled && let Some(job) = self.job.as_mut() {
            job.arrive();
        }
    }

    pub(super) fn probe_backend(
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
        let handle = atlas_rt::render::image::delivery::export_win32_handle(memory)
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

    pub(super) fn hand_off_zero_copy(&mut self, slot: usize) {
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

    /// Diffs validated chunks against the stored world, applies the edit
    /// primitive, and submits its batch. Does not suppress the display or touch
    /// the job. Refuses without a world or without a pipeline before mutating.
    pub(super) fn submit_edits(&mut self, edits: Result<Vec<ValidatedChunk>, String>) -> bool {
        let chunks = match edits {
            Ok(chunks) => chunks,
            Err(reason) => {
                godot_error!("{}{}", REJECT, reason);

                return false;
            }
        };

        if self.pipeline.is_none() {
            return false;
        }

        let Some(world) = &mut self.world else {
            godot_error!("{}{}", REJECT, "no world is loaded");

            return false;
        };

        let voxel_edits: Vec<VoxelEdit> = chunks
            .iter()
            .flat_map(|chunk| chunk_edits(world, chunk))
            .collect();

        let planned = match edit_world(world, &voxel_edits, &self.world_chunks) {
            Ok(planned) => planned,
            Err(err) => {
                godot_error!("{}{}", REJECT, err);

                return false;
            }
        };

        self.apply_batch(planned)
    }

    /// Callers must not hold the pipeline lock. This method acquires it, and
    /// reacquiring it on the same thread deadlocks.
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

    pub(super) fn validate_edits(edits: &Array<Variant>) -> Result<Vec<ValidatedChunk>, String> {
        let mut chunks = Vec::with_capacity(edits.len());

        for (index, edit) in edits.iter_shared().enumerate() {
            let fields = edit
                .try_to::<VarDictionary>()
                .map_err(|_| format!("edit {index} is not a Dictionary"))?;

            let coords = Self::read_field::<Vector3i>(&fields, index, "coords")?;
            let mask = Self::read_field::<PackedByteArray>(&fields, index, "mask")?;
            let materials = Self::read_field::<PackedByteArray>(&fields, index, "materials")?;

            chunks.push(Self::validate_edit(coords, &mask, &materials)?);
        }

        Ok(chunks)
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

    pub(super) fn validate_edit(
        coords: Vector3i,
        mask: &PackedByteArray,
        materials: &PackedByteArray,
    ) -> Result<ValidatedChunk, String> {
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

        if mask.len() != MASK_BYTES {
            return Err(format!(
                "mask has {} bytes; expected {MASK_BYTES}",
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

        let mut mask_bytes = [0u8; MASK_BYTES];

        for (index, byte) in mask.to_vec().iter().copied().enumerate() {
            if let Some(entry) = mask_bytes.get_mut(index) {
                *entry = byte;
            }
        }

        Ok(ValidatedChunk {
            origin: glam::IVec3::new(coords.x, coords.y, coords.z),
            mask: mask_bytes,
            materials: materials.to_vec(),
        })
    }
}

/// A Micro-chunk that passed the GDScript boundary checks, ready to diff.
/// Only the edit primitive builds Snapshots.
pub(super) struct ValidatedChunk {
    origin: IVec3,
    mask: [u8; MASK_BYTES],
    materials: Vec<u8>,
}

const MICRO_EDGE: usize = MICRO_CHUNK_LENGTH as usize;
const MICRO_AREA: usize = MICRO_EDGE * MICRO_EDGE;
const MICRO_CELLS: usize = MICRO_EDGE * MICRO_AREA;
const MASK_BYTES: usize = MICRO_CELLS / 8;

/// The Set and Clear edits that turn `world`'s copy of `chunk` into the
/// incoming mask and materials. Cells walk in ascending index order;
/// `materials` is consumed only for set bits, matching Snapshot packing.
fn chunk_edits(world: &World, chunk: &ValidatedChunk) -> Vec<VoxelEdit> {
    let ValidatedChunk {
        origin,
        mask,
        materials,
    } = chunk;
    let mut edits = Vec::new();
    let mut next_material = 0;

    for index in 0..MICRO_CELLS {
        let offset = IVec3::new(
            i32::try_from(index % MICRO_EDGE).unwrap_or(0),
            i32::try_from((index / MICRO_EDGE) % MICRO_EDGE).unwrap_or(0),
            i32::try_from(index / MICRO_AREA).unwrap_or(0),
        );
        let position = origin.saturating_add(offset);

        let occupied = mask
            .get(index / MICRO_EDGE)
            .is_some_and(|byte| byte & (1u8 << (index % MICRO_EDGE)) != 0);

        let incoming = if occupied {
            let material = materials.get(next_material).copied();
            next_material = next_material.saturating_add(1);
            material
        } else {
            None
        };

        let current = world
            .get_voxel(&position)
            .and_then(|voxel| u8::try_from(*voxel).ok());

        match (incoming, current) {
            (Some(material), Some(existing)) if material == existing => {}
            (Some(material), _) => edits.push(VoxelEdit {
                position,
                change: VoxelChange::Set(material),
            }),
            (None, Some(_)) => edits.push(VoxelEdit {
                position,
                change: VoxelChange::Clear,
            }),
            (None, None) => {}
        }
    }

    edits
}

#[cfg(test)]
mod tests {
    use atlas_rt::world::{
        World,
        update::{
            batch::TrackedCoords,
            edit::{VoxelChange, VoxelEdit, edit_world},
        },
    };
    use glam::IVec3;

    use super::{MASK_BYTES, ValidatedChunk, chunk_edits};

    const ORIGIN: IVec3 = IVec3::ZERO;

    fn chunk(origin: IVec3, mask: [u8; MASK_BYTES], materials: Vec<u8>) -> ValidatedChunk {
        ValidatedChunk {
            origin,
            mask,
            materials,
        }
    }

    fn mask_for(indices: &[u32]) -> [u8; MASK_BYTES] {
        let mut mask = [0u8; MASK_BYTES];

        for index in indices {
            if let Some(byte) = mask.get_mut((*index / 8) as usize) {
                *byte |= 1u8 << (index % 8);
            }
        }

        mask
    }

    fn cell_position(index: u32) -> IVec3 {
        IVec3::new(
            (index % 8) as i32,
            ((index / 8) % 8) as i32,
            (index / 64) as i32,
        )
    }

    fn set_edit(position: IVec3, material: u8) -> VoxelEdit {
        VoxelEdit {
            position,
            change: VoxelChange::Set(material),
        }
    }

    fn world_at(origin: IVec3, cells: &[(u32, u8)]) -> World {
        let mut world = World::default();
        let edits: Vec<VoxelEdit> = cells
            .iter()
            .map(|(index, material)| set_edit(origin + cell_position(*index), *material))
            .collect();

        if edit_world(&mut world, &edits, &TrackedCoords::default()).is_err() {
            panic!("the fixture edit must apply");
        }

        world
    }

    fn world_with(cells: &[(u32, u8)]) -> World {
        world_at(ORIGIN, cells)
    }

    #[test]
    fn a_zero_mask_clears_every_occupied_cell() {
        let world = world_with(&[(0, 3), (7, 4), (511, 5)]);

        let edits = chunk_edits(&world, &chunk(ORIGIN, [0u8; MASK_BYTES], Vec::new()));

        assert_eq!(
            edits,
            vec![
                VoxelEdit {
                    position: cell_position(0),
                    change: VoxelChange::Clear,
                },
                VoxelEdit {
                    position: cell_position(7),
                    change: VoxelChange::Clear,
                },
                VoxelEdit {
                    position: cell_position(511),
                    change: VoxelChange::Clear,
                },
            ],
            "cell index order, one clear per occupied cell"
        );
    }

    #[test]
    fn a_chunk_identical_to_the_world_produces_no_edits() {
        let world = world_with(&[(0, 9), (64, 2)]);

        let edits = chunk_edits(&world, &chunk(ORIGIN, mask_for(&[0, 64]), vec![9, 2]));

        assert!(edits.is_empty());
    }

    #[test]
    fn a_material_change_emits_set_for_that_cell_only() {
        let world = world_with(&[(0, 9), (1, 4)]);

        let edits = chunk_edits(&world, &chunk(ORIGIN, mask_for(&[0, 1]), vec![9, 7]));

        assert_eq!(
            edits,
            vec![VoxelEdit {
                position: cell_position(1),
                change: VoxelChange::Set(7),
            }]
        );
    }

    #[test]
    fn cells_the_world_lacks_emit_set() {
        let edits = chunk_edits(&World::default(), &chunk(ORIGIN, mask_for(&[3]), vec![6]));

        assert_eq!(
            edits,
            vec![VoxelEdit {
                position: cell_position(3),
                change: VoxelChange::Set(6),
            }]
        );
    }

    #[test]
    fn materials_are_walked_in_mask_bit_order() {
        let indices = [0, 7, 64, 511];

        let edits = chunk_edits(
            &World::default(),
            &chunk(ORIGIN, mask_for(&indices), vec![1, 2, 3, 4]),
        );

        assert_eq!(
            edits,
            vec![
                VoxelEdit {
                    position: cell_position(0),
                    change: VoxelChange::Set(1),
                },
                VoxelEdit {
                    position: cell_position(7),
                    change: VoxelChange::Set(2),
                },
                VoxelEdit {
                    position: cell_position(64),
                    change: VoxelChange::Set(3),
                },
                VoxelEdit {
                    position: cell_position(511),
                    change: VoxelChange::Set(4),
                },
            ]
        );
    }

    #[test]
    fn an_off_origin_chunk_diffs_against_its_own_cells() {
        let origin = IVec3::new(8, -16, 24);
        let world = world_at(origin, &[(0, 1)]);

        let edits = chunk_edits(&world, &chunk(origin, mask_for(&[0]), vec![5]));

        assert_eq!(
            edits,
            vec![VoxelEdit {
                position: origin + cell_position(0),
                change: VoxelChange::Set(5),
            }]
        );
    }

    fn must<T>(result: Result<T, impl std::fmt::Display>) -> T {
        match result {
            Ok(value) => value,
            Err(err) => panic!("{err}"),
        }
    }

    fn assert_feed_matches_world(
        input: &atlas_rt::render::region::feed::RendererInput,
        world: &World,
    ) {
        let expected = must(atlas_rt::world::update::snapshot::emit_snapshots(world));
        let want = must(atlas_rt::render::region::pack::pack_regions(&expected));
        let got = must(input.packed_regions());

        assert_eq!(got.len(), want.len(), "resident region count");

        for (got, want) in got.iter().zip(&want) {
            assert_eq!(got.region_index, want.region_index);
            assert!(got.blocks == want.blocks, "region blocks differ");
            assert_eq!(got.aabbs, want.aabbs, "region aabbs");
        }
    }

    #[test]
    fn a_chunk_submission_leaves_the_feed_and_the_world_in_agreement() {
        use atlas_rt::render::region::feed::RendererInput;
        use atlas_rt::world::update::batch::TrackedCoords;
        use atlas_rt::world::update::edit::edit_world;

        let mut world = world_with(&[(0, 1), (7, 2), (64, 3)]);
        let input = must(RendererInput::new());
        let origin = ORIGIN;

        let edits = chunk_edits(
            &world,
            &chunk(origin, mask_for(&[0, 7, 64, 100]), vec![1, 2, 3, 9]),
        );

        let batch = must(edit_world(&mut world, &edits, &TrackedCoords::default()));
        must(input.submit_batch(batch.snapshots));
        must(input.wait_until_idle());

        assert_feed_matches_world(&input, &world);
    }

    #[test]
    fn a_zero_mask_submission_empties_the_chunk_and_leaves_the_rest_resident() {
        use atlas_rt::render::region::feed::RendererInput;
        use atlas_rt::world::update::batch::TrackedCoords;
        use atlas_rt::world::update::edit::edit_world;

        let mut world = world_with(&[(0, 1), (7, 2)]);
        let neighbour = IVec3::new(8, 0, 0);
        let neighbour_edits = vec![set_edit(neighbour + cell_position(0), 4)];

        let batch = must(edit_world(
            &mut world,
            &neighbour_edits,
            &TrackedCoords::default(),
        ));

        let input = must(RendererInput::new());
        let mut tracked = batch.tracked;

        must(input.submit_batch(batch.snapshots));
        must(input.wait_until_idle());

        let edits = chunk_edits(&world, &chunk(ORIGIN, [0u8; MASK_BYTES], Vec::new()));
        let batch = must(edit_world(&mut world, &edits, &tracked));
        tracked = batch.tracked;

        must(input.submit_batch(batch.snapshots));
        must(input.wait_until_idle());

        assert!(
            !tracked.contains(&ORIGIN),
            "the emptied chunk leaves the tracked set"
        );
        assert!(tracked.contains(&neighbour), "the neighbour stays resident");
        assert_feed_matches_world(&input, &world);
    }
}
