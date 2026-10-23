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
use super::config::ProviderConfig;
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
    /// the provider's own token count of this request, through the same transport, key
    /// and outbound policy as the attempt; `None` when the provider's kind has no
    /// counting endpoint
    pub provider_count: Option<&'a dyn Fn() -> Result<u64, CallError>>,
}

/// The tokens an answered attempt used, as the provider reported them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
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
        self.settle(Some(Usage {
            input: 0,
            output: 0,
        }));
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
}
