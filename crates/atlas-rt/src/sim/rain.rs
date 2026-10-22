//! The schedule that plants a rain World's grains: which holes fire on which
//! tick, as a function of the tick count and the emitter alone.

use glam::IVec3;

use crate::world::rain::Emitter;

/// The cells `emitter` plants a grain into on `tick`.
///
/// A cell is due on the ticks that share its own phase, so no cell fires on two
/// ticks in a row. Among the due cells the tick takes the lowest hashed ranks,
/// ties broken by the cell's index, so the count is exact and the cells differ
/// from tick to tick.
#[must_use]
pub fn emitted(tick: u64, emitter: &Emitter) -> Vec<IVec3> {
    let interval = emitter.interval.max(1);
    let phase = tick.checked_rem(interval).unwrap_or(0);
    let mut ranked: Vec<(u64, usize)> = Vec::new();

    for (index, _) in emitter.cells.iter().enumerate() {
        let cell = u64::try_from(index).unwrap_or(u64::MAX);

        if mix(cell).checked_rem(interval) != Some(phase) {
            continue;
        }

        ranked.push((mix(tick ^ mix(cell)), index));
    }

    ranked.sort_unstable();
    ranked.truncate(emitter.per_tick);

    ranked
        .into_iter()
        .filter_map(|(_, index)| emitter.cells.get(index).copied())
        .collect()
}

/// `SplitMix64`'s finalizer: a bijection on 64 bits that spreads neighbouring
/// inputs across the whole range.
const fn mix(value: u64) -> u64 {
    let first = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let second = (first ^ (first >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);

    second ^ (second >> 31)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const CELLS: usize = 800;
    const PER_TICK: usize = 128;
    const INTERVAL: u64 = 2;

    fn emitter() -> Emitter {
        let cells = (0..CELLS)
            .map(|index| {
                let across = i32::try_from(index % 32).unwrap_or(0);
                let down = i32::try_from(index / 32).unwrap_or(0);

                IVec3::new(across, 0, down)
            })
            .collect();

        Emitter {
            cells,
            interval: INTERVAL,
            per_tick: PER_TICK,
        }
    }

    fn planted(tick: u64) -> HashSet<IVec3> {
        emitted(tick, &emitter()).into_iter().collect()
    }

    #[test]
    fn a_tick_plants_the_per_tick_count() {
        let emitter = emitter();

        for tick in 0..64u64 {
            let cells = emitted(tick, &emitter);

            assert_eq!(
                cells.len(),
                PER_TICK,
                "tick {tick} planted {} cells",
                cells.len()
            );
            assert_eq!(
                cells.iter().collect::<HashSet<_>>().len(),
                PER_TICK,
                "tick {tick} planted a cell twice"
            );
        }
    }

    #[test]
    fn no_cell_is_planted_on_two_consecutive_ticks() {
        for tick in 0..64u64 {
            let now = planted(tick);
            let next = planted(tick.saturating_add(1));

            assert!(
                now.is_disjoint(&next),
                "a cell fired on ticks {tick} and {}",
                tick.saturating_add(1)
            );
        }
    }

    #[test]
    fn every_cell_is_planted_within_a_window() {
        let mut seen: HashSet<IVec3> = HashSet::new();

        for tick in 0..512u64 {
            seen.extend(emitted(tick, &emitter()));
        }

        assert_eq!(
            seen.len(),
            CELLS,
            "{} of the emitter's {CELLS} cells never fired",
            CELLS.saturating_sub(seen.len())
        );
    }

    #[test]
    fn the_same_tick_plants_the_same_cells() {
        let emitter = emitter();

        for tick in [0u64, 1, 7, 63, 200] {
            assert_eq!(emitted(tick, &emitter), emitted(tick, &emitter));
        }
    }
}
