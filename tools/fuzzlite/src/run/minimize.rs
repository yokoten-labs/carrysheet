//! Delta reduction: remove chunks of the input, halving the chunk size,
//! while the failure still happens.

use std::time::{Duration, Instant};

/// How much work minimising one finding may take.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub runs: usize,
    pub time: Duration,
}

/// The smallest input found that still `fails`, within the budget.
pub fn minimize(input: &[u8], budget: Budget, mut fails: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + budget.time;
    let mut runs = 0;
    let mut check = |candidate: &[u8]| -> Option<bool> {
        if runs >= budget.runs || Instant::now() >= deadline {
            return None;
        }
        runs += 1;
        Some(fails(candidate))
    };
    let mut cur = input.to_vec();
    // Cut the tail first: cheap, and often most of the file.
    while cur.len() > 1 {
        let half = &cur[..cur.len() / 2];
        match check(half) {
            Some(true) => cur.truncate(cur.len() / 2),
            Some(false) => break,
            None => return cur,
        }
    }
    let mut chunk = cur.len().div_ceil(2);
    while chunk >= 1 {
        let mut at = 0;
        while at < cur.len() {
            let end = (at + chunk).min(cur.len());
            let mut candidate = cur[..at].to_vec();
            candidate.extend_from_slice(&cur[end..]);
            match check(&candidate) {
                Some(true) => cur = candidate,
                Some(false) => at += chunk,
                None => return cur,
            }
        }
        if chunk == 1 {
            break;
        }
        chunk = chunk.div_ceil(2);
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLENTY: Budget = Budget {
        runs: 100_000,
        time: Duration::from_secs(60),
    };

    #[test]
    fn shrinks_to_the_trigger() {
        let input = b"hello, this ! is a long input with one trigger";
        assert_eq!(minimize(input, PLENTY, |d| d.contains(&b'!')), b"!");
    }

    #[test]
    fn keeps_bytes_that_matter_together() {
        let input = b"xxAxxxxBxxxxxxxxxxCxx";
        let out = minimize(input, PLENTY, |d| {
            let a = d.iter().position(|&b| b == b'A');
            let c = d.iter().position(|&b| b == b'C');
            matches!((a, c), (Some(a), Some(c)) if a < c)
        });
        assert_eq!(out, b"AC");
    }

    #[test]
    fn stops_at_the_budget() {
        let input = vec![b'!'; 4096];
        let mut calls = 0;
        let budget = Budget {
            runs: 5,
            time: Duration::from_secs(60),
        };
        let out = minimize(&input, budget, |_| {
            calls += 1;
            false
        });
        assert_eq!(calls, 5);
        assert_eq!(out, input);
    }
}
