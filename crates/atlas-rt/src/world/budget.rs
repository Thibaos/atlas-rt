//! The cell budget a load or an edit reads before it allocates.
//!
//! The shipped threshold is unbounded; a test-only setter drives a small one.
//! It is per-thread, so the refusal tests cannot race the others beside them.

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static CELL_BUDGET: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// The most cells a World may hold.
#[cfg(not(test))]
#[must_use]
pub const fn cell_budget() -> usize {
    usize::MAX
}

/// The most cells a World may hold.
#[cfg(test)]
#[must_use]
pub fn cell_budget() -> usize {
    CELL_BUDGET.with(Cell::get)
}

/// Sets this thread's cell budget and restores it when the guard drops.
#[cfg(test)]
#[must_use]
pub fn set_cell_budget(limit: usize) -> CellBudget {
    let previous = CELL_BUDGET.with(Cell::get);

    CELL_BUDGET.with(|budget| budget.set(limit));

    CellBudget(previous)
}

/// Restores the cell budget that was in force before [`set_cell_budget`].
#[cfg(test)]
#[derive(Debug)]
pub struct CellBudget(usize);

#[cfg(test)]
impl Drop for CellBudget {
    fn drop(&mut self) {
        CELL_BUDGET.with(|budget| budget.set(self.0));
    }
}
