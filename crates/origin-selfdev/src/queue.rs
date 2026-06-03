// SPDX-License-Identifier: Apache-2.0
//! FIFO queue of self-modification jobs.
//!
//! Self-dev requests arrive while a job may already be in flight. The machine
//! processes exactly one job at a time (an edit→build→test→restart cycle is not
//! safe to interleave), so requests are queued and dequeued FIFO. The queue is
//! a plain data structure; the [`crate::SelfDevDriver`] owns it and the
//! single-in-flight invariant.

use serde::{Deserialize, Serialize};

/// A single self-modification request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildJob {
    /// Stable identifier for the job.
    pub id: String,
    /// Human description of what the self-modification should accomplish.
    pub description: String,
    /// Optional set of source paths the job intends to touch. Empty means
    /// "unscoped" (the agent decides). Used for audit and for scoping a
    /// rollback to just these paths when possible.
    #[serde(default)]
    pub target_paths: Vec<String>,
}

impl BuildJob {
    /// Construct an unscoped job (no declared target paths).
    #[must_use]
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            target_paths: Vec::new(),
        }
    }

    /// Declare the source paths this job intends to modify.
    #[must_use]
    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.target_paths = paths;
        self
    }
}

/// FIFO queue of [`BuildJob`]s.
///
/// Newly enqueued jobs go to the back; the driver pops from the front. Cloneable
/// and serde-friendly so it can be persisted/inspected.
#[allow(clippy::module_name_repetitions)] // `BuildQueue` is the documented public type re-exported at the crate root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildQueue {
    jobs: std::collections::VecDeque<BuildJob>,
}

impl BuildQueue {
    /// Create an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `job` to the back of the queue.
    pub fn enqueue(&mut self, job: BuildJob) {
        self.jobs.push_back(job);
    }

    /// Pop the front (oldest) job, if any.
    pub fn dequeue(&mut self) -> Option<BuildJob> {
        self.jobs.pop_front()
    }

    /// Peek at the front job without removing it.
    #[must_use]
    pub fn peek(&self) -> Option<&BuildJob> {
        self.jobs.front()
    }

    /// Number of queued jobs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Snapshot of the queued jobs in FIFO order (for inspection / reporting).
    pub fn iter(&self) -> impl Iterator<Item = &BuildJob> {
        self.jobs.iter()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn job_builders() {
        let j = BuildJob::new("j1", "do a thing");
        assert!(j.target_paths.is_empty());
        let j2 = BuildJob::new("j2", "scoped").with_paths(vec!["src/a.rs".into()]);
        assert_eq!(j2.target_paths, vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn fifo_order_is_preserved() {
        let mut q = BuildQueue::new();
        assert!(q.is_empty());
        q.enqueue(BuildJob::new("a", "first"));
        q.enqueue(BuildJob::new("b", "second"));
        q.enqueue(BuildJob::new("c", "third"));
        assert_eq!(q.len(), 3);
        assert_eq!(q.peek().unwrap().id, "a");
        assert_eq!(q.dequeue().unwrap().id, "a");
        assert_eq!(q.dequeue().unwrap().id, "b");
        assert_eq!(q.dequeue().unwrap().id, "c");
        assert!(q.dequeue().is_none());
    }

    #[test]
    fn iter_yields_fifo() {
        let mut q = BuildQueue::new();
        q.enqueue(BuildJob::new("a", "1"));
        q.enqueue(BuildJob::new("b", "2"));
        let ids: Vec<_> = q.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn queue_round_trips_through_serde() {
        let mut q = BuildQueue::new();
        q.enqueue(BuildJob::new("a", "1").with_paths(vec!["p".into()]));
        let json = serde_json::to_string(&q).unwrap();
        let back: BuildQueue = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
    }
}
