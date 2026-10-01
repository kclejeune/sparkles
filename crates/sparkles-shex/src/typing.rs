//! The typing: (node, pair kind) pairs discovered breadth first from the fixed map, with
//! a three-valued local pre-check that keeps definitely failing pairs from expanding,
//! then refined per stratum to the greatest fixed point in parallel waves.
