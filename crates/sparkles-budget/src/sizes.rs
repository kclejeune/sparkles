//! The allocation sizes of the standard collections, for code that reserves the bytes of
//! a growth before it happens.

/// The buckets hashbrown allocates for a table that can hold `cap` items without
/// growing (its `capacity()`).
pub fn buckets(cap: usize) -> usize {
    if cap == 0 {
        0
    } else if cap < 8 {
        // tables of up to 8 buckets hold one item less than their buckets
        (cap + 1).next_power_of_two()
    } else {
        // larger tables hold 7/8 of their buckets
        (cap.saturating_mul(8) / 7).next_power_of_two()
    }
}

/// The heap of a hashbrown table (`HashMap`, `HashSet` or `HashTable`) of capacity
/// `cap` with elements (keys with values) of `elem` bytes: one element and one control
/// byte per bucket, and a group of control bytes more.
pub fn table_bytes(cap: usize, elem: usize) -> usize {
    match buckets(cap) {
        0 => 0,
        b => b.saturating_mul(elem.saturating_add(1)).saturating_add(16),
    }
}

/// The capacity a table of capacity `cap` and `len` items has after room for `add`
/// more is reserved: unchanged when they fit, otherwise that of the smallest table
/// holding `len + add` items and more than `cap` (hashbrown's growth rule).
pub fn table_grown(cap: usize, len: usize, add: usize) -> usize {
    let want = len.saturating_add(add);
    if want <= cap {
        return cap;
    }
    table_capacity(want.max(cap.saturating_add(1)))
}

/// The capacity of a table made for `items` items (hashbrown's `with_capacity`).
pub fn table_capacity(items: usize) -> usize {
    let b = match items {
        0 => return 0,
        1..4 => 4,
        4..8 => 8,
        n => (n.saturating_mul(8) / 7).next_power_of_two(),
    };
    if b < 8 { b - 1 } else { b / 8 * 7 }
}

/// The capacity a vector of capacity `cap` with `len` elements of `elem` bytes grows to
/// when `add` more do not fit, or `cap` when they fit (the standard library's doubling
/// rule).
pub fn vec_grown(cap: usize, len: usize, add: usize, elem: usize) -> usize {
    let want = len.saturating_add(add);
    if want <= cap {
        return cap;
    }
    let min = if elem == 1 {
        8
    } else if elem <= 1024 {
        4
    } else {
        1
    };
    want.max(cap.saturating_mul(2)).max(min)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The estimate of a table's heap is what hashbrown allocates: checked against
    /// the capacities a `HashSet` reports as it grows.
    #[test]
    fn table_sizes_follow_hashbrown() {
        let mut s: std::collections::HashSet<u64> = Default::default();
        for i in 0..100_000u64 {
            let (cap, len) = (s.capacity(), s.len());
            s.insert(i);
            if s.capacity() != cap {
                assert_eq!(s.capacity(), table_grown(cap, len, 1), "at {i}");
            }
            assert!(buckets(s.capacity()) >= s.capacity());
        }
        assert_eq!(table_bytes(0, 8), 0);
        assert_eq!(table_bytes(3, 8), 4 * 9 + 16);
        assert_eq!(table_bytes(7, 8), 8 * 9 + 16);
        assert_eq!(table_bytes(14, 8), 16 * 9 + 16);
    }

    /// A vector grown by one push reaches the capacity `vec_grown` gives.
    #[test]
    fn vector_sizes_follow_the_standard_library() {
        let mut v: Vec<[u64; 3]> = Vec::new();
        for i in 0..10_000u64 {
            let (cap, len) = (v.capacity(), v.len());
            v.push([i; 3]);
            assert_eq!(v.capacity(), vec_grown(cap, len, 1, 24), "at {i}");
        }
    }
}
