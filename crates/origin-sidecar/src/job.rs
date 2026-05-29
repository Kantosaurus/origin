// SPDX-License-Identifier: Apache-2.0
//! Job enum + deliverer traits.

#[allow(
    clippy::module_name_repetitions,
    reason = "SidecarJob, SummaryDeliverer, ExtractDeliverer are the canonical public names"
)]
#[derive(Debug)]
pub enum SidecarJob {
    Summarize {
        session_id: String,
        turn_index: u32,
        transcript: Vec<origin_core::types::Message>,
        deliver_to: Box<dyn SummaryDeliverer>,
    },
    Extract {
        handle: origin_cas::Hash,
        deliver_to: Box<dyn ExtractDeliverer>,
    },
}

#[async_trait::async_trait]
pub trait SummaryDeliverer: Send + Sync + std::fmt::Debug {
    async fn deliver(&self, session_id: &str, turn_index: u32, summary: &str);
}

#[async_trait::async_trait]
pub trait ExtractDeliverer: Send + Sync + std::fmt::Debug {
    async fn deliver(&self, source: origin_cas::Hash, outline_handle: origin_cas::Hash);
}
