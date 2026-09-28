mod input;
mod interpolation;
mod player;
mod schedule;
mod sim_host;

use std::{
    path::Path,
    sync::{Arc, PoisonError, RwLock},
    time::{Duration, Instant},
};

use anyhow::Context;
use glam::{Mat4, camera::lh::proj::vulkan::perspective};

use tracing::{error, info, warn};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{DeviceEvent, ElementState, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowAttributes},
};

#[cfg(debug_assertions)]
use atlas_rt::render::pipeline::next_render_mode;
use atlas_rt::{
    render::{
        context::RenderContext,
        pipeline::{DEFAULT_FOV, FrameInput, FramePipeline, PROJ_FAR, PROJ_NEAR, task::RenderMode},
    },
    sim::{Command, PlayerProfile},
    world::{
        World,
        format::open_file,
        grid::LATTICE_HALF_EXTENT,
        material::load_table,
        raycast::{VoxelHit, screen_center_ray},
        update::{
            batch::TrackedCoords,
            edit::{VoxelChange, VoxelEdit, edit_world},
            snapshot::emit_snapshots,
        },
    },
};

use input::{Input, InputButton, InputKey};
use player::PlayerController;
use schedule::ScheduleController;
use sim_host::SimHost;

#[allow(clippy::struct_excessive_bools)]
pub struct App {
    close_requested: bool,

    pub gpu: RenderContext,

    delta_time: Duration,
    focused: bool,

    pub voxel_data: dot_vox::DotVoxData,
    world: Arc<RwLock<World>>,
    tracked: TrackedCoords,
    profile: PlayerProfile,
    sim: Option<SimHost>,

    player_controller: PlayerController,
    player_input: Input,
    schedule_controller: ScheduleController,

    log_frames: u16,
    log_since: Instant,

    render_mode: RenderMode,

    window: Option<Arc<Window>>,
    pipeline: Option<FramePipeline>,

    resize_pending: bool,
    #[cfg(debug_assertions)]
    mode_toggle_pending: bool,
}

impl App {
    /// # Errors
    ///
    /// Returns an error if the GPU could not be initialized or the loaded
    /// World's snapshots cannot be emitted.
    pub fn new(
        event_loop: &EventLoop<()>,
        world_path: &str,
        clip_oob: bool,
        fly: bool,
    ) -> anyhow::Result<Self> {
        let gpu = RenderContext::new(event_loop)?;

        let asset_path = format!("crates/atlas-rt/assets/{world_path}");
        let voxel_data = open_file(&asset_path);
        let (world, clipped) = if clip_oob {
            World::new_clipped(&voxel_data)
        } else {
            (World::new(&voxel_data), 0)
        };
        if clipped > 0 {
            warn!("clipped {clipped} voxels outside the ±{LATTICE_HALF_EXTENT} lattice");
        }

        let world = Arc::new(RwLock::new(world));

        let (snapshots, tracked) = {
            let guard = world.read().unwrap_or_else(PoisonError::into_inner);

            let snapshots = emit_snapshots(&guard)?;
            drop(guard);

            let tracked: TrackedCoords = snapshots
                .iter()
                .filter(|snapshot| snapshot.occupied_count() > 0)
                .map(|snapshot| snapshot.global_coords)
                .collect();

            (snapshots, tracked)
        };

        let profile = PlayerProfile::default();
        let materials = load_table(Some(Path::new(&asset_path)));

        let sim = if fly {
            info!("fly mode: free camera, no simulation thread");
            None
        } else {
            Some(SimHost::spawn(
                Arc::clone(&world),
                profile,
                snapshots,
                tracked.clone(),
                &materials,
            )?)
        };

        let mut schedule_controller = ScheduleController::new();
        schedule_controller.add_schedule_frames("delta", 1);
        schedule_controller.add_schedule_duration("log", Duration::from_secs(1));

        Ok(Self {
            close_requested: false,

            gpu,

            delta_time: Duration::ZERO,
            focused: false,

            player_controller: PlayerController::default(),
            player_input: Input::default(),
            schedule_controller,

            log_frames: 0u16,
            log_since: Instant::now(),

            render_mode: RenderMode::default(),

            voxel_data,
            world,
            tracked,
            profile,
            sim,

            window: None,
            pipeline: None,

            resize_pending: false,
            #[cfg(debug_assertions)]
            mode_toggle_pending: false,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the window is not available.
    pub fn toggle_capture_mouse(&mut self) -> anyhow::Result<()> {
        let window = self
            .window
            .as_ref()
            .context("app window is not available")?;

        if self.focused {
            self.focused = false;
            window.set_cursor_grab(winit::window::CursorGrabMode::None)?;
            window.set_cursor_visible(true);
        } else {
            self.focused = true;
            window.set_cursor_grab(winit::window::CursorGrabMode::Confined)?;
            window.set_cursor_visible(false);
        }

        Ok(())
    }

    #[cfg(debug_assertions)]
    fn handle_toggle_render_mode(&mut self) {
        if self
            .player_input
            .just_pressed
            .contains(&InputKey::ToggleRenderMode)
        {
            self.mode_toggle_pending = true;
        }
    }

    fn update_delta_time(&mut self) -> anyhow::Result<Duration> {
        self.delta_time = self
            .schedule_controller
            .check("delta")
            .context("delta schedule is not registered")?;

        Ok(self.delta_time)
    }

    fn request_log(&mut self) {
        self.log_frames = self.log_frames.saturating_add(1);

        if self.schedule_controller.check("log").is_some() {
            let fps = f32::from(self.log_frames) / self.log_since.elapsed().as_secs_f32();
            info!("{fps:.0} fps");

            self.log_frames = 0;
            self.log_since = Instant::now();
        }
    }

    /// Sends one frame of elapsed time and sampled input to the simulation
    /// and forwards what it pushed to the renderer. The first frame waits for
    /// readiness; later frames take what is queued without waiting.
    fn drive_simulation(&mut self) {
        let elapsed = self.delta_time;

        let Self {
            sim,
            pipeline,
            player_controller,
            player_input,
            ..
        } = self;

        let Some(sim) = sim.as_mut() else {
            return;
        };

        let sample = input::sample(player_controller.yaw(), player_input);

        let mut forward = |batch| {
            pipeline
                .as_ref()
                .context("app pipeline is none")
                .and_then(|pipeline| pipeline.input().submit_batch(batch))
        };

        if let Err(e) = sim.drive(elapsed, sample, &mut forward) {
            error!("{e:?}");
        }
    }

    fn next_player_view(&mut self) -> Mat4 {
        if self.focused {
            self.player_controller
                .rotate(self.player_input.mouse_motion);
        }

        match self
            .sim
            .as_ref()
            .filter(|sim| sim.ready())
            .map(|sim| sim.frame_state(Instant::now()))
        {
            Some(state) => self
                .player_controller
                .place_eye(state.feet, self.profile.eye_offset),
            None => self
                .player_controller
                .fly_movement(self.delta_time, &self.player_input),
        }

        self.player_controller.view()
    }

    /// Fires the center ray and turns the hit into a cell clear: a command
    /// the simulation commits when one is running, a direct write under the
    /// World's lock in fly mode.
    fn dig(&mut self) {
        let Some(hit) = self.center_hit() else {
            return;
        };

        let edit = VoxelEdit {
            position: hit.voxel,
            change: VoxelChange::Clear,
        };

        if self.sim.is_none() {
            self.apply_fly_edit(edit);

            return;
        }

        if let Some(sim) = self.sim.as_ref() {
            sim.command(Command::Cell(edit));
        }
    }

    fn center_hit(&mut self) -> Option<VoxelHit> {
        let extent = self.window.as_ref()?.inner_size();

        if extent.width == 0 || extent.height == 0 {
            return None;
        }

        let aspect = extent.width as f32 / extent.height as f32;
        let proj = perspective(DEFAULT_FOV, aspect, PROJ_NEAR, PROJ_FAR);
        let view = self.player_controller.view();
        let ray = screen_center_ray(proj.inverse(), view.inverse());

        self.world
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .raycast(ray)
    }

    fn apply_fly_edit(&mut self, edit: VoxelEdit) {
        let batch = {
            let mut world = self.world.write().unwrap_or_else(PoisonError::into_inner);

            match edit_world(&mut world, &[edit], &self.tracked) {
                Ok(batch) => batch,
                Err(e) => {
                    error!("{e:?}");
                    return;
                }
            }
        };

        self.tracked = batch.tracked;

        if let Some(pipeline) = self.pipeline.as_ref()
            && let Err(e) = pipeline.input().submit_batch(batch.snapshots)
        {
            error!("{e:?}");
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window_attributes =
            WindowAttributes::default().with_inner_size(PhysicalSize::new(1920, 1080));

        match event_loop.create_window(window_attributes) {
            Ok(win) => {
                let window = Arc::new(win);

                let pipeline = {
                    let guard = self.world.read().unwrap_or_else(PoisonError::into_inner);

                    FramePipeline::new(&self.gpu, window.clone(), &self.voxel_data, &guard)
                };

                match pipeline {
                    Ok(pipeline) => {
                        self.pipeline = Some(pipeline);

                        if let Some(sim) = self.sim.as_mut() {
                            sim.start();
                        }
                    }
                    Err(e) => error!("{e:?}"),
                }

                self.window = Some(window);
            }
            Err(e) => error!("{e:?}"),
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.close_requested = true;
            }
            WindowEvent::Resized(_) => {
                self.resize_pending = true;
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.update_delta_time() {
                    error!("{e:?}");
                }

                self.request_log();

                let resized = std::mem::take(&mut self.resize_pending);

                #[cfg(debug_assertions)]
                if std::mem::take(&mut self.mode_toggle_pending) {
                    self.render_mode = next_render_mode(self.render_mode);
                }

                let extent = self.window.as_ref().map(|w| w.inner_size());

                let view_extent: [u32; 2] =
                    extent.map_or([0, 0], |extent| [extent.width, extent.height]);

                self.drive_simulation();

                let view = self.next_player_view();

                if let Some(pipeline) = self.pipeline.as_mut() {
                    if let Err(e) = pipeline.run_frame(
                        &self.gpu,
                        &FrameInput {
                            view,
                            extent: view_extent,
                            fov: DEFAULT_FOV,
                            resized,
                            render_mode: self.render_mode,
                            delta_time: self.delta_time.as_secs_f32(),
                        },
                    ) {
                        error!("{e:?}");
                    }
                } else {
                    panic!("app pipeline is None");
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(mapped) = input::map_mouse_button(button) {
                    match state {
                        ElementState::Pressed => {
                            if mapped == InputButton::Right
                                && let Err(e) = self.toggle_capture_mouse()
                            {
                                error!("{e:?}");
                            }

                            if mapped == InputButton::Left && self.focused {
                                self.dig();
                            }

                            self.player_input.buttons_down.insert(mapped);
                        }
                        ElementState::Released => {
                            self.player_input.buttons_down.remove(&mapped);
                        }
                    }
                }
            }
            WindowEvent::MouseWheel {
                delta: MouseScrollDelta::LineDelta(_, y),
                ..
            } => {
                self.player_input.scroll_delta += y;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(key) = input::map_key(&event.logical_key) {
                    match event.state {
                        ElementState::Pressed => {
                            self.player_input.down.insert(key);
                            self.player_input.just_pressed.insert(key);
                        }
                        ElementState::Released => {
                            self.player_input.down.remove(&key);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.player_input.just_pressed.contains(&InputKey::Close) {
            self.close_requested = true;
        }

        #[cfg(debug_assertions)]
        self.handle_toggle_render_mode();
        self.player_input.clear();

        if self.close_requested {
            event_loop.exit();
        } else if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event {
            self.player_input.mouse_motion.0 += delta.0;
            self.player_input.mouse_motion.1 += delta.1;
        }
    }
}
