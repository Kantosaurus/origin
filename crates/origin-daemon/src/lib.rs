//! `origin-daemon` library entry — exposes session/agent/protocol modules for
//! the binary and for integration tests.

pub mod agent;
pub mod auth;
pub mod compactor;
pub mod config;
pub mod memory_wiring;
pub mod pairing;
pub mod plan_bus;
pub mod proposal_registry;
pub mod protocol;
pub mod provider_factory;
pub mod runtime_launch;
pub mod session;
pub mod session_store;
pub mod shutdown;
pub mod stream_relay;
pub mod tool_use_parser;

pub use memory_wiring::{MemoryDispatchHandle, MemoryWiring};
