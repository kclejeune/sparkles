//! Guarded construction of canonical exact numbers. BigUint is construction scratch;
//! retained values have immutable, shared u32 limbs and a last-alias allocation token.
use super::data::Kind;
use super::{Error, Limits, poll};
use num_bigint::BigUint;
use num_integer::Integer;
use num_traits::{One, Zero};
use parking_lot::Mutex;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::mem::size_of;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
};

fn overflow() -> Error {
    Error::Memory {
        limit: usize::MAX,
        used: usize::MAX,
    }
}
fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or_else(overflow)
}
fn mul(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_mul(b).ok_or_else(overflow)
}
fn align64(n: usize) -> Result<usize, Error> {
    Ok(add(n, 63)? & !63)
}

/// Live payload storage, independent of its normalization/index/context owners.
#[derive(Debug, Default)]
pub(super) struct NumericLedger {
    live: AtomicUsize,
}
impl NumericLedger {
    pub fn bytes(&self) -> usize {
        self.live.load(AtomicOrdering::Relaxed)
            + align64(size_of::<Self>() + 2 * size_of::<usize>()).unwrap()
    }
}
#[derive(Debug)]
struct Token {
    ledger: Arc<NumericLedger>,
    bytes: usize,
}
impl Drop for Token {
    fn drop(&mut self) {
        self.ledger
            .live
            .fetch_sub(self.bytes, AtomicOrdering::Relaxed);
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Point {
    Lexical,
    LexicalFinal,
    Digit,
    Gcd,
    Factor,
    Publish,
    IndexCopy,
}
#[cfg(test)]
type TestHook = dyn Fn(Point) -> Result<(), Error> + Send + Sync;

/// One build's aggregate admission. The lock serializes grant/peak transitions, never
/// arithmetic. Payloads retain their ledger, not this build's callbacks.
pub(super) struct Budget {
    limits: Limits,
    floor: usize,
    ledger: Mutex<Option<Arc<NumericLedger>>>,
    held: Mutex<Held>,
    /// An insertion exceeded its preallocation plan. Retained storage stays counted,
    /// and no later operation may request another grant from this build.
    bound_miss: AtomicUsize,
    #[cfg(test)]
    pub polls: AtomicUsize,
    #[cfg(test)]
    hook: Mutex<Option<Arc<TestHook>>>,
}
#[derive(Default)]
struct Held {
    base: usize,
    scratch: usize,
    peak: usize,
}
impl Budget {
    #[cfg(test)]
    pub fn set_hook(&self, hook: Arc<TestHook>) {
        *self.hook.lock() = Some(hook);
    }
    #[cfg(test)]
    pub(super) fn at(&self, point: Point) -> Result<(), Error> {
        self.poll()?;
        let hook = self.hook.lock().clone();
        if let Some(hook) = hook {
            hook(point)?;
        }
        Ok(())
    }

    pub fn new(limits: &Limits, base: usize, ledger: Option<Arc<NumericLedger>>) -> Arc<Self> {
        Arc::new(Self {
            limits: limits.clone(),
            floor: base,
            ledger: Mutex::new(ledger),
            held: Mutex::new(Held {
                base,
                ..Held::default()
            }),
            bound_miss: AtomicUsize::new(0),
            #[cfg(test)]
            polls: AtomicUsize::new(0),
            #[cfg(test)]
            hook: Mutex::new(None),
        })
    }
    pub fn ledger(&self) -> Option<Arc<NumericLedger>> {
        self.ledger.lock().clone()
    }
    pub fn peak(&self) -> usize {
        self.held.lock().peak
    }
    #[cfg(test)]
    pub(super) fn retained_storage(&self) -> (usize, usize) {
        let h = self.held.lock();
        (h.base, h.scratch)
    }
    pub fn poll(&self) -> Result<(), Error> {
        #[cfg(test)]
        self.polls.fetch_add(1, AtomicOrdering::Relaxed);
        self.check_halt()?;
        poll(&self.limits)
    }
    fn check_halt(&self) -> Result<(), Error> {
        let used = self.bound_miss.load(AtomicOrdering::Relaxed);
        if used != 0 {
            return Err(Error::Memory {
                limit: self.limits.max_memory.unwrap_or(usize::MAX),
                used,
            });
        }
        Ok(())
    }
    fn admit(&self, held: &mut Held, extra: usize) -> Result<(), Error> {
        self.poll()?;
        self.account(held, extra)
    }
    /// Checked admission without a redundant public work checkpoint. The terminal
    /// retained-plan halt still prevents any subsequent grant.
    fn account(&self, held: &mut Held, extra: usize) -> Result<(), Error> {
        self.check_halt()?;
        let live = self.ledger.lock().as_ref().map_or(0, |l| l.bytes());
        let used = add(add(add(held.base, held.scratch)?, live)?, extra)?;
        if self.limits.max_memory.is_some_and(|m| used > m)
            || self
                .limits
                .charge
                .as_ref()
                .is_some_and(|f| used > held.peak && !f(used - held.peak))
        {
            return Err(Error::Memory {
                limit: self.limits.max_memory.unwrap_or(usize::MAX),
                used,
            });
        }
        held.peak = held.peak.max(used);
        Ok(())
    }
    /// Base excludes the ledger, which is counted once here.
    pub fn baseline(&self, base: usize) -> Result<(), Error> {
        let mut h = self.held.lock();
        h.base = add(self.floor, base)?;
        self.admit(&mut h, 0)
    }
    /// Refresh serial normalization's accounting before each logical axiom. Its
    /// original periodic/phase polls and all actual numeric/storage work keep their
    /// public cancellation and deadline checkpoints.
    pub fn refresh_base(&self, base: usize) -> Result<(), Error> {
        let mut h = self.held.lock();
        self.check_halt()?;
        h.base = add(self.floor, base)?;
        self.account(&mut h, 0)
    }
    pub fn grow_base(&self, bytes: usize) -> Result<(), Error> {
        let mut h = self.held.lock();
        h.base = add(h.base, bytes)?;
        self.admit(&mut h, 0)
    }
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Result<Reservation, Error> {
        let mut h = self.held.lock();
        self.admit(&mut h, bytes)?;
        h.scratch = add(h.scratch, bytes)?;
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
    fn token(&self, bytes: usize) -> Result<Token, Error> {
        let mut h = self.held.lock();
        let header = if self.ledger.lock().is_none() {
            align64(size_of::<NumericLedger>() + 2 * size_of::<usize>())?
        } else {
            0
        };
        self.admit(&mut h, add(bytes, header)?)?;
        let mut slot = self.ledger.lock();
        let ledger = slot
            .get_or_insert_with(|| Arc::new(NumericLedger::default()))
            .clone();
        ledger.live.fetch_add(bytes, AtomicOrdering::Relaxed);
        Ok(Token { ledger, bytes })
    }
}
pub(super) struct Reservation {
    budget: Arc<Budget>,
    bytes: usize,
}
impl Reservation {
    /// Storage allocated under this reservation has become part of the retained
    /// baseline. Transfer before polling cancellation, so an error cannot uncharge
    /// storage that remains alive. This never calls the caller's grant callback.
    pub fn retain(mut self, bytes: Option<usize>) -> Result<(), Error> {
        let mut h = self.budget.held.lock();
        let retained = bytes.unwrap_or(usize::MAX);
        let base = h.base.checked_add(retained);
        let missed = bytes.is_none() || retained > self.bytes || base.is_none();
        h.base = base.unwrap_or(usize::MAX);
        h.scratch -= self.bytes;
        self.bytes = 0;
        if missed {
            let live = self.budget.ledger.lock().as_ref().map_or(0, |l| l.bytes());
            let used = h.base.saturating_add(h.scratch).saturating_add(live);
            self.budget
                .bound_miss
                .store(used.max(1), AtomicOrdering::Relaxed);
        }
        drop(h);
        self.budget.poll()
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.budget.held.lock().scratch -= self.bytes;
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Membership {
    integer: bool,
    decimal: bool,
    nonnegative: bool,
}
impl Membership {
    fn is_a(self, k: Kind) -> bool {
        match k {
            Kind::Real | Kind::Rational => true,
            Kind::Decimal => self.decimal,
            Kind::Integer => self.integer,
            Kind::NonNegativeInteger => self.nonnegative,
            _ => false,
        }
    }
}
#[derive(Debug)]
pub struct CanonicalBigNumber {
    negative: bool,
    numerator: Box<[u32]>,
    denominator: Box<[u32]>,
    membership: Membership,
    _token: Token,
}
/// Small and large forms are unique after reduction. Identity deliberately excludes
/// metadata and allocation tokens. Ordering retains numerator-then-denominator order.
#[derive(Debug)]
pub enum Number {
    Small {
        n: i128,
        d: i128,
        membership: Membership,
    },
    Big(Arc<CanonicalBigNumber>),
}
impl Clone for Number {
    fn clone(&self) -> Self {
        match self {
            Self::Small { n, d, membership } => Self::Small {
                n: *n,
                d: *d,
                membership: *membership,
            },
            Self::Big(n) => Self::Big(n.clone()),
        }
    }
}
impl Number {
    pub fn is_a(&self, k: Kind) -> bool {
        match self {
            Self::Small { membership, .. } => membership.is_a(k),
            Self::Big(b) => b.membership.is_a(k),
        }
    }
}
impl PartialEq for Number {
    fn eq(&self, b: &Self) -> bool {
        self.cmp(b) == Ordering::Equal
    }
}
impl Eq for Number {}
impl PartialOrd for Number {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        Some(self.cmp(b))
    }
}
fn limb_cmp(a: &[u32], b: &[u32]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}
fn small_limbs(n: u128) -> ([u32; 4], usize) {
    let a = [
        n as u32,
        (n >> 32) as u32,
        (n >> 64) as u32,
        (n >> 96) as u32,
    ];
    let len = a.iter().rposition(|&v| v != 0).map_or(0, |p| p + 1);
    (a, len)
}
impl Ord for Number {
    fn cmp(&self, b: &Self) -> Ordering {
        match (self, b) {
            (Self::Small { n, d, .. }, Self::Small { n: m, d: e, .. }) => {
                n.cmp(m).then_with(|| d.cmp(e))
            }
            (Self::Big(a), Self::Big(b)) => b
                .negative
                .cmp(&a.negative)
                .then_with(|| {
                    let c = limb_cmp(&a.numerator, &b.numerator);
                    if a.negative { c.reverse() } else { c }
                })
                .then_with(|| limb_cmp(&a.denominator, &b.denominator)),
            (Self::Small { n, d, .. }, Self::Big(b)) => {
                let (a, len) = small_limbs(n.unsigned_abs());
                let (den, dl) = small_limbs(*d as u128);
                b.negative
                    .cmp(&(*n < 0))
                    .then_with(|| {
                        let c = limb_cmp(&a[..len], &b.numerator);
                        if *n < 0 { c.reverse() } else { c }
                    })
                    .then_with(|| limb_cmp(&den[..dl], &b.denominator))
            }
            (Self::Big(_), Self::Small { .. }) => b.cmp(self).reverse(),
        }
    }
}
impl Hash for Number {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Self::Small { n, d, .. } => {
                0u8.hash(h);
                n.hash(h);
                d.hash(h);
            }
            Self::Big(b) => {
                1u8.hash(h);
                b.negative.hash(h);
                b.numerator.hash(h);
                b.denominator.hash(h);
            }
        }
    }
}

/// W from the reviewed resource contract, in checked requested-storage bytes.
pub(super) fn workspace(n_digits: usize, d_digits: usize) -> Result<usize, Error> {
    let limbs = |s| -> Result<usize, Error> { Ok(add(add(mul(4, s)?, 4)?, 31)? / 32) };
    let l = add(add(limbs(n_digits)?, limbs(d_digits)?)?, 8)?;
    let x = add(l, 2)?;
    let log = usize::BITS as usize - (x - 1).leading_zeros() as usize;
    add(65536, mul(mul(8192, add(l, 32)?)?, add(4, mul(2, log)?)?)?)
}

/// Borrowed spans only. Every scan, including invalid forms and insignificant zeros,
/// polls no later than 256 inspected bytes. Decimal scale excludes trailing zeros.
struct Parts<'a> {
    negative: bool,
    a: &'a [u8],
    b: &'a [u8],
    den: &'a [u8],
    scale: usize,
    significant: usize,
    den_significant: usize,
}
fn scan<'a>(s: &'a str, k: Kind, guard: &Budget) -> Result<Option<Parts<'a>>, Error> {
    guard.poll()?;
    let bytes = s.as_bytes();
    let negative = bytes.first() == Some(&b'-');
    let start = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut delimiter = None;
    let mut n_digits = 0usize;
    let mut den_digits = 0usize;
    let mut significant = 0usize;
    let mut den_significant = 0usize;
    let mut last_nonzero = None;
    let mut valid = start < bytes.len();
    for (i, &c) in bytes.iter().enumerate().skip(start) {
        if (i - start).is_multiple_of(256) {
            guard.poll()?;
            #[cfg(test)]
            if i > start {
                guard.at(Point::Lexical)?;
            }
        }
        if c.is_ascii_digit() {
            if k == Kind::Rational && delimiter.is_some() {
                den_digits += 1;
                if c != b'0' || den_significant != 0 {
                    den_significant += 1;
                }
            } else {
                n_digits += 1;
                if c != b'0' || significant != 0 {
                    significant += 1;
                }
                if c != b'0' {
                    last_nonzero = Some(i);
                }
            }
        } else if delimiter.is_none()
            && ((k == Kind::Decimal && c == b'.') || (k == Kind::Rational && c == b'/'))
        {
            delimiter = Some(i);
        } else {
            valid = false;
        }
    }
    guard.poll()?;
    #[cfg(test)]
    guard.at(Point::LexicalFinal)?;
    valid &= n_digits > 0;
    if k == Kind::Rational {
        valid &= delimiter.is_some() && den_digits > 0 && den_significant > 0;
    }
    if k == Kind::NonNegativeInteger {
        valid &= !negative || significant == 0;
    }
    if !valid {
        return Ok(None);
    }
    let mut p = Parts {
        negative: negative && significant != 0,
        a: &bytes[start..],
        b: &[],
        den: &[],
        scale: 0,
        significant,
        den_significant,
    };
    if let Some(at) = delimiter {
        p.a = &bytes[start..at];
        if k == Kind::Rational {
            p.den = &bytes[at + 1..];
        } else {
            let end = last_nonzero.map_or(at + 1, |i| (i + 1).max(at + 1));
            p.b = &bytes[at + 1..end];
            p.scale = p.b.len();
            p.significant = significant.saturating_sub(bytes.len() - end);
            p.den_significant = add(p.scale, 1)?;
        }
    }
    Ok(Some(p))
}
fn small_unsigned(parts: &[&[u8]], guard: &Budget) -> Result<Option<u128>, Error> {
    let mut n = 0u128;
    let mut i = 0usize;
    for part in parts {
        for &c in *part {
            if i.is_multiple_of(256) {
                guard.poll()?;
            }
            i += 1;
            let Some(next) = n
                .checked_mul(10)
                .and_then(|v| v.checked_add((c - b'0') as u128))
            else {
                return Ok(None);
            };
            n = next;
        }
    }
    Ok(Some(n))
}
fn signed(n: u128, negative: bool) -> Option<i128> {
    if negative && n == (1u128 << 127) {
        Some(i128::MIN)
    } else {
        i128::try_from(n)
            .ok()
            .map(|v| if negative { -v } else { v })
    }
}
fn small(n: u128, d: u128, negative: bool, guard: &Budget) -> Result<Option<Number>, Error> {
    let (mut a, mut b) = (n, d);
    while b != 0 {
        guard.poll()?;
        (a, b) = (b, a % b);
        guard.poll()?;
    }
    let (n, d) = (n / a, d / a);
    let (Some(n), Ok(d)) = (signed(n, negative), i128::try_from(d)) else {
        return Ok(None);
    };
    let mut factor = d;
    for p in [2, 5] {
        while factor % p == 0 {
            guard.poll()?;
            factor /= p;
            guard.poll()?;
        }
    }
    Ok(Some(Number::Small {
        n,
        d,
        membership: Membership {
            integer: d == 1,
            decimal: factor == 1,
            nonnegative: d == 1 && n >= 0,
        },
    }))
}
fn accumulate(parts: &[&[u8]], guard: &Budget) -> Result<BigUint, Error> {
    guard.poll()?;
    let mut out = BigUint::zero();
    for part in parts {
        for &c in *part {
            guard.poll()?;
            out *= 10u32;
            guard.poll()?;
            out += (c - b'0') as u32;
            guard.poll()?;
            #[cfg(test)]
            guard.at(Point::Digit)?;
        }
    }
    Ok(out)
}
fn clone_big(n: &BigUint, guard: &Budget) -> Result<BigUint, Error> {
    guard.poll()?;
    let r = n.clone();
    guard.poll()?;
    Ok(r)
}
fn divide(n: &BigUint, d: &BigUint, guard: &Budget) -> Result<(BigUint, BigUint), Error> {
    guard.poll()?;
    let r = n.div_rem(d);
    guard.poll()?;
    Ok(r)
}
fn small_magnitude(n: &BigUint) -> Option<u128> {
    let mut digits = n.iter_u32_digits();
    let mut out = 0u128;
    for i in 0..4 {
        if let Some(d) = digits.next() {
            out |= (d as u128) << (32 * i);
        } else {
            return Some(out);
        }
    }
    digits.next().is_none().then_some(out)
}
fn extract(n: &BigUint, guard: &Budget) -> Result<Box<[u32]>, Error> {
    guard.poll()?;
    let len = n.iter_u32_digits().len();
    let mut out = Vec::with_capacity(len);
    for (i, limb) in n.iter_u32_digits().enumerate() {
        if i.is_multiple_of(64) {
            guard.poll()?;
        }
        out.push(limb);
    }
    guard.poll()?;
    Ok(out.into_boxed_slice())
}
pub(super) fn parse(s: &str, k: Kind, guard: &Arc<Budget>) -> Result<Option<Number>, Error> {
    let Some(p) = scan(s, k, guard)? else {
        return Ok(None);
    };
    if p.significant == 0 {
        return Ok(Some(Number::Small {
            n: 0,
            d: 1,
            membership: Membership {
                integer: true,
                decimal: true,
                nonnegative: true,
            },
        }));
    }
    let n = small_unsigned(&[p.a, p.b], guard)?;
    let d = if p.den.is_empty() {
        u32::try_from(p.scale)
            .ok()
            .and_then(|e| 10u128.checked_pow(e))
    } else {
        small_unsigned(&[p.den], guard)?
    };
    if let (Some(n), Some(d)) = (n, d)
        && let Some(n) = small(n, d, p.negative, guard)?
    {
        return Ok(Some(n));
    }
    let _scratch = guard.reserve(workspace(p.significant, p.den_significant.max(1))?)?;
    let n = accumulate(&[p.a, p.b], guard)?;
    let d = if p.den.is_empty() {
        guard.poll()?;
        let mut d = BigUint::one();
        guard.poll()?;
        for _ in 0..p.scale {
            guard.poll()?;
            d *= 10u32;
            guard.poll()?;
        }
        d
    } else {
        accumulate(&[p.den], guard)?
    };
    let (mut a, mut b) = (clone_big(&n, guard)?, clone_big(&d, guard)?);
    while !b.is_zero() {
        let (_, rem) = divide(&a, &b, guard)?;
        #[cfg(test)]
        guard.at(Point::Gcd)?;
        a = b;
        b = rem;
    }
    let (n, _) = divide(&n, &a, guard)?;
    let (d, _) = divide(&d, &a, guard)?;
    guard.poll()?;
    if let (Some(n), Some(d)) = (small_magnitude(&n), small_magnitude(&d))
        && let Some(n) = small(n, d, p.negative, guard)?
    {
        return Ok(Some(n));
    }
    let mut factor = clone_big(&d, guard)?;
    for prime in [2u32, 5] {
        guard.poll()?;
        let mut divisor = BigUint::one();
        guard.poll()?;
        divisor *= prime;
        guard.poll()?;
        loop {
            let (q, r) = divide(&factor, &divisor, guard)?;
            #[cfg(test)]
            guard.at(Point::Factor)?;
            if !r.is_zero() {
                break;
            }
            factor = q;
        }
    }
    let membership = Membership {
        integer: d.is_one(),
        decimal: factor.is_one(),
        nonnegative: d.is_one() && !p.negative,
    };
    guard.poll()?;
    let nlen = n.iter_u32_digits().len();
    let dlen = d.iter_u32_digits().len();
    guard.poll()?;
    let bytes = add(
        add(
            align64(add(
                size_of::<CanonicalBigNumber>(),
                2 * size_of::<usize>(),
            )?)?,
            align64(mul(4, nlen)?)?,
        )?,
        align64(mul(4, dlen)?)?,
    )?;
    #[cfg(test)]
    guard.at(Point::Publish)?;
    let token = guard.token(bytes)?;
    let numerator = extract(&n, guard)?;
    let denominator = extract(&d, guard)?;
    guard.poll()?;
    let result = Number::Big(Arc::new(CanonicalBigNumber {
        negative: p.negative,
        numerator,
        denominator,
        membership,
        _token: token,
    }));
    guard.poll()?;
    Ok(Some(result))
}

#[cfg(test)]
#[path = "numeric_tests.rs"]
pub(super) mod tests;
