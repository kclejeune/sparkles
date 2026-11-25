//! Bounded arbitrary-precision integers and decimals for SPARQL numeric values beyond
//! the fixed-width `i64` / `oxsdatatypes::Decimal` ranges (roadmap item 1065).
//!
//! Ordinary values stay on `Value::Integer` and `Value::Decimal`; this module is used
//! only as a cold fallback when a valid `xsd:integer`, derived integer, or `xsd:decimal`
//! literal or arithmetic result exceeds those fixed widths.
//!
//! Both types store magnitudes in base 10^9 (`u32` limbs, little-endian) so decimal
//! parsing, formatting, scaling, rounding, and exact order comparisons are linear in the
//! digit count. Digit count and fractional scale are bounded by [`MAX_DIGITS`] and
//! [`MAX_SCALE`] so malicious literals or repeated multiplications cannot exhaust memory
//! or CPU.

use oxsdatatypes::Decimal;
use std::cmp::Ordering;
use std::fmt;

/// Radix of each limb: 10^9 fits in `u32`, and products of two limbs fit in `u64`.
const BASE: u32 = 1_000_000_000;
const BASE_U64: u64 = 1_000_000_000;
const BASE_U128: u128 = 1_000_000_000;
const BASE_DIGITS: usize = 9;
const POW10: [u32; 10] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
];

/// Maximum significant decimal digits retained by a [`BigInt`] or [`BigDec`].
pub const MAX_DIGITS: usize = 4_096;
/// Maximum fractional decimal scale retained by a [`BigDec`].
pub const MAX_SCALE: u32 = 4_096;
/// Minimum fractional digits produced by a non-terminating [`BigDec`] division, matching
/// `oxsdatatypes::Decimal`.
const MIN_DIV_SCALE: u32 = 18;
/// Raw scale of `oxsdatatypes::Decimal` (`raw * 10^-18`).
const OXS_DEC_SCALE: u32 = 18;

/// Signed arbitrary-precision integer in base 10^9, bounded to [`MAX_DIGITS`] decimal
/// digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BigInt {
    /// True only when strictly negative (`limbs` non-empty).
    neg: bool,
    /// Little-endian base-10^9 limbs with no trailing zeros. Empty means zero.
    limbs: Vec<u32>,
}

/// Signed arbitrary-precision decimal `mantissa * 10^(-scale)`, normalized so that
/// `scale == 0` whenever `mantissa` is zero or divisible by 10.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BigDec {
    mantissa: BigInt,
    scale: u32,
}

#[inline]
fn digits_u32(mut n: u32) -> usize {
    debug_assert!(n > 0 && n < BASE);
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

fn trim_limbs(limbs: &mut Vec<u32>) {
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
}

fn limbs_digits(limbs: &[u32]) -> usize {
    match limbs.last() {
        None => 0,
        Some(&top) => (limbs.len() - 1) * BASE_DIGITS + digits_u32(top),
    }
}

fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let n = a.len().max(b.len());
    let mut out = Vec::with_capacity(n + 1);
    let mut carry: u64 = 0;
    for i in 0..n {
        let s = u64::from(*a.get(i).unwrap_or(&0)) + u64::from(*b.get(i).unwrap_or(&0)) + carry;
        if s >= BASE_U64 {
            out.push((s - BASE_U64) as u32);
            carry = 1;
        } else {
            out.push(s as u32);
            carry = 0;
        }
    }
    if carry > 0 {
        out.push(carry as u32);
    }
    out
}

/// Subtract `b` from `a`, requiring `a >= b`.
fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow: i64 = 0;
    for (i, &ai) in a.iter().enumerate() {
        let d = i64::from(ai) - i64::from(*b.get(i).unwrap_or(&0)) - borrow;
        if d < 0 {
            out.push((d + BASE_U64 as i64) as u32);
            borrow = 1;
        } else {
            out.push(d as u32);
            borrow = 0;
        }
    }
    debug_assert_eq!(borrow, 0);
    trim_limbs(&mut out);
    out
}

fn mul_mag_u32(a: &[u32], k: u32) -> Vec<u32> {
    if a.is_empty() || k == 0 {
        return Vec::new();
    }
    if k == 1 {
        return a.to_vec();
    }
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry: u64 = 0;
    let k = u64::from(k);
    for &limb in a {
        let p = u64::from(limb) * k + carry;
        out.push((p % BASE_U64) as u32);
        carry = p / BASE_U64;
    }
    if carry > 0 {
        out.push(carry as u32);
    }
    out
}

fn div_rem_mag_u32(a: &[u32], k: u32) -> (Vec<u32>, u32) {
    debug_assert!(k > 0);
    if a.is_empty() {
        return (Vec::new(), 0);
    }
    if k == 1 {
        return (a.to_vec(), 0);
    }
    let mut q = vec![0u32; a.len()];
    let mut rem: u64 = 0;
    let k = u64::from(k);
    for i in (0..a.len()).rev() {
        let cur = rem * BASE_U64 + u64::from(a[i]);
        q[i] = (cur / k) as u32;
        rem = cur % k;
    }
    trim_limbs(&mut q);
    (q, rem as u32)
}

fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &ai) in a.iter().enumerate() {
        if ai == 0 {
            continue;
        }
        let mut carry: u64 = 0;
        let ai = u64::from(ai);
        for (j, &bj) in b.iter().enumerate() {
            let cur = u64::from(out[i + j]) + ai * u64::from(bj) + carry;
            out[i + j] = (cur % BASE_U64) as u32;
            carry = cur / BASE_U64;
        }
        let mut idx = i + b.len();
        while carry > 0 {
            let cur = u64::from(out[idx]) + carry;
            out[idx] = (cur % BASE_U64) as u32;
            carry = cur / BASE_U64;
            idx += 1;
        }
    }
    trim_limbs(&mut out);
    out
}

/// Unsigned long division in base 10^9 (Knuth Algorithm D). `v` must be non-empty.
fn div_rem_mag(u: &[u32], v: &[u32]) -> (Vec<u32>, Vec<u32>) {
    debug_assert!(!v.is_empty());
    match cmp_mag(u, v) {
        Ordering::Less => return (Vec::new(), u.to_vec()),
        Ordering::Equal => return (vec![1], Vec::new()),
        Ordering::Greater => {}
    }
    if v.len() == 1 {
        let (q, r) = div_rem_mag_u32(u, v[0]);
        return (q, if r == 0 { Vec::new() } else { vec![r] });
    }
    let n = v.len();
    let m = u.len() - n;
    // Normalize so v'[n - 1] >= BASE / 2.
    let d = (BASE_U64 / (u64::from(v[n - 1]) + 1)) as u32;
    let mut un = mul_mag_u32(u, d);
    un.resize(m + n + 1, 0);
    let vn = mul_mag_u32(v, d);
    debug_assert_eq!(vn.len(), n);
    let v1 = u128::from(vn[n - 1]);
    let v2 = u128::from(vn[n - 2]);

    let mut q = vec![0u32; m + 1];
    for j in (0..=m).rev() {
        let num = u128::from(un[j + n]) * BASE_U128 + u128::from(un[j + n - 1]);
        let mut qhat = num / v1;
        let mut rhat = num % v1;
        if qhat >= BASE_U128 {
            qhat = BASE_U128 - 1;
            rhat += v1;
        }
        while rhat < BASE_U128 && qhat * v2 > rhat * BASE_U128 + u128::from(un[j + n - 2]) {
            qhat -= 1;
            rhat += v1;
        }
        let mut borrow: u64 = 0;
        let qh = qhat as u64;
        for i in 0..n {
            let prod = qh * u64::from(vn[i]) + borrow;
            let prod_lo = (prod % BASE_U64) as u32;
            borrow = prod / BASE_U64;
            if un[j + i] >= prod_lo {
                un[j + i] -= prod_lo;
            } else {
                un[j + i] = un[j + i] + BASE - prod_lo;
                borrow += 1;
            }
        }
        if u64::from(un[j + n]) < borrow {
            // Trial quotient was 1 too large; add vn back once.
            qhat -= 1;
            let mut carry: u64 = 0;
            for i in 0..n {
                let sum = u64::from(un[j + i]) + u64::from(vn[i]) + carry;
                if sum >= BASE_U64 {
                    un[j + i] = (sum - BASE_U64) as u32;
                    carry = 1;
                } else {
                    un[j + i] = sum as u32;
                    carry = 0;
                }
            }
            debug_assert_eq!(u64::from(un[j + n]) + carry, borrow);
            un[j + n] = 0;
        } else {
            un[j + n] -= borrow as u32;
        }
        q[j] = qhat as u32;
    }
    trim_limbs(&mut q);
    let mut rem_slice = un[..n].to_vec();
    trim_limbs(&mut rem_slice);
    let (r, rem_check) = div_rem_mag_u32(&rem_slice, d);
    debug_assert_eq!(rem_check, 0);
    (q, r)
}

fn shift_mag_pow10(a: &[u32], k: usize) -> Vec<u32> {
    if a.is_empty() || k == 0 {
        return a.to_vec();
    }
    let limb_shift = k / BASE_DIGITS;
    let rem = k % BASE_DIGITS;
    let mut scaled = mul_mag_u32(a, POW10[rem]);
    if limb_shift == 0 {
        return scaled;
    }
    let mut out = vec![0u32; limb_shift + scaled.len()];
    out[limb_shift..].copy_from_slice(&scaled);
    scaled.clear();
    out
}

/// Divide `a` by 10^k, returning `(quotient, remainder)`.
fn div_rem_mag_pow10(a: &[u32], k: usize) -> (Vec<u32>, Vec<u32>) {
    if a.is_empty() || k == 0 {
        return (a.to_vec(), Vec::new());
    }
    let limb_shift = k / BASE_DIGITS;
    let rem = k % BASE_DIGITS;
    if limb_shift >= a.len() {
        return (Vec::new(), a.to_vec());
    }
    let p = POW10[rem];
    let (q, top_rem) = div_rem_mag_u32(&a[limb_shift..], p);
    let mut r = Vec::with_capacity(limb_shift + 1);
    r.extend_from_slice(&a[..limb_shift]);
    if top_rem > 0 {
        r.push(top_rem);
    }
    trim_limbs(&mut r);
    (q, r)
}

/// Count trailing decimal zeros of a non-empty magnitude.
fn trailing_zeros_mag(a: &[u32]) -> usize {
    let mut zeros = 0;
    for &limb in a {
        if limb == 0 {
            zeros += BASE_DIGITS;
        } else {
            let mut x = limb;
            while x.is_multiple_of(10) {
                x /= 10;
                zeros += 1;
            }
            break;
        }
    }
    zeros
}

/// Decimal digit at position `idx` from the most significant digit (`0` = MSB) of a
/// non-empty magnitude with `total` decimal digits.
#[inline]
fn digit_from_msb(limbs: &[u32], total: usize, idx: usize) -> u8 {
    if idx >= total {
        return 0;
    }
    let pos_from_lsb = total - 1 - idx;
    let limb_idx = pos_from_lsb / BASE_DIGITS;
    let in_limb = pos_from_lsb % BASE_DIGITS;
    ((limbs[limb_idx] / POW10[in_limb]) % 10) as u8
}

/// Format up to `max_sig` significant decimal digits of `limbs` (with `total` digits) into
/// `out`, setting `sticky` if any dropped digit is nonzero.
fn write_sig_digits(limbs: &[u32], total: usize, max_sig: usize, out: &mut String) -> bool {
    let take = total.min(max_sig);
    for idx in 0..take {
        out.push(char::from(b'0' + digit_from_msb(limbs, total, idx)));
    }
    if take == total {
        return false;
    }
    // Check if any remaining digit below `take` is nonzero.
    let rem_digits = total - take;
    let full_limbs = rem_digits / BASE_DIGITS;
    let rem_in_limb = rem_digits % BASE_DIGITS;
    if limbs[..full_limbs].iter().any(|&l| l != 0) {
        return true;
    }
    if rem_in_limb > 0 && !limbs[full_limbs].is_multiple_of(POW10[rem_in_limb]) {
        return true;
    }
    false
}

/// Convert `(-1)^neg * limbs * 10^(-scale)` to exact IEEE-754 `f64`.
fn to_f64_exact(neg: bool, limbs: &[u32], scale: u32) -> f64 {
    if limbs.is_empty() {
        return 0.0;
    }
    let total = limbs_digits(limbs);
    // Value is in [10^(mag - 1), 10^mag).
    let mag = total as i64 - i64::from(scale);
    if mag > 310 {
        return if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    if mag < -326 {
        return if neg { -0.0 } else { 0.0 };
    }
    // 800 significant digits exceeds the 768-digit exact worst case for IEEE-754 f64
    // half-way ties, and a sticky digit preserves exact round-to-nearest-even.
    let mut s = String::with_capacity(820);
    if neg {
        s.push('-');
    }
    let sticky = write_sig_digits(limbs, total, 800, &mut s);
    let kept = total.min(800);
    let mut exp = total as i64 - kept as i64 - i64::from(scale);
    if sticky {
        s.push('1');
        exp -= 1;
    }
    if exp != 0 {
        use std::fmt::Write;
        let _ = write!(s, "e{exp}");
    }
    s.parse::<f64>().unwrap_or(if neg {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    })
}

/// Convert `(-1)^neg * limbs * 10^(-scale)` to exact IEEE-754 `f32` without double
/// rounding.
fn to_f32_exact(neg: bool, limbs: &[u32], scale: u32) -> f32 {
    if limbs.is_empty() {
        return 0.0;
    }
    let total = limbs_digits(limbs);
    let mag = total as i64 - i64::from(scale);
    if mag > 40 {
        return if neg {
            f32::NEG_INFINITY
        } else {
            f32::INFINITY
        };
    }
    if mag < -47 {
        return if neg { -0.0 } else { 0.0 };
    }
    // 130 significant digits exceeds the 112-digit exact worst case for IEEE-754 f32.
    let mut s = String::with_capacity(150);
    if neg {
        s.push('-');
    }
    let sticky = write_sig_digits(limbs, total, 130, &mut s);
    let kept = total.min(130);
    let mut exp = total as i64 - kept as i64 - i64::from(scale);
    if sticky {
        s.push('1');
        exp -= 1;
    }
    if exp != 0 {
        use std::fmt::Write;
        let _ = write!(s, "e{exp}");
    }
    s.parse::<f32>().unwrap_or(if neg {
        f32::NEG_INFINITY
    } else {
        f32::INFINITY
    })
}

/// Compare `a * 10^(-sa)` and `b * 10^(-sb)` (both unsigned, non-empty) without
/// allocating.
fn cmp_mag_scaled(a: &[u32], sa: u32, b: &[u32], sb: u32) -> Ordering {
    let da = limbs_digits(a);
    let db = limbs_digits(b);
    let mag_a = da as i64 - i64::from(sa);
    let mag_b = db as i64 - i64::from(sb);
    if mag_a != mag_b {
        return mag_a.cmp(&mag_b);
    }
    let n = da.max(db);
    for i in 0..n {
        let ca = digit_from_msb(a, da, i);
        let cb = digit_from_msb(b, db, i);
        if ca != cb {
            return ca.cmp(&cb);
        }
    }
    Ordering::Equal
}

fn parse_unsigned_digits(digits: &str) -> Vec<u32> {
    let stripped = digits.trim_start_matches('0');
    if stripped.is_empty() {
        return Vec::new();
    }
    let bytes = stripped.as_bytes();
    let mut limbs = Vec::with_capacity(bytes.len().div_ceil(BASE_DIGITS));
    let mut end = bytes.len();
    while end > 0 {
        let start = end.saturating_sub(BASE_DIGITS);
        let mut val: u32 = 0;
        for &b in &bytes[start..end] {
            val = val * 10 + u32::from(b - b'0');
        }
        limbs.push(val);
        end = start;
    }
    trim_limbs(&mut limbs);
    limbs
}

fn write_limbs(limbs: &[u32], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let Some((&top, rest)) = limbs.split_last() else {
        return f.write_str("0");
    };
    write!(f, "{top}")?;
    for &limb in rest.iter().rev() {
        write!(f, "{limb:09}")?;
    }
    Ok(())
}

impl BigInt {
    pub fn zero() -> Self {
        Self {
            neg: false,
            limbs: Vec::new(),
        }
    }

    fn from_parts(neg: bool, mut limbs: Vec<u32>) -> Option<Self> {
        trim_limbs(&mut limbs);
        if limbs_digits(&limbs) > MAX_DIGITS {
            return None;
        }
        let neg = neg && !limbs.is_empty();
        Some(Self { neg, limbs })
    }

    fn from_parts_unchecked(neg: bool, mut limbs: Vec<u32>) -> Self {
        trim_limbs(&mut limbs);
        let neg = neg && !limbs.is_empty();
        Self { neg, limbs }
    }

    pub fn from_i64(n: i64) -> Self {
        Self::from_i128(i128::from(n))
    }

    pub fn from_u64(n: u64) -> Self {
        Self::from_u128(u128::from(n))
    }

    pub fn from_i128(n: i128) -> Self {
        let neg = n < 0;
        let mut mag = n.unsigned_abs();
        let mut limbs = Vec::new();
        while mag > 0 {
            limbs.push((mag % BASE_U128) as u32);
            mag /= BASE_U128;
        }
        Self { neg, limbs }
    }

    pub fn from_u128(mut n: u128) -> Self {
        let mut limbs = Vec::new();
        while n > 0 {
            limbs.push((n % BASE_U128) as u32);
            n /= BASE_U128;
        }
        Self { neg: false, limbs }
    }

    /// Strict XSD `xsd:integer` lexical parse (`(\+|-)?[0-9]+`).
    pub fn parse(lex: &str) -> Option<Self> {
        let (neg, digits) = match lex.as_bytes().first() {
            Some(b'-') => (true, &lex[1..]),
            Some(b'+') => (false, &lex[1..]),
            Some(_) => (false, lex),
            None => return None,
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let stripped = digits.trim_start_matches('0');
        if stripped.len() > MAX_DIGITS {
            return None;
        }
        let limbs = parse_unsigned_digits(stripped);
        Some(Self::from_parts_unchecked(neg, limbs))
    }

    /// Exact truncation of a finite `f64` towards zero.
    pub fn from_f64_trunc(x: f64) -> Option<Self> {
        if !x.is_finite() {
            return None;
        }
        if x.abs() < 1.0 {
            return Some(Self::zero());
        }
        let bits = x.to_bits();
        let neg = (bits >> 63) != 0;
        let exp_bits = ((bits >> 52) & 0x7ff) as i32;
        let frac = bits & ((1u64 << 52) - 1);
        // |x| >= 1.0 implies normal float (exp_bits >= 1023).
        let mant = (1u64 << 52) | frac;
        let exp = exp_bits - 1023 - 52;
        if exp <= 0 {
            let v = mant >> (-exp);
            let mut out = Self::from_u64(v);
            if neg && !out.is_zero() {
                out.neg = true;
            }
            return Some(out);
        }
        // Multiply `mant` by 2^exp in chunks of 2^29 (fits cleanly in u32 < BASE).
        let mut limbs = Self::from_u64(mant).limbs;
        let mut shift = exp as u32;
        while shift > 0 {
            let step = shift.min(29);
            limbs = mul_mag_u32(&limbs, 1u32 << step);
            shift -= step;
        }
        Self::from_parts(neg, limbs)
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    #[inline]
    pub fn is_negative(&self) -> bool {
        self.neg
    }

    #[inline]
    pub fn is_positive(&self) -> bool {
        !self.neg && !self.limbs.is_empty()
    }

    #[inline]
    pub fn is_even(&self) -> bool {
        self.limbs.first().is_none_or(|&l| l.is_multiple_of(2))
    }

    pub fn decimal_digits(&self) -> usize {
        limbs_digits(&self.limbs)
    }

    pub fn heap_bytes(&self) -> u64 {
        (self.limbs.capacity() * std::mem::size_of::<u32>()) as u64 + 32
    }

    pub fn to_i64(&self) -> Option<i64> {
        let v = self.to_i128()?;
        i64::try_from(v).ok()
    }

    pub fn to_u64(&self) -> Option<u64> {
        if self.neg {
            return None;
        }
        let v = self.to_i128()?;
        u64::try_from(v).ok()
    }

    pub fn to_i128(&self) -> Option<i128> {
        let mut acc: u128 = 0;
        for &limb in self.limbs.iter().rev() {
            acc = acc.checked_mul(BASE_U128)?.checked_add(u128::from(limb))?;
        }
        if self.neg {
            if acc == (i128::MAX as u128) + 1 {
                Some(i128::MIN)
            } else {
                i128::try_from(acc).ok().map(|v| -v)
            }
        } else {
            i128::try_from(acc).ok()
        }
    }

    pub fn negated(&self) -> Self {
        Self {
            neg: !self.is_zero() && !self.neg,
            limbs: self.limbs.clone(),
        }
    }

    pub fn abs(&self) -> Self {
        Self {
            neg: false,
            limbs: self.limbs.clone(),
        }
    }

    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        if self.neg == rhs.neg {
            Self::from_parts(self.neg, add_mag(&self.limbs, &rhs.limbs))
        } else {
            match cmp_mag(&self.limbs, &rhs.limbs) {
                Ordering::Equal => Some(Self::zero()),
                Ordering::Greater => Self::from_parts(self.neg, sub_mag(&self.limbs, &rhs.limbs)),
                Ordering::Less => Self::from_parts(rhs.neg, sub_mag(&rhs.limbs, &self.limbs)),
            }
        }
    }

    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        if self.neg != rhs.neg {
            Self::from_parts(self.neg, add_mag(&self.limbs, &rhs.limbs))
        } else {
            match cmp_mag(&self.limbs, &rhs.limbs) {
                Ordering::Equal => Some(Self::zero()),
                Ordering::Greater => Self::from_parts(self.neg, sub_mag(&self.limbs, &rhs.limbs)),
                Ordering::Less => Self::from_parts(!self.neg, sub_mag(&rhs.limbs, &self.limbs)),
            }
        }
    }

    pub fn checked_mul(&self, rhs: &Self) -> Option<Self> {
        if self.is_zero() || rhs.is_zero() {
            return Some(Self::zero());
        }
        if self.decimal_digits() + rhs.decimal_digits() > MAX_DIGITS + 1 {
            return None;
        }
        let limbs = mul_mag(&self.limbs, &rhs.limbs);
        Self::from_parts(self.neg != rhs.neg, limbs)
    }

    /// Truncating division towards zero: `self = q * rhs + r`, `|r| < |rhs|`, `r` carries
    /// the sign of `self`. Returns `None` on division by zero.
    pub fn checked_div_rem(&self, rhs: &Self) -> Option<(Self, Self)> {
        if rhs.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some((Self::zero(), Self::zero()));
        }
        let (q, r) = div_rem_mag(&self.limbs, &rhs.limbs);
        Some((
            Self::from_parts_unchecked(self.neg != rhs.neg, q),
            Self::from_parts_unchecked(self.neg, r),
        ))
    }

    pub fn to_f64(&self) -> f64 {
        to_f64_exact(self.neg, &self.limbs, 0)
    }

    pub fn to_f32(&self) -> f32 {
        to_f32_exact(self.neg, &self.limbs, 0)
    }
}

impl Ord for BigInt {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.neg, other.neg) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => cmp_mag(&self.limbs, &other.limbs),
            (true, true) => cmp_mag(&other.limbs, &self.limbs),
        }
    }
}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.neg {
            f.write_str("-")?;
        }
        write_limbs(&self.limbs, f)
    }
}

impl BigDec {
    pub fn zero() -> Self {
        Self {
            mantissa: BigInt::zero(),
            scale: 0,
        }
    }

    fn normalize(neg: bool, mut limbs: Vec<u32>, mut scale: u32) -> Option<Self> {
        trim_limbs(&mut limbs);
        if limbs.is_empty() {
            return Some(Self::zero());
        }
        if scale > 0 {
            let tz = trailing_zeros_mag(&limbs).min(scale as usize);
            if tz > 0 {
                let (q, _) = div_rem_mag_pow10(&limbs, tz);
                limbs = q;
                scale -= tz as u32;
            }
        }
        if scale > MAX_SCALE || limbs_digits(&limbs) > MAX_DIGITS {
            return None;
        }
        Some(Self {
            mantissa: BigInt::from_parts_unchecked(neg, limbs),
            scale,
        })
    }

    pub fn from_bigint(i: BigInt) -> Self {
        Self {
            mantissa: i,
            scale: 0,
        }
    }

    pub fn from_i64(n: i64) -> Self {
        Self::from_bigint(BigInt::from_i64(n))
    }

    pub fn from_oxs_decimal(d: Decimal) -> Self {
        let raw = i128::from_be_bytes(d.to_be_bytes());
        let bi = BigInt::from_i128(raw);
        Self::normalize(bi.neg, bi.limbs, OXS_DEC_SCALE)
            .expect("oxsdatatypes::Decimal fits within MAX_DIGITS and MAX_SCALE")
    }

    /// Lossless conversion to `oxsdatatypes::Decimal` when the value fits its fixed
    /// 18-fractional-digit `i128` representation.
    pub fn to_oxs_decimal(&self) -> Option<Decimal> {
        if self.scale > OXS_DEC_SCALE {
            return None;
        }
        let m = self.mantissa.to_i128()?;
        let factor = 10i128.pow(OXS_DEC_SCALE - self.scale);
        let raw = m.checked_mul(factor)?;
        Some(Decimal::from_be_bytes(raw.to_be_bytes()))
    }

    /// Strict XSD `xsd:decimal` lexical parse (`(\+|-)?([0-9]+(\.[0-9]*)?|\.[0-9]+)`).
    pub fn parse(lex: &str) -> Option<Self> {
        let (neg, body) = match lex.as_bytes().first() {
            Some(b'-') => (true, &lex[1..]),
            Some(b'+') => (false, &lex[1..]),
            Some(_) => (false, lex),
            None => return None,
        };
        let (int_raw, frac_raw) = match body.split_once('.') {
            Some((i, f)) => {
                if f.contains('.') {
                    return None;
                }
                (i, f)
            }
            None => (body, ""),
        };
        if int_raw.is_empty() && frac_raw.is_empty() {
            return None;
        }
        if !int_raw.bytes().all(|b| b.is_ascii_digit())
            || !frac_raw.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let int_trim = int_raw.trim_start_matches('0');
        let frac_trim = frac_raw.trim_end_matches('0');
        if int_trim.is_empty() && frac_trim.is_empty() {
            return Some(Self::zero());
        }
        let scale = u32::try_from(frac_trim.len()).ok()?;
        if scale > MAX_SCALE {
            return None;
        }
        // Significant digits: if integer part is empty, strip leading zeros of fraction.
        let sig_len = if int_trim.is_empty() {
            frac_trim.trim_start_matches('0').len()
        } else {
            int_trim.len() + frac_trim.len()
        };
        if sig_len > MAX_DIGITS {
            return None;
        }
        let mut combined = String::with_capacity(sig_len);
        if int_trim.is_empty() {
            combined.push_str(frac_trim.trim_start_matches('0'));
        } else {
            combined.push_str(int_trim);
            combined.push_str(frac_trim);
        }
        let limbs = parse_unsigned_digits(&combined);
        Self::normalize(neg, limbs, scale)
    }

    /// Parse a shortest scientific decimal (`format!("{x:e}")`) from a finite float or
    /// double into an exact `BigDec`.
    fn parse_scientific(s: &str) -> Option<Self> {
        let (neg, body) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let (mant, exp) = match body.split_once(['e', 'E']) {
            Some((m, e)) => (m, e.parse::<i64>().ok()?),
            None => (body, 0),
        };
        let (int_part, frac_part) = mant.split_once('.').unwrap_or((mant, ""));
        let mut digits = String::with_capacity(int_part.len() + frac_part.len());
        digits.push_str(int_part);
        digits.push_str(frac_part);
        let stripped = digits.trim_start_matches('0');
        if stripped.is_empty() {
            return Some(Self::zero());
        }
        let net_exp = exp - frac_part.len() as i64;
        let limbs = parse_unsigned_digits(stripped);
        if net_exp >= 0 {
            let shift = usize::try_from(net_exp).ok()?;
            if stripped.len() + shift > MAX_DIGITS {
                return None;
            }
            let shifted = shift_mag_pow10(&limbs, shift);
            Self::normalize(neg, shifted, 0)
        } else {
            let scale = u32::try_from(-net_exp).ok()?;
            Self::normalize(neg, limbs, scale)
        }
    }

    pub fn from_f64(x: f64) -> Option<Self> {
        if !x.is_finite() {
            return None;
        }
        Self::parse_scientific(&format!("{x:e}"))
    }

    pub fn from_f32(x: f32) -> Option<Self> {
        if !x.is_finite() {
            return None;
        }
        Self::parse_scientific(&format!("{x:e}"))
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.mantissa.is_zero()
    }

    #[inline]
    pub fn is_negative(&self) -> bool {
        self.mantissa.is_negative()
    }

    pub fn heap_bytes(&self) -> u64 {
        self.mantissa.heap_bytes() + 8
    }

    pub fn negated(&self) -> Self {
        Self {
            mantissa: self.mantissa.negated(),
            scale: self.scale,
        }
    }

    pub fn abs(&self) -> Self {
        Self {
            mantissa: self.mantissa.abs(),
            scale: self.scale,
        }
    }

    /// Compare this `BigDec` with a `BigInt` without allocating.
    pub fn cmp_bigint(&self, other: &BigInt) -> Ordering {
        match (self.mantissa.neg, other.neg) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => match (self.is_zero(), other.is_zero()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => cmp_mag_scaled(&self.mantissa.limbs, self.scale, &other.limbs, 0),
            },
            (true, true) => cmp_mag_scaled(&other.limbs, 0, &self.mantissa.limbs, self.scale),
        }
    }

    fn align_pair(&self, rhs: &Self) -> Option<(Vec<u32>, Vec<u32>, u32)> {
        let scale = self.scale.max(rhs.scale);
        let shift_a = (scale - self.scale) as usize;
        let shift_b = (scale - rhs.scale) as usize;
        // Allow up to MAX_DIGITS + 1 during intermediate alignment so cancelling additions
        // at the boundary stay exact, while still bounding allocation strictly.
        let lim = MAX_DIGITS + 2;
        if (!self.is_zero() && self.mantissa.decimal_digits() + shift_a > lim)
            || (!rhs.is_zero() && rhs.mantissa.decimal_digits() + shift_b > lim)
        {
            return None;
        }
        let a = shift_mag_pow10(&self.mantissa.limbs, shift_a);
        let b = shift_mag_pow10(&rhs.mantissa.limbs, shift_b);
        Some((a, b, scale))
    }

    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        if self.is_zero() {
            return Some(rhs.clone());
        }
        if rhs.is_zero() {
            return Some(self.clone());
        }
        let (a, b, scale) = self.align_pair(rhs)?;
        if self.mantissa.neg == rhs.mantissa.neg {
            Self::normalize(self.mantissa.neg, add_mag(&a, &b), scale)
        } else {
            match cmp_mag(&a, &b) {
                Ordering::Equal => Some(Self::zero()),
                Ordering::Greater => Self::normalize(self.mantissa.neg, sub_mag(&a, &b), scale),
                Ordering::Less => Self::normalize(rhs.mantissa.neg, sub_mag(&b, &a), scale),
            }
        }
    }

    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        if rhs.is_zero() {
            return Some(self.clone());
        }
        if self.is_zero() {
            return Some(rhs.negated());
        }
        let (a, b, scale) = self.align_pair(rhs)?;
        if self.mantissa.neg != rhs.mantissa.neg {
            Self::normalize(self.mantissa.neg, add_mag(&a, &b), scale)
        } else {
            match cmp_mag(&a, &b) {
                Ordering::Equal => Some(Self::zero()),
                Ordering::Greater => Self::normalize(self.mantissa.neg, sub_mag(&a, &b), scale),
                Ordering::Less => Self::normalize(!self.mantissa.neg, sub_mag(&b, &a), scale),
            }
        }
    }

    pub fn checked_mul(&self, rhs: &Self) -> Option<Self> {
        if self.is_zero() || rhs.is_zero() {
            return Some(Self::zero());
        }
        let scale = self.scale.checked_add(rhs.scale)?;
        if self.mantissa.decimal_digits() + rhs.mantissa.decimal_digits() > MAX_DIGITS * 2 {
            return None;
        }
        let limbs = mul_mag(&self.mantissa.limbs, &rhs.mantissa.limbs);
        Self::normalize(self.mantissa.neg != rhs.mantissa.neg, limbs, scale)
    }

    /// Exact decimal division truncated towards zero, retaining at least 18 fractional
    /// digits (or enough digits for 18 significant digits on tiny quotients), bounded by
    /// [`MAX_DIGITS`] and [`MAX_SCALE`].
    pub fn checked_div(&self, rhs: &Self) -> Option<Self> {
        if rhs.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some(Self::zero());
        }
        let mag_a = self.mantissa.decimal_digits() as i64 - i64::from(self.scale);
        let mag_b = rhs.mantissa.decimal_digits() as i64 - i64::from(rhs.scale);
        let q_mag = mag_a - mag_b;
        let min_for_sig = if q_mag < 0 {
            let need = (-q_mag).saturating_add(i64::from(MIN_DIV_SCALE));
            if need > i64::from(MAX_SCALE) {
                return None;
            }
            need as u32
        } else {
            MIN_DIV_SCALE
        };
        let out_scale = self.scale.max(rhs.scale).max(min_for_sig).min(MAX_SCALE);
        let shift = i64::from(out_scale) + i64::from(rhs.scale) - i64::from(self.scale);
        let max_work = MAX_DIGITS * 2;
        let (q, _) = if shift >= 0 {
            let shift = usize::try_from(shift).ok()?;
            if self.mantissa.decimal_digits() + shift > max_work {
                return None;
            }
            let num = shift_mag_pow10(&self.mantissa.limbs, shift);
            div_rem_mag(&num, &rhs.mantissa.limbs)
        } else {
            let shift = usize::try_from(-shift).ok()?;
            if rhs.mantissa.decimal_digits() + shift > max_work {
                return None;
            }
            let den = shift_mag_pow10(&rhs.mantissa.limbs, shift);
            div_rem_mag(&self.mantissa.limbs, &den)
        };
        Self::normalize(self.mantissa.neg != rhs.mantissa.neg, q, out_scale)
    }

    /// `fn:numeric-mod` on decimals: remainder of truncating division.
    pub fn checked_rem(&self, rhs: &Self) -> Option<Self> {
        if rhs.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some(Self::zero());
        }
        let (a, b, scale) = self.align_pair(rhs)?;
        let (_, r) = div_rem_mag(&a, &b);
        Self::normalize(self.mantissa.neg, r, scale)
    }

    /// `fn:numeric-integer-divide` on decimals: integer quotient truncated towards zero.
    pub fn checked_idiv(&self, rhs: &Self) -> Option<BigInt> {
        if rhs.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some(BigInt::zero());
        }
        let (a, b, _) = self.align_pair(rhs)?;
        let (q, _) = div_rem_mag(&a, &b);
        BigInt::from_parts(self.mantissa.neg != rhs.mantissa.neg, q)
    }

    /// Truncate towards zero to a [`BigInt`].
    pub fn trunc_to_bigint(&self) -> BigInt {
        if self.scale == 0 || self.is_zero() {
            return self.mantissa.clone();
        }
        let (q, _) = div_rem_mag_pow10(&self.mantissa.limbs, self.scale as usize);
        BigInt::from_parts_unchecked(self.mantissa.neg, q)
    }

    pub fn ceil(&self) -> Option<Self> {
        if self.scale == 0 || self.is_zero() {
            return Some(self.clone());
        }
        let (q, r) = div_rem_mag_pow10(&self.mantissa.limbs, self.scale as usize);
        let q = BigInt::from_parts_unchecked(self.mantissa.neg, q);
        if !self.mantissa.neg && !r.is_empty() {
            Some(Self::from_bigint(q.checked_add(&BigInt::from_i64(1))?))
        } else {
            Some(Self::from_bigint(q))
        }
    }

    pub fn floor(&self) -> Option<Self> {
        if self.scale == 0 || self.is_zero() {
            return Some(self.clone());
        }
        let (q, r) = div_rem_mag_pow10(&self.mantissa.limbs, self.scale as usize);
        let q = BigInt::from_parts_unchecked(self.mantissa.neg, q);
        if self.mantissa.neg && !r.is_empty() {
            Some(Self::from_bigint(q.checked_sub(&BigInt::from_i64(1))?))
        } else {
            Some(Self::from_bigint(q))
        }
    }

    /// SPARQL / XPath `ROUND` (half towards positive infinity).
    pub fn round(&self) -> Option<Self> {
        if self.scale == 0 || self.is_zero() {
            return Some(self.clone());
        }
        let scale = self.scale as usize;
        let (q, r) = div_rem_mag_pow10(&self.mantissa.limbs, scale);
        let q_int = BigInt::from_parts_unchecked(self.mantissa.neg, q);
        if r.is_empty() {
            return Some(Self::from_bigint(q_int));
        }
        // Compare r (0 < r < 10^scale) against 5 * 10^(scale - 1).
        let r_digits = limbs_digits(&r);
        let cmp_half = if r_digits < scale {
            Ordering::Less
        } else {
            debug_assert_eq!(r_digits, scale);
            let top = digit_from_msb(&r, r_digits, 0);
            match top.cmp(&5) {
                Ordering::Less => Ordering::Less,
                Ordering::Greater => Ordering::Greater,
                Ordering::Equal => {
                    // Check if any lower digit of `r` is nonzero.
                    let (_, low) = div_rem_mag_pow10(&r, scale - 1);
                    if low.is_empty() {
                        Ordering::Equal
                    } else {
                        Ordering::Greater
                    }
                }
            }
        };
        let rounded = if !self.mantissa.neg {
            if matches!(cmp_half, Ordering::Greater | Ordering::Equal) {
                q_int.checked_add(&BigInt::from_i64(1))?
            } else {
                q_int
            }
        } else if cmp_half == Ordering::Greater {
            q_int.checked_sub(&BigInt::from_i64(1))?
        } else {
            q_int
        };
        Some(Self::from_bigint(rounded))
    }

    pub fn to_f64(&self) -> f64 {
        to_f64_exact(self.mantissa.neg, &self.mantissa.limbs, self.scale)
    }

    pub fn to_f32(&self) -> f32 {
        to_f32_exact(self.mantissa.neg, &self.mantissa.limbs, self.scale)
    }
}

impl Ord for BigDec {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.mantissa.neg, other.mantissa.neg) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => match (self.is_zero(), other.is_zero()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => cmp_mag_scaled(
                    &self.mantissa.limbs,
                    self.scale,
                    &other.mantissa.limbs,
                    other.scale,
                ),
            },
            (true, true) => cmp_mag_scaled(
                &other.mantissa.limbs,
                other.scale,
                &self.mantissa.limbs,
                self.scale,
            ),
        }
    }
}

impl PartialOrd for BigDec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for BigDec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            return f.write_str("0");
        }
        if self.mantissa.neg {
            f.write_str("-")?;
        }
        if self.scale == 0 {
            return write_limbs(&self.mantissa.limbs, f);
        }
        let scale = self.scale as usize;
        let digits = self.mantissa.decimal_digits();
        if digits <= scale {
            f.write_str("0.")?;
            for _ in 0..(scale - digits) {
                f.write_str("0")?;
            }
            write_limbs(&self.mantissa.limbs, f)
        } else {
            let int_len = digits - scale;
            for i in 0..int_len {
                write!(f, "{}", digit_from_msb(&self.mantissa.limbs, digits, i))?;
            }
            f.write_str(".")?;
            for i in int_len..digits {
                write!(f, "{}", digit_from_msb(&self.mantissa.limbs, digits, i))?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigint_parse_format_and_ord() {
        let a = BigInt::parse("99999999999999999999999").unwrap();
        assert_eq!(a.to_string(), "99999999999999999999999");
        assert_eq!(a.decimal_digits(), 23);
        let b = BigInt::parse("+00099999999999999999999999").unwrap();
        assert_eq!(a, b);
        let c = BigInt::parse("-99999999999999999999999").unwrap();
        assert!(c < BigInt::zero());
        assert!(BigInt::zero() < a);
        assert_eq!(BigInt::parse("-0").unwrap(), BigInt::zero());
        assert!(BigInt::parse("").is_none());
        assert!(BigInt::parse("1.0").is_none());
        assert!(BigInt::parse(" 1").is_none());
        // DoS digit cap
        let too_long = "9".repeat(MAX_DIGITS + 1);
        assert!(BigInt::parse(&too_long).is_none());
        let exact_cap = "9".repeat(MAX_DIGITS);
        assert!(BigInt::parse(&exact_cap).is_some());
    }

    #[test]
    fn bigint_arithmetic_and_knuth_div_matches_i128() {
        use rand::{RngExt, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let edges: &[i128] = &[
            0,
            1,
            -1,
            999_999_999,
            1_000_000_000,
            -1_000_000_000,
            i64::MAX as i128,
            i64::MIN as i128,
            i128::MAX,
            i128::MIN,
        ];
        let mut samples: Vec<i128> = edges.to_vec();
        for _ in 0..200 {
            let shift = rng.random_range(0..100);
            samples.push(rng.random::<i128>() >> shift);
        }
        for &x in &samples {
            let bx = BigInt::from_i128(x);
            assert_eq!(bx.to_string(), x.to_string());
            assert_eq!(bx.to_i128(), Some(x));
            for &y in &samples[..30] {
                let by = BigInt::from_i128(y);
                assert_eq!(bx.cmp(&by), x.cmp(&y));
                if let Some(s) = x.checked_add(y) {
                    assert_eq!(bx.checked_add(&by).unwrap().to_i128(), Some(s));
                }
                if let Some(d) = x.checked_sub(y) {
                    assert_eq!(bx.checked_sub(&by).unwrap().to_i128(), Some(d));
                }
                if let Some(p) = x.checked_mul(y) {
                    assert_eq!(bx.checked_mul(&by).unwrap().to_i128(), Some(p));
                }
                if y != 0
                    && let (Some(q), Some(r)) = (x.checked_div(y), x.checked_rem(y))
                {
                    let (bq, br) = bx.checked_div_rem(&by).unwrap();
                    assert_eq!(bq.to_i128(), Some(q), "{x} / {y}");
                    assert_eq!(br.to_i128(), Some(r), "{x} % {y}");
                }
            }
        }
        // Multi-limb Knuth D test beyond i128
        let big_u = BigInt::parse("99999999999999999999999999999999999999999999999999").unwrap();
        let big_v = BigInt::parse("123456789012345678901234567890").unwrap();
        let (q, r) = big_u.checked_div_rem(&big_v).unwrap();
        assert!(r < big_v && !r.is_negative());
        assert_eq!(
            q.checked_mul(&big_v).unwrap().checked_add(&r).unwrap(),
            big_u
        );
    }

    #[test]
    fn bigdec_operations_and_rounding() {
        let a = BigDec::parse("99999999999999999999999999999.25").unwrap();
        let b = BigDec::parse("0.75").unwrap();
        assert_eq!(
            a.checked_add(&b).unwrap().to_string(),
            "100000000000000000000000000000"
        );
        let tiny = BigDec::parse("0.0000000000000000001").unwrap();
        assert_eq!(tiny.to_string(), "0.0000000000000000001");
        assert!(tiny.to_oxs_decimal().is_none());
        let ten = BigDec::from_i64(10);
        assert_eq!(
            tiny.checked_mul(&ten).unwrap().to_oxs_decimal(),
            Some("0.000000000000000001".parse().unwrap())
        );
        assert_eq!(
            BigDec::parse("2.5").unwrap().round().unwrap().to_string(),
            "3"
        );
        assert_eq!(
            BigDec::parse("-2.5").unwrap().round().unwrap().to_string(),
            "-2"
        );
        assert_eq!(
            BigDec::parse("-2.50000000000000000001")
                .unwrap()
                .round()
                .unwrap()
                .to_string(),
            "-3"
        );
        assert_eq!(
            BigDec::parse("99999999999999999999999.1")
                .unwrap()
                .ceil()
                .unwrap()
                .to_string(),
            "100000000000000000000000"
        );
        assert_eq!(
            BigDec::parse("-99999999999999999999999.1")
                .unwrap()
                .floor()
                .unwrap()
                .to_string(),
            "-100000000000000000000000"
        );
        // Division of huge and tiny values
        let one = BigDec::from_i64(1);
        let pow25 = BigDec::parse("10000000000000000000000000").unwrap();
        assert_eq!(
            one.checked_div(&pow25).unwrap().to_string(),
            "0.0000000000000000000000001"
        );
    }

    #[test]
    fn float_and_double_conversions() {
        let huge = BigInt::parse(&format!("1{}", "0".repeat(400))).unwrap();
        assert!(huge.to_f64().is_infinite() && huge.to_f64().is_sign_positive());
        assert!(
            huge.negated().to_f64().is_infinite() && huge.negated().to_f64().is_sign_negative()
        );
        assert!(huge.to_f32().is_infinite());

        let exact = BigInt::from_f64_trunc(1e25).unwrap();
        assert_eq!(exact.to_string(), "10000000000000000905969664");
        assert_eq!(
            BigDec::from_f64(1e25).unwrap().to_string(),
            "10000000000000000000000000"
        );
        assert_eq!(
            BigDec::from_f64(1e-25).unwrap().to_string(),
            "0.0000000000000000000000001"
        );
    }
}
