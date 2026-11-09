//! Admission of each HTTP attempt of a model call (spec C22 §3.5).
//!
//! A caller that enforces a strict token budget passes an [`Admission`] to
//! [`super::Models::call_admitted`]. The client asks it before every HTTP attempt,
//! including its own retries of a `429` or `5xx` answer, and every request that
//! `Models::call` makes for a schema-correction retry or another structured-output level
//! goes through the client, so no attempt escapes the check. The admission returns a
//! [`Ticket`] for the attempt, and the client settles the ticket with the usage the
//! provider reported, or leaves it unsettled when the usage is unknown.

use super::client::{CallError, ChatRequest};
use super::config::{CacheTtl, Pricing, ProviderConfig};
use serde_json::Value;

/// What one HTTP attempt would send.
pub struct Attempt<'a> {
    /// the provider's name in the configuration
    pub provider: &'a str,
    pub config: &'a ProviderConfig,
    pub request: &'a ChatRequest,
    pub url: &'a str,
    pub body: &'a Value,
    /// the serialized body, exactly as it is posted
    pub bytes: &'a [u8],
    /// the TTL of the prefixes the body asks the provider to cache, when it marks any:
    /// such an attempt can be charged a cache write for each of its input tokens
    pub cache_ttl: Option<CacheTtl>,
    /// the provider's own token count of this request, through the same transport, key
    /// and outbound policy as the attempt; `None` when the provider's kind has no
    /// counting endpoint
    pub provider_count: Option<&'a dyn Fn() -> Result<u64, CallError>>,
}

/// The tokens an answered attempt used, as the provider reported them. `input` is the
/// input the provider read without its cache. Anthropic reports the tokens it wrote to
/// its cache and the tokens it read from it apart from `input`, so the prompt had
/// `input + cache_write_5m + cache_write_1h + cache_read` tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    /// input written to the cache with the 5-minute TTL
    pub cache_write_5m: u64,
    /// input written to the cache with the 1-hour TTL
    pub cache_write_1h: u64,
    /// input read from the cache
    pub cache_read: u64,
}

/// The price of each class of input relative to uncached input, in fortieths: a write
/// with the 5-minute TTL costs 1.25 times as much, one with the 1-hour TTL 2 times, and
/// a read 0.1 times. These are Anthropic's standard multipliers. Models whose reads cost
/// less, such as Claude Opus 5.5, are charged the standard rate, which is the higher.
const INPUT: u64 = 40;
const WRITE_5M: u64 = 50;
const WRITE_1H: u64 = 80;
const READ: u64 = 4;

impl Usage {
    /// The tokens a token budget counts: the output, and the input priced as uncached
    /// input tokens, rounded up once. A cache write counts 1.25 tokens with the 5-minute
    /// TTL and 2 tokens with the 1-hour TTL, and a cache read counts 0.1 token. Without
    /// caching this is input plus output.
    pub fn budget(&self) -> u64 {
        let weighted = (self.input as u128 * INPUT as u128)
            + (self.cache_write_5m as u128 * WRITE_5M as u128)
            + (self.cache_write_1h as u128 * WRITE_1H as u128)
            + (self.cache_read as u128 * READ as u128);
        let input = weighted.div_ceil(INPUT as u128).min(u64::MAX as u128) as u64;
        input.saturating_add(self.output)
    }

    /// The cost at `p`, in its currency.
    pub fn cost(&self, p: &Pricing) -> f64 {
        (self.input as f64 * p.input_per_m_tok
            + self.cache_write_5m as f64 * p.cache_write_5m()
            + self.cache_write_1h as f64 * p.cache_write_1h()
            + self.cache_read as f64 * p.cache_read()
            + self.output as f64 * p.output_per_m_tok)
            / 1e6
    }

    /// The sum of two usages.
    pub fn plus(self, o: Usage) -> Usage {
        Usage {
            input: self.input.saturating_add(o.input),
            output: self.output.saturating_add(o.output),
            cache_write_5m: self.cache_write_5m.saturating_add(o.cache_write_5m),
            cache_write_1h: self.cache_write_1h.saturating_add(o.cache_write_1h),
            cache_read: self.cache_read.saturating_add(o.cache_read),
        }
    }
}

/// The most input tokens [`Usage::budget`] can count for a prompt of `prompt` tokens
/// whose prefixes are cached with `ttl`: every token written to the cache at that TTL.
pub fn input_budget_bound(prompt: u64, ttl: Option<CacheTtl>) -> u64 {
    let weight = match ttl {
        None => INPUT,
        Some(CacheTtl::FiveMinutes) => WRITE_5M,
        Some(CacheTtl::OneHour) => WRITE_1H,
    };
    ((prompt as u128 * weight as u128).div_ceil(INPUT as u128)).min(u64::MAX as u128) as u64
}

/// The settlement of one admitted attempt.
pub trait Settle: Send {
    /// The provider reported its usage.
    fn reported(self: Box<Self>, usage: Usage);
    /// The usage is not known: the attempt failed after it may have been sent, or the
    /// answer reported no usage. The reservation stays counted.
    fn unknown(self: Box<Self>);
}

/// An admitted attempt, to settle once.
pub struct Ticket(Option<Box<dyn Settle>>);

impl Ticket {
    pub fn new(s: Box<dyn Settle>) -> Ticket {
        Ticket(Some(s))
    }

    /// The attempt was not sent after all, so it used nothing.
    pub fn not_sent(self) {
        self.settle(Some(Usage::default()));
    }

    pub fn settle(mut self, usage: Option<Usage>) {
        if let Some(s) = self.0.take() {
            match usage {
                Some(u) => s.reported(u),
                None => s.unknown(),
            }
        }
    }
}

impl Drop for Ticket {
    /// A ticket dropped without a settlement leaves its usage unknown.
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            s.unknown();
        }
    }
}

/// Decides whether an attempt may be sent.
pub trait Admission: Send + Sync {
    /// Admit the attempt and reserve its tokens, or refuse it with
    /// [`CallError::Admission`].
    fn admit(&self, attempt: &Attempt<'_>) -> Result<Ticket, CallError>;

    /// Whether an admitted attempt may follow redirects, which send its body again
    /// without another admission. An admission that must see every send says no.
    fn follows_redirects(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, w5: u64, w1: u64, read: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            cache_write_5m: w5,
            cache_write_1h: w1,
            cache_read: read,
        }
    }

    /// A budget counts each class of input at its price relative to uncached input,
    /// rounded up once, and output as it is.
    #[test]
    fn budgets_price_each_class() {
        assert_eq!(usage(100, 0, 0, 0, 50).budget(), 150);
        assert_eq!(usage(0, 1000, 0, 0, 0).budget(), 1250);
        assert_eq!(usage(0, 0, 1000, 0, 0).budget(), 2000);
        assert_eq!(usage(0, 0, 0, 1000, 0).budget(), 100);
        // 10 + 1.25 + 2 + 0.1 = 13.35, rounded up
        assert_eq!(usage(10, 1, 1, 1, 0).budget(), 14);
        assert_eq!(usage(0, 0, 0, 1, 0).budget(), 1);
        assert_eq!(usage(u64::MAX, 0, 0, 0, 1).budget(), u64::MAX);
    }

    /// No split of a prompt between the cache classes can count more than the bound at
    /// the request's TTL.
    #[test]
    fn the_input_bound_holds_for_every_split() {
        assert_eq!(input_budget_bound(1000, None), 1000);
        assert_eq!(input_budget_bound(1000, Some(CacheTtl::FiveMinutes)), 1250);
        assert_eq!(input_budget_bound(1001, Some(CacheTtl::FiveMinutes)), 1252);
        assert_eq!(input_budget_bound(1000, Some(CacheTtl::OneHour)), 2000);
        for prompt in [0u64, 1, 7, 333, 1001] {
            for w in 0..=prompt.min(40) {
                for r in 0..=(prompt - w).min(40) {
                    let i = prompt - w - r;
                    let five = usage(i, w, 0, r, 0).budget();
                    assert!(five <= input_budget_bound(prompt, Some(CacheTtl::FiveMinutes)));
                    let hour = usage(i, 0, w, r, 0).budget();
                    assert!(hour <= input_budget_bound(prompt, Some(CacheTtl::OneHour)));
                    assert!(
                        usage(i + w + r, 0, 0, 0, 0).budget() <= input_budget_bound(prompt, None)
                    );
                }
            }
        }
    }

    /// A cost prices each class at the pricing's cache prices, which default to the
    /// standard multipliers of the input price.
    #[test]
    fn costs_price_each_class() {
        let p = Pricing {
            input_per_m_tok: 4.0,
            output_per_m_tok: 20.0,
            ..Default::default()
        };
        let u = usage(1_000_000, 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((u.cost(&p) - (4.0 + 5.0 + 8.0 + 0.4 + 20.0)).abs() < 1e-9);
        let p = Pricing {
            cache_read_per_m_tok: Some(0.2),
            cache_write_per_m_tok: Some(6.0),
            cache_write_1h_per_m_tok: Some(9.0),
            ..p
        };
        assert!((u.cost(&p) - (4.0 + 6.0 + 9.0 + 0.2 + 20.0)).abs() < 1e-9);
    }
}
