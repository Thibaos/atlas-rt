use std::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{JoinHandle, spawn},
};

use glam::Mat4;

use atlas_rt::render::{
    context::RenderContext,
    embedded::{EmbeddedPipeline, PublishedSlot},
    pipeline::FrameInput,
    region::task::RenderMode,
};

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
    const fn new() -> Self {
        Self {
            cell: Mutex::new(None),
            signal: Condvar::new(),
            shutdown: AtomicBool::new(false),
        }
    }

    fn kick(&self, kick: Kick) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }

        lock(&self.cell).replace(kick);
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

            cell = self
                .signal
                .wait(cell)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let mailbox = Arc::new(Mailbox::new());

        let shared = Arc::new(Shared {
            gpu,
            pipeline,
            published,
        });

        let thread_shared = Arc::clone(&shared);
        let mailbox_handle = Arc::clone(&mailbox);

        let handle = spawn(move || {
            while let Some(kick) = mailbox_handle.take() {
                let input = FrameInput {
                    view: Mat4::from_cols_array(&kick.view_mat),
                    extent: kick.extent,
                    fov: kick.fov,
                    resized: false,
                    render_mode: kick.mode,
                    delta_time: kick.delta_time,
                };

                let gpu_guard = lock(&thread_shared.gpu);
                let mut pipeline_guard = lock(&thread_shared.pipeline);

                let result = pipeline_guard.run_frame(&gpu_guard, &input).ok().flatten();

                drop(pipeline_guard);
                drop(gpu_guard);

                if let Some(slot) = result {
                    lock(&thread_shared.published).push(slot);
                }
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

    #[must_use]
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
                godot::prelude::godot_error!("atlas_rt: worker thread panicked");
            }
        }
    }
}
