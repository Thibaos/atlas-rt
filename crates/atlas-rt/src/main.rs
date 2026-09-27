mod app;

use app::App;
use winit::event_loop::EventLoop;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    let world_path = args
        .iter()
        .position(|arg| arg == "--world")
        .and_then(|i| args.get(i.strict_add(1)))
        .map_or("castle.vox", String::as_str);

    let fly = args.iter().any(|arg| arg == "--fly");

    let event_loop = EventLoop::new()?;

    let mut app = App::new(&event_loop, world_path, true, fly)?;

    event_loop.run_app(&mut app)?;

    Ok(())
}
