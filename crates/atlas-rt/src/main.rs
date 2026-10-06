mod app;

use app::{App, launch};
use winit::event_loop::EventLoop;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let launch = launch::parse(&args).map_err(anyhow::Error::msg)?;

    let event_loop = EventLoop::new()?;

    let mut app = App::new(
        &event_loop,
        launch.request,
        launch.free_camera,
        launch.no_sim,
    )?;

    event_loop.run_app(&mut app)?;

    Ok(())
}
