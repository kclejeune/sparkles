//! Server-wide memory settings: the memory profiles, the shared cache sizes and the
//! query memory pool (spec X08 §2 to §4).

use crate::Cli;
use sparkles::store::caches::CacheSizes;

/// A set of defaults for the memory flags. A flag given on the command line overrides
/// the profile, and the profile overrides the built-in default.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    #[default]
    Default,
    Small,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Default => "default",
            Profile::Small => "small",
        }
    }

    /// The profile's cache sizes.
    pub fn caches(self) -> CacheSizes {
        match self {
            Profile::Default => CacheSizes::default(),
            Profile::Small => CacheSizes::small(),
        }
    }

    /// The profile's budget for one query, in MiB.
    pub fn query_memory_mb(self) -> u64 {
        match self {
            Profile::Default => 8192,
            Profile::Small => 1024,
        }
    }

    /// The profile's query memory pool in MiB, or `None` when the pool is sized from the
    /// memory of the host.
    pub fn total_query_memory_mb(self) -> Option<u64> {
        match self {
            Profile::Default => None,
            Profile::Small => Some(2048),
        }
    }
}

/// The cache sizes: each flag, else the profile's value.
pub fn cache_sizes(cli: &Cli) -> CacheSizes {
    let p = cli.memory_profile.caches();
    let mib = |flag: Option<u64>, profile: u64| flag.map_or(profile, |m| m << 20);
    CacheSizes {
        blocks: mib(cli.cache_mb, p.blocks),
        results: mib(cli.result_cache_mb, p.results),
        service: mib(cli.service_cache_mb, p.service),
        history: mib(cli.history_cache_mb, p.history),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn sizes(args: &[&str]) -> CacheSizes {
        let mut v = vec!["sparkles"];
        v.extend_from_slice(args);
        v.push("serve");
        cache_sizes(&Cli::try_parse_from(v).unwrap())
    }

    #[test]
    fn profiles_set_defaults_and_flags_override_them() {
        assert_eq!(sizes(&[]), CacheSizes::default());
        assert_eq!(sizes(&["--memory-profile", "small"]), CacheSizes::small());
        let s = sizes(&["--memory-profile", "small", "--cache-mb", "512"]);
        assert_eq!(s.blocks, 512 << 20);
        assert_eq!(s.results, CacheSizes::small().results);
        assert_eq!(sizes(&["--result-cache-mb", "0"]).results, 0);
    }
}
