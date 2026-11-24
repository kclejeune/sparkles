//! Observations of named-domain planning and delivery in unit tests.

use std::sync::atomic::AtomicUsize;

use super::index::Ex;

#[derive(Default)]
pub(super) struct Probe {
    pub build_visits: AtomicUsize,
    pub matches: AtomicUsize,
    pub back_events: AtomicUsize,
    pub back_tests: AtomicUsize,
    pub top_events: AtomicUsize,
    pub top_tests: AtomicUsize,
    pub global_messages: AtomicUsize,
    pub world_messages: AtomicUsize,
    pub world_ready: Option<Box<dyn Fn(Ex, usize) + Send + Sync>>,
}
