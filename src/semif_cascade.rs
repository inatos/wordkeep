//! Conformal singleton check for draft→verify cascade (no generated tokens).

/// Indices whose nonconformity `1 - p_i` is at most `qhat`.
pub fn conformal_option_set(probs: &[f64], qhat: f64) -> Vec<usize> {
    probs
        .iter()
        .enumerate()
        .filter(|(_, p)| (1.0 - *p) <= qhat)
        .map(|(i, _)| i)
        .collect()
}

/// Commit draft when the conformal set is a singleton.
pub fn should_commit_draft(probs: &[f64], qhat: f64) -> (bool, usize) {
    let set = conformal_option_set(probs, qhat);
    (set.len() == 1, set.len())
}

/// Top-1 minus top-2 probability (0 when fewer than two options).
pub fn top_margin(probs: &[f64]) -> f64 {
    if probs.is_empty() {
        return 0.0;
    }
    let mut sorted = probs.to_vec();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    if sorted.len() == 1 {
        return sorted[0];
    }
    sorted[0] - sorted[1]
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
