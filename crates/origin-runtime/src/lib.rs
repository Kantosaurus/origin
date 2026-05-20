//! `origin-runtime` — task-class budgeting + `spawn_in` helper.

pub mod class;
pub mod registry;
pub mod spawn;

pub use class::TaskClass;
pub use registry::init_for_test;
pub use spawn::spawn_in;
