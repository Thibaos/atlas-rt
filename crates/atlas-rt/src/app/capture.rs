//! The headless capture: build the World the flags name, advance the
//! simulation the flags ask for, draw one frame offscreen, and write it to a
//! PNG.
//!
//! Nothing here opens a window or needs a surface. The frame is drawn by the
//! same embedded pipeline the Godot view renders with, into a delivery image a
//! swapchain would otherwise show, and the readback encodes it the way the
//! host's composite shader does.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use glam::{Mat4, Vec3};
use tracing::info;

use atlas_rt::{
    render::{
        capture::{self, FrameCapture},
        context::RenderContext,
        embedded::{EmbeddedPipeline, WrapTimes},
        pipeline::{DEFAULT_FOV, FrameInput, task::RenderMode},
    },
    sim::InputSample,
};

use crate::app::{
    PlayerController,
    launch::{CaptureRequest, Launch},
    sim_host::SimHost,
    startup::supply_world,
};

/// The frames drawn once the simulation has advanced. The first carries the
/// residency the store applied and the second is the same picture with every
/// upload settled, so a capture never races the input worker.
const FRAMES_AFTER_SETTLE: u32 = 2;

/// The view matrix for an eye and the point it looks at, in the left-handed
/// world the renderer draws.
fn look_at(eye: Vec3, target: Vec3) -> Mat4 {
    glam::camera::lh::view::look_at_mat4(eye, target, Vec3::Y)
}

/// # Errors
///
/// Returns an error if the device, the World supply, the pipeline, the
/// simulation, or the PNG write failed.
pub fn run(launch: &Launch, request: &CaptureRequest) -> anyhow::Result<()> {
    let path = Path::new(&request.path);
    let [width, height] = request.extent;

    info!(
        "capturing {width} by {height} after {} ticks to {}",
        request.ticks,
        path.display()
    );

    let gpu = RenderContext::new_headless()?;
    let startup = supply_world(&launch.request)?;
    let materials = startup.materials;
    let profile = startup.profile;

    let mut pipeline = EmbeddedPipeline::new(&gpu, request.extent, launch.ao_samples)?;

    pipeline
        .upload_palette(&gpu, startup.palette.map(|color| color.to_array()))
        .context("the capture's palette upload failed")?;

    pipeline.input().submit_batch(startup.snapshots.clone())?;

    let mut camera = PlayerController::default();

    if !launch.no_sim {
        let mut sim = SimHost::spawn(
            Arc::clone(&startup.world),
            profile,
            startup.policy,
            startup.snapshots,
            startup.tracked,
            startup.granular_cells,
            startup.emitter,
            &materials,
        )?;

        sim.start();

        let mut forward = |batch| pipeline.input().submit_batch(batch);

        sim.drive(
            Duration::from_millis(1),
            InputSample::default(),
            &mut forward,
        )?;
        sim.advance(request.ticks, &mut forward)?;

        let tally = sim.take_tally();

        info!(
            "advanced {} ticks, {} grains processed{}",
            tally.ticks,
            tally.processed,
            sim.queued()
                .map_or_else(String::new, |queued| format!(", {queued} queued"))
        );

        if !launch.free_camera {
            camera.place_eye(sim.frame_state(Instant::now()).feet, profile.eye_offset);
        }
    }

    pipeline.input().wait_until_idle()?;

    let view: Mat4 = request.camera.map_or_else(
        || camera.view(),
        |camera| look_at(camera.eye, camera.target),
    );

    let delta_time = profile.tick_period().as_secs_f32();

    for _ in 0..FRAMES_AFTER_SETTLE {
        pipeline.run_frame(
            &gpu,
            &FrameInput {
                view,
                extent: request.extent,
                fov: DEFAULT_FOV,
                resized: false,
                render_mode: RenderMode::Voxel,
                delta_time,
            },
            &WrapTimes::default(),
        )?;
    }

    let capture: FrameCapture = pipeline.capture(&gpu, 0, RenderMode::Voxel)?;

    capture::save_png(path, &capture)?;

    info!(
        "saved {} by {} to {}",
        capture.extent[0],
        capture.extent[1],
        path.display()
    );

    Ok(())
}
