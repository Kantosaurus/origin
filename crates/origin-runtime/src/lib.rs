//! `origin-runtime` — task-class budgeting + `spawn_in` helper.

pub mod bulk_gate;
pub mod class;
pub mod registry;
pub mod spawn;

pub use bulk_gate::BulkGate;
pub use class::TaskClass;
pub use registry::init_for_test;
pub use spawn::spawn_in;
