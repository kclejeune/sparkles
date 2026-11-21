//! The datatype checker of items 768 and 972 (F24, "The datatype checker").
//!
//! It decides, for one element, whether a set of data restrictions can hold together
//! over the whole OWL 2 datatype map with its facets: ranges in a normal form by family
//! ([`range`]), the values of literals compared by identity ([`value`]), and the local
//! problem that counts values by type ([`solve`]). The tiers call it from phase 2 on,
//! so until then only its tests use it.
#![cfg_attr(not(test), allow(dead_code))]

pub mod bcp47;
pub mod range;
pub mod rule;
pub mod sets;
pub mod solve;
pub mod value;

#[cfg(test)]
mod tests;
