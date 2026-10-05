//! Conformal singleton check for draft→verify cascade (no generated tokens).

/// Indices whose nonconformity `1 - p_i` is at most `qhat`.
#[allow(dead_code)] // public helper; commit path uses should_commit_draft
pub fn conformal_option_set(probs: &[f64], qhat: f64) -> Vec<usize> {
    probs
        .iter()
        .enumerate()
        .filter(|(_, p)| (1.0 - *p) <= qhat)
        .map(|(i, _)| i)
        .collect()
}

/// Top-1 minus top-2 probability (0 when fewer than two options).
pub fn top_margin(probs: &[f64]) -> f64 {
    if probs.is_empty() {
        return 0.0;
    }
    let mut best = f64::NEG_INFINITY;
    let mut second = f64::NEG_INFINITY;
    for &p in probs {
        if p > best {
            second = best;
            best = p;
        } else if p > second {
            second = p;
        }
    }
    if second == f64::NEG_INFINITY {
        return best;
    }
    best - second
}

/// Commit draft when the conformal set is a singleton — allocation-free count.
pub fn should_commit_draft(probs: &[f64], qhat: f64) -> (bool, usize) {
    let mut n = 0usize;
    for &p in probs {
        if (1.0 - p) <= qhat {
            n += 1;
            if n > 1 {
                // Still count remaining for cascade_set_size.
            }
        }
    }
    // Recount fully for accurate set size (small n).
    let set_n = probs.iter().filter(|p| (1.0 - *p) <= qhat).count();
    (set_n == 1, set_n)
}

/// Sharp enough to skip adaptive permute cycles: conformal singleton **or** margin.
pub fn adaptive_sharp_enough(probs: &[f64], qhat: f64, margin_min: f64) -> bool {
    let (singleton, _) = should_commit_draft(probs, qhat);
    singleton || top_margin(probs) >= margin_min
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaked_distribution_is_singleton() {
        let p = [0.96, 0.02, 0.02];
        let (commit, n) = should_commit_draft(&p, 0.1);
        assert!(commit);
        assert_eq!(n, 1);
    }

    #[test]
    fn flat_distribution_escalates() {
        let p = [0.4, 0.35, 0.25];
        let (commit, n) = should_commit_draft(&p, 0.1);
        assert!(!commit);
        assert_ne!(n, 1);
    }
}
