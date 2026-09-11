use std::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{JoinHandle, spawn},
};

use atlas_rt::render::{
    context::RenderContext,
    embedded::{EmbeddedPipeline, PublishedSlot, WrapTimes},
    pipeline::FrameInput,
};

pub struct FrameRequest {
    pub input: FrameInput,
    pub wrap_times: WrapTimes,
}

struct FrameChannel {
    pending: Mutex<Option<FrameRequest>>,
    signal: Condvar,
    shutdown: AtomicBool,
}

impl FrameChannel {
    const fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            signal: Condvar::new(),
            shutdown: AtomicBool::new(false),
        }
    }

    fn submit(&self, request: FrameRequest) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }

        lock(&self.pending).replace(request);
        self.signal.notify_one();
    }

    fn recv(&self) -> Option<FrameRequest> {
        let mut pending = lock(&self.pending);

        loop {
            if let Some(request) = pending.take() {
                return Some(request);
            }

            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }

            pending = self
                .signal
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.signal.notify_one();
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct Worker {
    channel: Arc<FrameChannel>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(
        gpu: Arc<Mutex<RenderContext>>,
        pipeline: Arc<Mutex<EmbeddedPipeline>>,
        published_tx: mpsc::Sender<PublishedSlot>,
    ) -> Self {
        let channel = Arc::new(FrameChannel::new());
        let channel_handle = Arc::clone(&channel);

        let handle = spawn(move || {
            while let Some(request) = channel_handle.recv() {
                let gpu_guard = lock(&gpu);
                let mut pipeline_guard = lock(&pipeline);

                let result =
                    pipeline_guard.run_frame(&gpu_guard, &request.input, &request.wrap_times);

                let result = match result {
                    Ok(Some(slot)) => Some(slot),
                    Ok(None) => None,
                    Err(error) => {
                        eprintln!("atlas_rt: frame failed: {error:#}");

                        None
                    }
                };

                drop(pipeline_guard);
                drop(gpu_guard);

                if let Some(slot) = result
                    && published_tx.send(slot).is_err()
                {
                    return;
                }
            }
        });

        Self {
            channel,
            handle: Some(handle),
        }
    }

    pub fn submit(&self, request: FrameRequest) {
        self.channel.submit(request);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.channel.shutdown();

        if let Some(handle) = self.handle.take() {
            let joined = handle.join();

            if joined.is_err() {
                godot::prelude::godot_error!("atlas_rt: worker thread panicked");
            }
        }
    }
}
