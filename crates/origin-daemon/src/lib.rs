//! `origin-daemon` library entry — exposes session/agent/protocol modules for
//! the binary and for integration tests.

pub mod agent;
pub mod auth;
pub mod compactor;
pub mod config;
pub mod daemon_memory_handle;
pub mod default_workflow;
pub mod memory_wiring;
pub mod pairing;
pub mod plan_bus;
pub mod proposal_registry;
pub mod protocol;
pub mod provider_factory;
pub mod ra_impl;
pub mod runtime_launch;
pub mod session;
pub mod session_store;
pub mod shutdown;
pub mod skill_catalog;
pub mod stream_relay;
pub mod tool_use_parser;
pub mod workflow_progress;
pub mod workflows;

pub use memory_wiring::{MemoryDispatchHandle, MemoryWiring};
