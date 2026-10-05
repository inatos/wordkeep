//! Cyclic permute / PriDe averaging over typed options.
//!
//! Letter-to-content remap uses `orig = (i + k) % n` after rotating option order
//! (Zheng-style). `(i - k)` is the verified wrong mapping.

use crate::semif::{softmax_labeled, DecisionRequest, OptionSpec, Scorer};
use crate::semif_cascade;

/// Average softmax over `cycles` cyclic rotations of option order.
pub fn permute_average(
    scorer: &dyn Scorer,
    req: &DecisionRequest,
    cycles: usize,
) -> Result<Vec<(String, f64)>, String> {
    let ident = scorer.score_one(req)?;
    permute_from_identity(scorer, req, &ident, cycles)
}

/// Extra permute cycles after an identity `score_detailed` (k=0 uses `identity_raw`).
pub fn permute_from_identity(
    scorer: &dyn Scorer,
    req: &DecisionRequest,
    identity_raw: &[(String, f64)],
    cycles: usize,
) -> Result<Vec<(String, f64)>, String> {
    let n = req.options.len();
    if n == 0 {
        return Err("options must be nonempty".into());
    }
    let cycles = cycles.max(1).min(n);
    let mut acc = vec![0.0; n];
    for k in 0..cycles {
        let probs = if k == 0 {
            softmax_labeled(identity_raw)
        } else {
            let mut rotated = req.clone();
            rotated.options = (0..n)
                .map(|i| req.options[(i + k) % n].clone())
                .collect::<Vec<OptionSpec>>();
            softmax_labeled(&scorer.score_one(&rotated)?)
        };
        for (i, (_id, p)) in probs.iter().enumerate() {
            let orig = (i + k) % n;
            acc[orig] += *p;
        }
    }
    let scale = cycles as f64;
    Ok(req
        .options
        .iter()
        .zip(acc.iter())
        .map(|(o, s)| (o.id.clone(), *s / scale))
        .collect())
}

/// Adaptive permute: identity only when sharp; else full cyclic average.
/// Returns `(averaged_probs, ran_extra_cycles)`.
pub fn adaptive_from_identity(
    scorer: &dyn Scorer,
    req: &DecisionRequest,
    identity_raw: &[(String, f64)],
    cycles: usize,
    qhat: f64,
    margin_min: f64,
) -> Result<(Vec<(String, f64)>, bool), String> {
    let probs = softmax_labeled(identity_raw);
    let p_only: Vec<f64> = probs.iter().map(|(_, p)| *p).collect();
    if semif_cascade::adaptive_sharp_enough(&p_only, qhat, margin_min) {
        return Ok((probs, false));
    }
    Ok((
        permute_from_identity(scorer, req, identity_raw, cycles)?,
        true,
    ))
}

/// Identity plus reversed option order (opt-in; weaker than permute).
pub fn pride_average(
    scorer: &dyn Scorer,
    req: &DecisionRequest,
) -> Result<Vec<(String, f64)>, String> {
    let ident = scorer.score_one(req)?;
    pride_from_identity(scorer, req, &ident)
}

pub fn pride_from_identity(
    scorer: &dyn Scorer,
    req: &DecisionRequest,
    identity_raw: &[(String, f64)],
) -> Result<Vec<(String, f64)>, String> {
    let n = req.options.len();
    if n == 0 {
        return Err("options must be nonempty".into());
    }
    let a = softmax_labeled(identity_raw);
    let mut rev = req.clone();
    rev.options = req.options.iter().rev().cloned().collect();
    let b_raw = scorer.score_one(&rev)?;
    let b = softmax_labeled(&b_raw);
    let mut acc = vec![0.0; n];
    for (i, (_, p)) in a.iter().enumerate() {
        acc[i] += *p;
    }
    for (i, (_, p)) in b.iter().enumerate() {
        let orig = n - 1 - i;
        acc[orig] += *p;
    }
    Ok(req
        .options
        .iter()
        .zip(acc.iter())
        .map(|(o, s)| (o.id.clone(), *s / 2.0))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semif::{DecisionRequest, HeuristicScorer, OptionSpec};

    #[test]
    fn permute_maps_content_ids_not_rotated_letters() {
        let req = DecisionRequest {
            id: "p".into(),
            state: "alpha evidence matches yes strongly".into(),
            question: "Pick yes?".into(),
            options: vec![
                OptionSpec {
                    id: "yes".into(),
                    description: "alpha evidence yes strongly".into(),
                },
                OptionSpec {
                    id: "no".into(),
                    description: "unrelated zebra".into(),
                },
            ],
        };
        let s = HeuristicScorer;
        let once = softmax_labeled(&s.score_one(&req).unwrap());
        let avg = permute_average(&s, &req, 2).unwrap();
        assert_eq!(avg.len(), 2);
        assert_eq!(avg[0].0, "yes");
        assert_eq!(once[0].0, "yes");
        let sum: f64 = avg.iter().map(|(_, p)| *p).sum();
        assert!((sum - 1.0).abs() < 1e-6, "sum={sum}");
    }

    #[test]
    fn adaptive_runs_when_margin_impossible() {
        let req = DecisionRequest {
            id: "p".into(),
            state: "x".into(),
            question: "Pick?".into(),
            options: vec![
                OptionSpec {
                    id: "a".into(),
                    description: "option a".into(),
                },
                OptionSpec {
                    id: "b".into(),
                    description: "option b".into(),
                },
                OptionSpec {
                    id: "c".into(),
                    description: "option c".into(),
                },
            ],
        };
        let s = HeuristicScorer;
        let ident = s.score_one(&req).unwrap();
        let (_avg, ran) = adaptive_from_identity(&s, &req, &ident, 3, 0.1, 0.99).unwrap();
        assert!(ran, "margin_min=0.99 should force permute cycles");
    }

    #[test]
    fn adaptive_skips_when_margin_zero() {
        let req = DecisionRequest {
            id: "p".into(),
            state: "alpha evidence matches yes strongly".into(),
            question: "Pick yes?".into(),
            options: vec![
                OptionSpec {
                    id: "yes".into(),
                    description: "alpha evidence yes strongly".into(),
                },
                OptionSpec {
                    id: "no".into(),
                    description: "unrelated zebra".into(),
                },
            ],
        };
        let s = HeuristicScorer;
        let ident = s.score_one(&req).unwrap();
        let probs = softmax_labeled(&ident);
        let margin = crate::semif_cascade::top_margin(
            &probs.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
        );
        assert!(margin > 0.0, "heuristic should peak on yes");
        let (_avg, ran) = adaptive_from_identity(&s, &req, &ident, 2, 0.1, 0.0).unwrap();
        // margin_min=0 always treats any positive margin as sharp enough.
        assert!(!ran, "margin_min=0 should skip extra cycles when margin>0");
    }
}
