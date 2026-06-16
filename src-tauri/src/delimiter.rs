//! Delimiter auto-detection.
//!
//! The `csv` crate has no built-in sniffer, so we use a small heuristic: count
//! how often each candidate delimiter appears *outside* quoted fields on the
//! first several lines, and pick the candidate whose per-line count is both
//! non-zero and the most consistent across lines.

/// Delimiters we try to detect, in priority order for tie-breaks.
pub const CANDIDATES: [u8; 4] = [b',', b'\t', b';', b'|'];

const MAX_SAMPLE_LINES: usize = 50;

/// Guess the delimiter of a decoded text sample. Falls back to a comma when the
/// sample is empty or inconclusive.
pub fn detect(sample: &str) -> u8 {
    let lines: Vec<&str> = sample
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(MAX_SAMPLE_LINES)
        .collect();

    if lines.is_empty() {
        return b',';
    }

    let mut best = b',';
    let mut best_score = f64::MIN;

    for &candidate in &CANDIDATES {
        let counts: Vec<usize> = lines
            .iter()
            .map(|line| count_outside_quotes(line, candidate))
            .collect();

        let mode = mode_of(&counts);
        if mode == 0 {
            continue; // delimiter never appears as a real separator
        }

        let consistent = counts.iter().filter(|&&c| c == mode).count();
        // Reward consistency first, then the number of columns it implies.
        let score = consistent as f64 + (mode as f64) * 0.01;

        if score > best_score {
            best_score = score;
            best = candidate;
        }
    }

    best
}

/// Count occurrences of `delim` in `line` that are not inside a double-quoted
/// field (RFC 4180 style, where `""` is an escaped quote within a field).
fn count_outside_quotes(line: &str, delim: u8) -> usize {
    let mut in_quotes = false;
    let mut count = 0;
    for &byte in line.as_bytes() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b if b == delim && !in_quotes => count += 1,
            _ => {}
        }
    }
    count
}

/// The most frequent value in `counts` (the statistical mode). Ties resolve to
/// the larger value, which favours rows that actually contain the delimiter.
fn mode_of(counts: &[usize]) -> usize {
    use std::collections::HashMap;
    let mut freq: HashMap<usize, usize> = HashMap::new();
    for &c in counts {
        *freq.entry(c).or_insert(0) += 1;
    }
    freq.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)))
        .map(|(value, _)| value)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_comma() {
        let sample = "a,b,c\n1,2,3\n4,5,6";
        assert_eq!(detect(sample), b',');
    }

    #[test]
    fn detects_tab() {
        let sample = "a\tb\tc\n1\t2\t3";
        assert_eq!(detect(sample), b'\t');
    }

    #[test]
    fn detects_semicolon() {
        let sample = "a;b;c\n1;2;3\n4;5;6";
        assert_eq!(detect(sample), b';');
    }

    #[test]
    fn detects_pipe() {
        let sample = "a|b|c\n1|2|3";
        assert_eq!(detect(sample), b'|');
    }

    #[test]
    fn ignores_delimiters_inside_quotes() {
        // Commas inside quotes should not beat the real semicolon delimiter.
        let sample = "\"a,b,c,d\";x\n\"e,f,g,h\";y";
        assert_eq!(detect(sample), b';');
    }

    #[test]
    fn empty_sample_defaults_to_comma() {
        assert_eq!(detect(""), b',');
        assert_eq!(detect("\n\n   \n"), b',');
    }
}
