#[allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::option_if_let_else,
    clippy::missing_const_for_fn,
    clippy::needless_continue,
    clippy::uninlined_format_args,
    clippy::borrow_as_ptr,
    clippy::cast_precision_loss,
    clippy::large_types_passed_by_value,
)]
pub mod render;

#[allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args,
    clippy::large_types_passed_by_value,
)]
pub mod world;

pub mod app;
