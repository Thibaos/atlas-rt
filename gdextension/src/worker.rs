use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use godot::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{JoinHandle, spawn};

use glam::Mat4;

use atlas_rt::render::context::RenderContext;
use atlas_rt::render::embedded::{EmbeddedPipeline, PublishedSlot};
use atlas_rt::render::pipeline::FrameInput;
use atlas_rt::render::region::task::RenderMode;

pub struct Kick {
    pub view_mat: [f32; 16],
    pub fov: f32,
    pub extent: [u32; 2],
    pub mode: RenderMode,
    pub delta_time: f32,
}

struct Mailbox {
    cell: Mutex<Option<Kick>>,
    signal: Condvar,
    shutdown: AtomicBool,
}

impl Mailbox {
    fn kick(&self, kick: Kick) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }

        *lock(&self.cell) = Some(kick);
        self.signal.notify_one();
    }

    fn take(&self) -> Option<Kick> {
        let mut cell = lock(&self.cell);

        loop {
            if let Some(kick) = cell.take() {
                return Some(kick);
            }

            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }

            cell = self.signal.wait(cell).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.signal.notify_one();
    }
}

struct Shared {
    gpu: Arc<Mutex<RenderContext>>,
    pipeline: Arc<Mutex<EmbeddedPipeline>>,
    published: Arc<Mutex<Vec<PublishedSlot>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct Worker {
    mailbox: Arc<Mailbox>,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(
        gpu: Arc<Mutex<RenderContext>>,
        pipeline: Arc<Mutex<EmbeddedPipeline>>,
        published: Arc<Mutex<Vec<PublishedSlot>>>,
    ) -> Self {
        let mailbox = Arc::new(Mailbox {
            cell: Mutex::new(None),
            signal: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });

        let shared = Arc::new(Shared {
            gpu,
            pipeline,
            published,
        });

        let thread_shared = shared.clone();

        let handle = spawn(move || loop {
            let Some(kick) = mailbox.take() else {
                break;
            };

            let input = FrameInput {
                view: Mat4::from_cols_array(&kick.view_mat),
                extent: kick.extent,
                fov: kick.fov,
                resized: false,
                render_mode: kick.mode,
                delta_time: kick.delta_time,
            };

            let published = lock(&thread_shared.gpu);
            let mut pipeline = lock(&thread_shared.pipeline);

            let result = pipeline.run_frame(&published, &input).ok().flatten();
            drop(pipeline);
            drop(published);

            if let Some(slot) = result {
                lock(&thread_shared.published).push(slot);
            }
        });

        Self {
            mailbox,
            shared,
            handle: Some(handle),
        }
    }

    pub fn kick(&self, kick: Kick) {
        self.mailbox.kick(kick);
    }

    pub fn drain(&self) -> Vec<PublishedSlot> {
        lock(&self.shared.published).drain(..).collect()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.mailbox.shutdown();

        if let Some(handle) = self.handle.take() {
            let joined = handle.join();

            if joined.is_err() {
                godot_error!("atlas_rt: worker thread panicked");
            }
        }
    }
}