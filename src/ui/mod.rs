//! Terminal UI helpers (spinners etc.). All output goes to stderr so it
//! doesn't pollute stdout pipelines.

pub mod spinner;
pub mod tui;

pub use spinner::Spinner;
