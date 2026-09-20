use std::sync::{Arc, Mutex, mpsc};

use atlas_rt::world::update::batch::TrackedCoords;
use atlas_rt::world::update::job::{Status, WorldUpdateJob};
use godot::classes::{
    Camera3D, Control, IControl, Material, RenderingServer, ShaderMaterial, Texture2Drd,
};
use godot::prelude::*;

use atlas_rt::render::{
    context::RenderContext,
    delivery::SLOT_COUNT,
    display_gate::DisplayGate,
    embedded::{EmbeddedPipeline, PublishedSlot, WrapTimes},
    pipeline::{DEFAULT_FOV, FrameInput},
};

use crate::view::ATLAS_MODE_UNIFORM;
use crate::worker::{FrameRequest, Worker, lock};

#[derive(GodotClass)]
#[class(base=Control)]
pub struct AtlasRtView {
    fov: f32,
    pub(super) origin: Vector3,
    pub(super) basis: Basis,
    pub(super) render_mode_index: i32,

    pub(super) gpu: Option<Arc<Mutex<RenderContext>>>,
    pub(super) pipeline: Option<Arc<Mutex<EmbeddedPipeline>>>,
    worker: Option<Worker>,
    pub(super) job: Option<WorldUpdateJob>,

    worker_publish_in: Option<mpsc::Receiver<PublishedSlot>>,
    pub(super) display: DisplayGate,
    pub(super) composite_material: Option<Gd<Material>>,
    pub(super) material_detached: bool,
    pub(super) wrapped_texture: Option<Gd<Texture2Drd>>,
    wrapped_at: [Option<u64>; SLOT_COUNT],
    pub(super) world_chunks: TrackedCoords,
    tick: u64,
    pub(super) camera: Option<Gd<Camera3D>>,

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

                        let job = WorldUpdateJob::new();
                        job.arrive();

                        self.pipeline = Some(shared_pipeline.clone());
                        self.worker_publish_in = Some(published_rx);
                        self.job = Some(job);

                        if let Err(probe) = Self::probe_backend(&gpu, &shared_pipeline) {
                            godot_error!("atlas_rt: init probe failed: {}", probe);
                        }

                        Signal::from_object_signal(
                            &RenderingServer::singleton(),
                            "frame_post_draw",
                        )
                        .connect(&Callable::from_object_method(
                            &self.to_gd(),
                            "on_frame_post_draw",
                        ));
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

    /// Status for the loading overlay and load buttons. Reports empty and idle,
    /// loading, a resident world, or failure. A pipeline that failed to initialize
    /// has no world or job and reports failure.
    #[func]
    pub fn job_status(&self) -> i32 {
        self.job
            .as_ref()
            .map_or(Status::Failed, WorldUpdateJob::status)
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
                .map_or(Status::Failed, WorldUpdateJob::status)
                .name(),
        )
    }

    /// Why the last job failed, for display. Empty while the last job did not
    /// fail.
    #[func]
    pub fn job_error(&self) -> GString {
        self.job
            .as_ref()
            .and_then(WorldUpdateJob::error)
            .map_or_else(GString::new, |reason| GString::from(reason.as_str()))
    }

    /// Job progress from 0 to 1 for the loading overlay. Returns 1 when no job
    /// is in flight.
    #[func]
    pub fn job_progress(&self) -> f64 {
        self.job.as_ref().map_or(1.0, WorldUpdateJob::progress)
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

#[cfg(test)]
mod tests {
    use godot::prelude::*;

    use crate::view::camera_view;

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
