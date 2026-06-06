// SPDX-License-Identifier: Apache-2.0
//! `origin-daemon` library entry — exposes session/agent/protocol modules for
//! the binary and for integration tests.

pub mod agent;
pub mod anthropic_verifier;
pub mod auth;
pub mod compactor;
pub mod config;
pub mod daemon_memory_handle;
pub mod default_workflow;
pub mod goal_checkpoint;
pub mod goal_clear_all;
pub mod goal_driver;
pub mod ipc_prompter;
pub mod lsp_diagnostics;
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
pub mod workflow_runner;
pub mod workflows;

pub use memory_wiring::{MemoryDispatchHandle, MemoryWiring};

pub mod subsystems;
pub mod scheduler;
pub mod ambient;
pub mod webhook;
pub mod routing;
pub mod overnight;
pub mod mem_garden;
pub mod hooks_runtime;
pub mod swarm_worker;
pub mod subagents_md;
pub mod supervisor;
pub mod selfdev;
pub mod teams;
