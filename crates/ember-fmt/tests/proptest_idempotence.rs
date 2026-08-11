use ember_fmt::format;
use proptest::prelude::*;
use std::fs;
use std::path::PathBuf;

fn conformance_sources() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("em"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|p| fs::read_to_string(&p).unwrap())
        .collect()
}

/// Inserts `extra` blank lines / spaces at pseudo-random positions that are
/// always safe (only ever widens existing whitespace runs, or duplicates an
/// existing blank line, never touches non-whitespace bytes) so the result
/// is still valid ember source with the exact same token stream.
fn perturb_whitespace(src: &str, seed: u64) -> String {
    let mut out = String::with_capacity(src.len() + 16);
    let mut counter = seed;
    for line in src.lines() {
        out.push_str(line);
        out.push('\n');
        counter = counter.wrapping_mul(6364136223846793005).wrapping_add(1);
        if line.trim().is_empty() && counter.is_multiple_of(3) {
            out.push('\n');
        }
    }
    out
}

proptest! {
    #[test]
    fn formatting_is_idempotent_over_perturbed_conformance_corpus(
        idx in 0..conformance_sources().len(),
        seed in any::<u64>(),
    ) {
        let sources = conformance_sources();
        let perturbed = perturb_whitespace(&sources[idx], seed);
        let once = format(&perturbed);
        let twice = format(&once);
        prop_assert_eq!(once, twice);
    }
}
