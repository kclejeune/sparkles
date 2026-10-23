//! Community summaries for global questions (spec C22).
//!
//! Phase 1 so far holds the parts that need no report publication: the versioned
//! settings, the persisted build state, the hierarchy adapter over F14's Leiden, stable
//! community identities, and the token budget that admits every model attempt. Report
//! publication, routes and the build task wait for the disclosure guard of C22 §6, so
//! nothing outside the tests calls this module yet.
#![cfg_attr(not(test), allow(dead_code))]

pub mod budget;
pub mod hierarchy;
pub mod identity;
pub mod ledger;
pub mod projection;
pub mod settings;
pub mod state;
