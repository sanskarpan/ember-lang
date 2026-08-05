/// Standard Levenshtein edit distance via the two-row dynamic-programming
/// algorithm — O(len(a) * len(b)) time, O(min(len(a), len(b))) space.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Finds the closest candidate to `target` among `candidates` by
/// Levenshtein distance, only if it's close enough to plausibly be a typo
/// rather than an unrelated name — and never suggests `target` itself.
pub fn closest_match<'a>(
    target: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let threshold = (target.chars().count() / 3).max(2);
    candidates
        .map(|c| (c, levenshtein(target, c)))
        .filter(|(_, d)| *d <= threshold && *d > 0)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_have_zero_distance() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn one_substitution_has_distance_one() {
        assert_eq!(levenshtein("cat", "bat"), 1);
    }

    #[test]
    fn completely_different_strings_have_a_larger_distance() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn closest_match_finds_a_near_typo() {
        let candidates = ["count", "counter", "total"];
        assert_eq!(closest_match("cout", candidates.into_iter()), Some("count"));
    }

    #[test]
    fn closest_match_returns_none_when_nothing_is_close() {
        let candidates = ["apple", "banana"];
        assert_eq!(closest_match("xyz", candidates.into_iter()), None);
    }

    #[test]
    fn closest_match_excludes_exact_matches() {
        let candidates = ["count"];
        assert_eq!(closest_match("count", candidates.into_iter()), None);
    }
}
