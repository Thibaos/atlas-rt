//! What a run builds before it can draw: the World under its lock, the
//! snapshots the renderer takes as its first residency, and what the simulation
//! needs at activation.

use std::sync::{Arc, RwLock};

use glam::{IVec3, Vec4};
use tracing::{info, warn};

use atlas_rt::{
    sim::{DrainPolicy, PlayerProfile},
    world::{
        World,
        budget::cell_budget,
        diff::{batch::TrackedCoords, snapshot::MicroChunkSnapshot},
        grid::LATTICE_HALF_EXTENT,
        load::{
            progress::Progress,
            supply::{self, FileWorldSource, SuppliedWorld},
        },
        material::PhysicalMaterialTable,
        rain::Emitter,
    },
};

use crate::app::launch::WorldRequest;

pub struct Startup {
    pub world: Arc<RwLock<World>>,
    pub snapshots: Vec<MicroChunkSnapshot>,
    pub tracked: TrackedCoords,
    pub palette: [Vec4; 256],
    pub materials: PhysicalMaterialTable,
    pub granular_cells: Option<Vec<IVec3>>,
    pub emitter: Option<Emitter>,
    pub profile: PlayerProfile,
    pub policy: DrainPolicy,
}

/// Builds the World a run's flags name and the simulation's first state.
///
/// # Errors
///
/// Returns an error when the file cannot be read, the Generation fails, or a
/// fixed scene cannot be built.
pub fn supply_world(request: &WorldRequest) -> anyhow::Result<Startup> {
    let SuppliedWorld {
        world,
        snapshots,
        palette,
        materials,
        granular_cells,
        emitter,
        clipped,
    } = match request {
        WorldRequest::Load(world_path) => {
            let source =
                FileWorldSource::new(format!("crates/atlas-rt/assets/{world_path}"), world_path);

            supply::load(&source, cell_budget(), &Progress::load_path())?
        }
        WorldRequest::Generate(params) => {
            info!("generating a World from seed {}", params.seed);

            supply::generate(*params, &Progress::generate_path())?
        }
        WorldRequest::SandBall => {
            info!("building the sand ball");

            supply::sand_ball(&Progress::generate_path())?
        }
        WorldRequest::Raining => {
            info!("building the raining world");

            supply::raining(&Progress::generate_path())?
        }
    };

    if clipped > 0 {
        warn!("clipped {clipped} voxels outside the ±{LATTICE_HALF_EXTENT} lattice");
    }

    let tracked: TrackedCoords = snapshots
        .iter()
        .filter(|snapshot| snapshot.occupied_count() > 0)
        .map(|snapshot| snapshot.global_coords)
        .collect();

    Ok(Startup {
        world: Arc::new(RwLock::new(world)),
        snapshots,
        tracked,
        palette,
        materials,
        granular_cells,
        emitter,
        profile: PlayerProfile::default(),
        policy: DrainPolicy::default(),
    })
}
