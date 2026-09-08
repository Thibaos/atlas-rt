use std::sync::{Arc, Mutex};

use godot::prelude::*;

use atlas_rt::render::{context::RenderContext, embedded::EmbeddedPipeline};

#[derive(GodotClass)]
#[class(base=Control)]
pub struct AtlasRtView {
    status: i32,
    gpu: Option<Arc<Mutex<RenderContext>>>,
    pipeline: Option<Arc<Mutex<EmbeddedPipeline>>>,
    base: Base<Control>,
}

#[godot_api]
impl IControl for AtlasRtView {
    fn init(base: Base<Control>) -> Self {
        Self {
            status: 0,
            gpu: None,
            pipeline: None,
            base,
        }
    }

    fn ready(&mut self) {
        self.to_gd().set_anchors_and_offsets_preset(
            godot::classes::control::LayoutPreset::PRESET_FULL_RECT,
        );

        match RenderContext::new_headless() {
            Ok(context) => {
                let gpu = Arc::new(Mutex::new(context));

                match EmbeddedPipeline::new(&gpu, [1280, 720]) {
                    Ok(pipeline) => {
                        let pipeline = Arc::new(Mutex::new(pipeline));
                        let _ = pipeline;

                        self.status = 1;
                    }
                    Err(err) => {
                        godot_error!("atlas_rt: pipeline failed: {}", err);
                        self.status = 2;
                    }
                }

                self.gpu = Some(gpu);
            }
            Err(err) => {
                godot_error!("atlas_rt: initialization failed: {}", err);
                self.status = 2;
            }
        }
    }
}
