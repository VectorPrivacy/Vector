//! The combinator and the frozen selection pipeline (§3.1, §3.2 — wire contract).
//!
//! Convictions combine like independent probabilities, in exact integer math:
//!
//! ```text
//! Q       = PRODUCT over selected convictions of (100 - w_i)     // u128
//! conf_pm = 1000 * (100^n - Q) / 100^n                           // floor, ONE division
//! confidence = conf_pm / 10                                      // floor
//! ```
//!
//! Never stepwise per-mille multiplication: truncation makes it order-dependent
//! and lets four 90s reach a false 100. Weights are 1..=99, so `conf_pm` lives
//! in `{0} ∪ [10, 999]` — confidence can never reach 100, and `Band::Clear`
//! appears only when nothing combined at all.
//!
//! Selection is a fixed sequence; any other order produces different numbers
//! from identical evidence:
//! 1. drop suppressed (they never occupy a slot)
//! 2. fold families to their max tier_weight (ties: `(rule_id, scope)` asc)
//! 3. take the top `COMBINATOR_MAX_CONVICTIONS` by weight desc (same tie-break)
//! 4. combine
//!
//! `proven` runs this ENTIRE pipeline independently over the Deterministic-only
//! subset — its own folds, its own top-N. It is not a filter applied to the
//! confidence selection.

use super::types::{caps, Band, Basis, Conviction};
use std::collections::BTreeMap;

/// Exact combination of already-selected weights. Callers pass at most
/// [`caps::COMBINATOR_MAX_CONVICTIONS`] weights in 1..=99.
pub fn conf_pm(weights: &[u32]) -> u32 {
    if weights.is_empty() {
        return 0;
    }
    debug_assert!(weights.len() <= caps::COMBINATOR_MAX_CONVICTIONS);
    let mut q: u128 = 1;
    let mut d: u128 = 1;
    for &w in weights {
        debug_assert!((caps::WEIGHT_MIN..=caps::WEIGHT_MAX).contains(&w));
        q *= (100 - w) as u128;
        d *= 100u128;
    }
    ((1000 * (d - q)) / d) as u32
}

/// Fixed anchors, compared on per-mille — never on the rounded display value.
pub fn band(conf_pm: u32) -> Band {
    match conf_pm {
        0 => Band::Clear,
        1..=249 => Band::Noted,
        250..=499 => Band::Watch,
        500..=749 => Band::Flagged,
        _ => Band::Alert,
    }
}

/// The score a consumer displays: floor of per-mille over ten.
pub fn confidence(conf_pm: u32) -> u32 {
    conf_pm / 10
}

/// One subject's scores, with the selection flags already written back onto the
/// convictions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubjectScore {
    pub confidence_pm: u32,
    pub confidence: u32,
    pub proven_pm: u32,
    pub proven: u32,
    pub band: Band,
}

/// Selection order: strongest first, ties broken ascending by `(rule_id, scope)`
/// so two implementations pick identical slates.
fn retain_order(convictions: &[Conviction], idx: &mut [usize]) {
    idx.sort_by(|&a, &b| {
        let (ca, cb) = (&convictions[a], &convictions[b]);
        cb.tier_weight
            .cmp(&ca.tier_weight)
            .then_with(|| ca.rule_id.cmp(&cb.rule_id))
            .then_with(|| ca.scope.cmp(&cb.scope))
    });
}

/// Run the frozen pipeline over one subject's convictions, writing
/// `folded`/`folded_into`/`combined`/`proven_combined` onto them, and return the
/// scores.
pub fn run_pipeline(convictions: &mut [Conviction]) -> SubjectScore {
    let conf = select(convictions, false);
    let prov = select(convictions, true);
    SubjectScore {
        confidence_pm: conf,
        confidence: confidence(conf),
        proven_pm: prov,
        proven: confidence(prov),
        band: band(conf),
    }
}

/// One pipeline pass. `deterministic_only` = the proven pass: same steps, its
/// own family folds, its own top-N, flags written to `proven_combined` instead
/// of `combined`/`folded`.
fn select(convictions: &mut [Conviction], deterministic_only: bool) -> u32 {
    // 1. Drop suppressed; the proven pass also drops Heuristic.
    let mut live: Vec<usize> = (0..convictions.len())
        .filter(|&i| {
            let c = &convictions[i];
            !c.suppressed && (!deterministic_only || c.basis == Basis::Deterministic)
        })
        .collect();

    // 2. Fold families: within each tag keep the max tier_weight (ties by
    //    (rule_id, scope) ascending — the retain order's head).
    let mut by_family: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for &i in &live {
        if let Some(f) = convictions[i].family.as_deref() {
            by_family.entry(f).or_default().push(i);
        }
    }
    let mut folded_away: Vec<(usize, usize)> = Vec::new(); // (loser, winner)
    for (_, mut members) in std::mem::take(&mut by_family) {
        if members.len() < 2 {
            continue;
        }
        retain_order(convictions, &mut members);
        let winner = members[0];
        for &loser in &members[1..] {
            folded_away.push((loser, winner));
        }
    }
    for &(loser, winner) in &folded_away {
        live.retain(|&i| i != loser);
        if !deterministic_only {
            let winner_id = convictions[winner].id;
            convictions[loser].folded = true;
            convictions[loser].folded_into = Some(winner_id);
        }
    }

    // 3. Top-N by declared weight.
    retain_order(convictions, &mut live);
    live.truncate(caps::COMBINATOR_MAX_CONVICTIONS);

    for &i in &live {
        if deterministic_only {
            convictions[i].proven_combined = true;
        } else {
            convictions[i].combined = true;
        }
    }

    // 4. Combine.
    let weights: Vec<u32> = live.iter().map(|&i| convictions[i].tier_weight).collect();
    conf_pm(&weights)
}

#[cfg(test)]
mod tests {
    use super::super::types::*;
    use super::*;

    fn conv(rule: &str, scope: Scope, w: u32, basis: Basis, family: Option<&str>) -> Conviction {
        Conviction {
            id: conviction_id(&Hash32([0x11; 32]), rule, scope, 0, &SubjectId([0x22; 32])),
            subject: SubjectId([0x22; 32]),
            rule_id: rule.into(),
            scope,
            rung: 0,
            hits: 1,
            severity: Severity::Major,
            basis,
            tier_weight: w,
            retroactive: Retroactive::Unknown,
            suppressed: false,
            folded: false,
            folded_into: None,
            combined: false,
            proven_combined: false,
            citations: vec![],
            citation_count: 0,
            earliest_citation_at: 0,
            latest_citation_at: 0,
            family: family.map(|s| s.to_string()),
            evidence: vec![],
        }
    }

    /// The §8 reference values — the first conformance vectors. Every number
    /// here was machine-verified before it was frozen; a change to any is a
    /// wire break, not a refactor.
    #[test]
    fn reference_vectors_hold_exactly() {
        assert_eq!((conf_pm(&[99; 12]), confidence(conf_pm(&[99; 12]))), (999, 99), "the ceiling is never 100");
        assert_eq!((conf_pm(&[1; 12]), confidence(conf_pm(&[1; 12]))), (113, 11));
        assert_eq!((conf_pm(&[20; 10]), confidence(conf_pm(&[20; 10]))), (892, 89), "why family folds exist");
        assert_eq!(conf_pm(&[70, 90]), 970, "strict link blocker, 3 links");
        assert_eq!(conf_pm(&[10, 70]), 730, "soft swears, 10 in a window");
        assert_eq!(conf_pm(&[85, 40, 20]), 928, "swarm guard: cohort+burst+folded aggravator");
        assert_eq!(conf_pm(&[20]), 200, "swarm guard proven");
        assert_eq!(conf_pm(&[85, 85]), 977, "spam guard: repeat + cohort");
        assert_eq!(conf_pm(&[]), 0, "nothing combined");
    }

    #[test]
    fn bands_anchor_on_per_mille() {
        assert_eq!(band(0), Band::Clear);
        assert_eq!(band(100), Band::Noted);
        assert_eq!(band(249), Band::Noted);
        assert_eq!(band(250), Band::Watch);
        assert_eq!(band(499), Band::Watch);
        assert_eq!(band(500), Band::Flagged);
        assert_eq!(band(730), Band::Flagged, "the soft swear policy peaks at review, not alert");
        assert_eq!(band(750), Band::Alert);
        assert_eq!(band(999), Band::Alert);
    }

    #[test]
    fn order_never_matters() {
        let a = [5, 15, 95, 40, 70];
        let mut b = a;
        b.reverse();
        assert_eq!(conf_pm(&a), conf_pm(&b));
    }

    /// Swarm Guard end to end: the two fresh-account aggravators fold to one,
    /// confidence combines all planes, proven sees only the folded
    /// deterministic aggravator.
    #[test]
    fn swarm_guard_pipeline_yields_92_over_20() {
        let mut cs = vec![
            conv("cohort", Scope::Whole, 85, Basis::Heuristic, None),
            conv("joinburst", Scope::Whole, 40, Basis::Heuristic, None),
            conv("tenure", Scope::Whole, 20, Basis::Deterministic, Some("fresh-account")),
            conv("quiet", Scope::Whole, 10, Basis::Deterministic, Some("fresh-account")),
        ];
        let s = run_pipeline(&mut cs);
        assert_eq!((s.confidence, s.proven), (92, 20));
        assert_eq!(s.band, Band::Alert);
        let quiet = cs.iter().find(|c| c.rule_id == "quiet").unwrap();
        assert!(quiet.folded && !quiet.combined, "the weaker family member folded away");
        assert_eq!(quiet.folded_into, Some(cs.iter().find(|c| c.rule_id == "tenure").unwrap().id));
        assert!(cs.iter().find(|c| c.rule_id == "tenure").unwrap().combined);
        // The proven pass folds its own family: tenure wins there too.
        assert!(cs.iter().find(|c| c.rule_id == "tenure").unwrap().proven_combined);
        assert!(!cs.iter().find(|c| c.rule_id == "quiet").unwrap().proven_combined);
    }

    #[test]
    fn suppressed_never_occupies_a_slot_and_the_cap_keeps_the_strongest() {
        // 13 convictions; one suppressed heavyweight. The suppressed one takes
        // no slot, the 12 strongest survivors combine, the weakest is left out
        // with combined=false.
        let mut cs: Vec<Conviction> = (0..12).map(|i| conv(&format!("r{i:02}"), Scope::Whole, 50 + i, Basis::Deterministic, None)).collect();
        cs.push(conv("weakest", Scope::Whole, 5, Basis::Deterministic, None));
        let mut boss = conv("boss", Scope::Whole, 99, Basis::Deterministic, None);
        boss.suppressed = true;
        cs.push(boss);
        let s = run_pipeline(&mut cs);
        assert!(!cs.iter().find(|c| c.rule_id == "boss").unwrap().combined, "pardoned: no slot");
        assert!(!cs.iter().find(|c| c.rule_id == "weakest").unwrap().combined, "13th strongest: out");
        assert_eq!(cs.iter().filter(|c| c.combined).count(), 12);
        assert!(s.confidence >= 99 - 1, "twelve 50-61s combine near the ceiling: {}", s.confidence);
    }

    /// A conviction can be in one pipeline and out of the other — the reason
    /// `proven_combined` exists as its own flag.
    #[test]
    fn the_two_pipelines_select_independently() {
        let mut cs: Vec<Conviction> = (0..12).map(|i| conv(&format!("h{i:02}"), Scope::Whole, 90, Basis::Heuristic, None)).collect();
        cs.push(conv("det", Scope::Whole, 10, Basis::Deterministic, None));
        let s = run_pipeline(&mut cs);
        let det = cs.iter().find(|c| c.rule_id == "det").unwrap();
        assert!(!det.combined, "13th by weight: out of confidence");
        assert!(det.proven_combined, "alone in the deterministic subset: in proven");
        assert_eq!(s.proven, 10);
    }
}
