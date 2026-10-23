//! Token budgets of a build (spec C22 §3.5): an upper bound on each HTTP attempt's input
//! from the first available source, a reservation of that bound plus the output limit
//! before the attempt is sent, and the settlement of the reservation with the usage the
//! provider reported.
//!
//! The sources, in order:
//!
//! 1. an exact local tokenizer registered for the pair's model;
//! 2. the provider's own counting endpoint, through the same transport, key and send
//!    policy as the attempt, unless `budget.providerCounting` is off;
//! 3. the byte bound of [`byte_bound`].
//!
//! The budget is strict as long as the bounds hold. A reported usage above its
//! reservation is an accounting anomaly: the ledger counts the reported figure and the
//! build admits no further attempt.

use super::settings::{Budget, ByteBound};
use crate::models::CallError;
use crate::models::Kind;
use crate::models::admission::{Admission, Attempt, Settle, Ticket, Usage};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

/// The byte bound of a request: its serialized length in bytes times the kind's
/// multiplier, plus the framing allowance for each message and the system text, and the
/// allowance per request for what a provider adds around a schema or a tool. The
/// serialized body holds the text with JSON escapes and member names around it, so it
/// is never shorter than the text the model reads. Every kind's models are taken to
/// tokenize with a byte-level BPE or a SentencePiece vocabulary with byte fallback,
/// where every token of text covers at least one byte, which is why the default
/// multiplier is 1. The allowances are settings with unmeasured defaults
/// ([`ByteBound`]).
pub fn byte_bound(b: &ByteBound, bytes: &[u8], messages: usize) -> u64 {
    (bytes.len() as u64)
        .saturating_mul(b.multiplier)
        .saturating_add(b.per_message.saturating_mul(messages as u64 + 1))
        .saturating_add(b.per_request)
}

/// Where an input bound came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    ExactLocal,
    ProviderEndpoint,
    ByteBound,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::ExactLocal => "exact-local",
            Source::ProviderEndpoint => "provider-endpoint",
            Source::ByteBound => "byte-bound",
        }
    }

    /// Whether the count is exact rather than an upper bound.
    pub fn exact(self) -> bool {
        !matches!(self, Source::ByteBound)
    }
}

/// The input bound of one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputBound {
    pub tokens: u64,
    pub source: Source,
    /// the local tokenizer and its version, when one counted
    pub counter: Option<String>,
    pub provider: String,
    pub model: String,
    /// SHA-256 of the URL and the serialized body, so the bound names its request
    pub request_digest: [u8; 32],
}

/// An exact local tokenizer for one model.
pub trait LocalTokenizer: Send + Sync {
    /// the tokenizer's name and version
    fn identity(&self) -> String;
    /// The exact input tokens of the request body as the provider of `kind` renders it,
    /// or `None` when it cannot count this request exactly.
    fn count(&self, kind: Kind, body: &Value) -> Option<u64>;
}

/// The local tokenizers known for (provider, model) pairs.
#[derive(Clone, Default)]
pub struct Tokenizers {
    by_pair: HashMap<(String, String), Arc<dyn LocalTokenizer>>,
}

impl Tokenizers {
    pub fn insert(&mut self, provider: &str, model: &str, t: Arc<dyn LocalTokenizer>) {
        self.by_pair
            .insert((provider.to_string(), model.to_string()), t);
    }

    fn get(&self, provider: &str, model: &str) -> Option<&Arc<dyn LocalTokenizer>> {
        self.by_pair.get(&(provider.to_string(), model.to_string()))
    }
}

/// The daily ledger of a dataset, kept with its GraphRAG state so that a restart does
/// not reset it. Days are UTC days since the epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DayLedger {
    pub day: u64,
    /// tokens settled or still reserved that day
    pub used: u64,
}

/// A reported usage above its reservation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Anomaly {
    pub provider: String,
    pub model: String,
    pub source: Source,
    pub reserved: u64,
    pub reported: u64,
}

/// Why an attempt was not admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// the reservation does not fit in the build's or the day's budget
    Budget {
        needed: u64,
        left: u64,
        which: &'static str,
    },
    /// an earlier attempt of this build used more than its reservation
    Anomaly(Anomaly),
}

impl Refusal {
    fn into_call_error(self) -> CallError {
        match self {
            Refusal::Budget {
                needed,
                left,
                which,
            } => CallError::Admission(
                "budget-exceeded",
                format!(
                    "the request needs a reservation of {needed} tokens and {left} remain in {which}"
                ),
                false,
            ),
            Refusal::Anomaly(a) => CallError::Admission(
                "accounting-anomaly",
                format!(
                    "provider {} reported {} tokens for a request that reserved {}, so the build sends no further request",
                    a.provider, a.reported, a.reserved
                ),
                false,
            ),
        }
    }
}

struct Inner {
    build_cap: u64,
    build_used: u64,
    day_cap: u64,
    anomaly: Option<Anomaly>,
}

/// The ledger of one build, with the dataset's daily ledger that concurrent builds and
/// refreshes share.
pub struct Ledger {
    inner: Mutex<Inner>,
    day: Arc<Mutex<DayLedger>>,
    today: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl Ledger {
    pub fn new(
        tokens_per_build: u64,
        tokens_per_day: u64,
        day: Arc<Mutex<DayLedger>>,
        today: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Ledger {
        Ledger {
            inner: Mutex::new(Inner {
                build_cap: tokens_per_build,
                build_used: 0,
                day_cap: tokens_per_day,
                anomaly: None,
            }),
            day,
            today: Box::new(today),
        }
    }

    /// Tokens settled or reserved by this build.
    pub fn build_used(&self) -> u64 {
        self.inner.lock().build_used
    }

    pub fn anomaly(&self) -> Option<Anomaly> {
        self.inner.lock().anomaly.clone()
    }

    /// Reserve `tokens` in both budgets at once, or neither.
    fn reserve(&self, tokens: u64) -> Result<u64, Refusal> {
        let mut inner = self.inner.lock();
        if let Some(a) = &inner.anomaly {
            return Err(Refusal::Anomaly(a.clone()));
        }
        let mut day = self.day.lock();
        let today = (self.today)();
        if day.day != today {
            *day = DayLedger {
                day: today,
                used: 0,
            };
        }
        let build_left = inner.build_cap.saturating_sub(inner.build_used);
        if tokens > build_left {
            return Err(Refusal::Budget {
                needed: tokens,
                left: build_left,
                which: "budget.tokensPerBuild",
            });
        }
        let day_left = inner.day_cap.saturating_sub(day.used);
        if tokens > day_left {
            return Err(Refusal::Budget {
                needed: tokens,
                left: day_left,
                which: "budget.tokensPerDay",
            });
        }
        inner.build_used += tokens;
        day.used += tokens;
        Ok(today)
    }

    /// Replace a reservation made on `day` by the reported usage.
    fn settle(&self, bound: &InputBound, reserved: u64, day: u64, usage: Usage) {
        let actual = usage.input.saturating_add(usage.output);
        let mut inner = self.inner.lock();
        let mut d = self.day.lock();
        let same_day = d.day == day;
        if actual <= reserved {
            let credit = reserved - actual;
            inner.build_used -= credit;
            // a reservation of a day that has ended stays with that day
            if same_day {
                d.used = d.used.saturating_sub(credit);
            }
            return;
        }
        let excess = actual - reserved;
        inner.build_used = inner.build_used.saturating_add(excess);
        if same_day || d.day == (self.today)() {
            d.used = d.used.saturating_add(excess);
        }
        tracing::warn!(
            target: "sparkles::graphrag",
            "accounting anomaly: provider {} reported {actual} tokens for a request that reserved {reserved} ({})",
            bound.provider,
            bound.source.as_str()
        );
        inner.anomaly.get_or_insert(Anomaly {
            provider: bound.provider.clone(),
            model: bound.model.clone(),
            source: bound.source,
            reserved,
            reported: actual,
        });
    }
}

/// The admission of a build's model attempts.
pub struct BuildAdmission {
    pub ledger: Arc<Ledger>,
    tokenizers: Tokenizers,
    provider_counting: bool,
    budget: Budget,
    /// the bound of every admitted attempt, in order
    bounds: Mutex<Vec<InputBound>>,
}

impl BuildAdmission {
    pub fn new(ledger: Arc<Ledger>, tokenizers: Tokenizers, budget: &Budget) -> Self {
        BuildAdmission {
            ledger,
            tokenizers,
            provider_counting: budget.provider_counting,
            budget: budget.clone(),
            bounds: Mutex::new(Vec::new()),
        }
    }

    pub fn bounds(&self) -> Vec<InputBound> {
        self.bounds.lock().clone()
    }

    /// The input bound of an attempt, from the first available source.
    pub fn bound(&self, a: &Attempt<'_>) -> InputBound {
        let mut h = Sha256::new();
        h.update(a.url.as_bytes());
        h.update([0u8]);
        h.update(a.bytes);
        let digest: [u8; 32] = h.finalize().into();
        let make = |tokens, source, counter| InputBound {
            tokens,
            source,
            counter,
            provider: a.provider.to_string(),
            model: a.request.model.clone(),
            request_digest: digest,
        };
        if let Some(t) = self.tokenizers.get(a.provider, &a.request.model)
            && let Some(n) = t.count(a.config.kind, a.body)
        {
            return make(n, Source::ExactLocal, Some(t.identity()));
        }
        if self.provider_counting
            && let Some(count) = a.provider_count
        {
            match count() {
                Ok(n) => return make(n, Source::ProviderEndpoint, None),
                Err(e) => tracing::info!(
                    target: "sparkles::graphrag",
                    "provider {}: token count failed, using the byte bound: {e}",
                    a.provider
                ),
            }
        }
        make(
            byte_bound(
                &self.budget.byte_bound_of(a.config.kind.as_str()),
                a.bytes,
                a.request.messages.len(),
            ),
            Source::ByteBound,
            None,
        )
    }
}

struct Reserved {
    ledger: Arc<Ledger>,
    bound: InputBound,
    tokens: u64,
    day: u64,
}

impl Settle for Reserved {
    fn reported(self: Box<Self>, usage: Usage) {
        self.ledger
            .settle(&self.bound, self.tokens, self.day, usage);
    }

    fn unknown(self: Box<Self>) {
        // the whole reservation stays counted
    }
}

impl Admission for BuildAdmission {
    fn admit(&self, a: &Attempt<'_>) -> Result<Ticket, CallError> {
        if let Some(an) = self.ledger.anomaly() {
            return Err(Refusal::Anomaly(an).into_call_error());
        }
        let bound = self.bound(a);
        let tokens = bound
            .tokens
            .saturating_add(u64::from(a.request.max_output_tokens));
        let day = self
            .ledger
            .reserve(tokens)
            .map_err(Refusal::into_call_error)?;
        self.bounds.lock().push(bound.clone());
        Ok(Ticket::new(Box::new(Reserved {
            ledger: self.ledger.clone(),
            bound,
            tokens,
            day,
        })))
    }
}

#[cfg(test)]
mod tests;
