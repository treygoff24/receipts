use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::providers::Spend;

#[derive(Debug)]
pub struct Budget {
    max_dollars: Option<f64>,
    max_seconds: Option<u64>,
    start: Instant,
    hit: OnceLock<&'static str>,
}

impl Budget {
    pub fn new(max_dollars: Option<f64>, max_seconds: Option<u64>) -> Self {
        Self {
            max_dollars,
            max_seconds,
            start: Instant::now(),
            hit: OnceLock::new(),
        }
    }

    /// Pre-launch projection gate. Returns `true` when the projected unit cost
    /// fits within both the dollar and seconds caps; otherwise records the hit
    /// reason (which sticks) and returns `false`.
    ///
    /// # Concurrency contract
    ///
    /// Calls **must be serialized by the caller** — receipts serializes via
    /// `StageContext::may_launch`'s `budget_gate` mutex, which holds an
    /// exclusive lock across the read of `spend` and the elapsed-time check.
    /// This check is TOCTOU-racy without that serialization: it reads `spend`
    /// and the elapsed time without holding any lock across the subsequent
    /// launch, so concurrent calls can jointly overshoot the cap. The pipeline
    /// serializes every `may_launch` call through `StageContext`, which makes
    /// this safe. Overshoot is bounded at one in-flight unit per concurrent
    /// worker — the design's stated bound.
    pub fn may_launch(&self, spend: &Spend, projected_unit_cost: f64) -> bool {
        if self.hit.get().is_some() {
            return false;
        }

        if self
            .max_dollars
            .is_some_and(|cap| spend.total_dollars() + projected_unit_cost > cap)
        {
            self.hit.set("dollars").expect("budget hit was unset");
            return false;
        }

        if self
            .max_seconds
            .is_some_and(|cap| self.start.elapsed() > Duration::from_secs(cap))
        {
            self.hit.set("seconds").expect("budget hit was unset");
            return false;
        }

        true
    }

    pub fn hit(&self) -> Option<&str> {
        self.hit.get().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_caps_always_allows_launch() {
        let budget = Budget::new(None, None);

        assert!(budget.may_launch(&Spend::default(), f64::MAX));
        assert_eq!(budget.hit(), None);
    }

    #[test]
    fn dollar_cap_blocks_projected_overspend_and_sticks() {
        let budget = Budget::new(Some(0.10), None);
        let spend = Spend {
            dollars: 0.09,
            ..Spend::default()
        };

        assert!(!budget.may_launch(&spend, 0.02));
        assert_eq!(budget.hit(), Some("dollars"));
        assert!(!budget.may_launch(&Spend::default(), 0.0));
        assert_eq!(budget.hit(), Some("dollars"));
    }

    #[test]
    fn seconds_cap_blocks_after_elapsed_time() {
        let budget = Budget::new(None, Some(0));
        std::thread::sleep(Duration::from_millis(2));

        assert!(!budget.may_launch(&Spend::default(), 0.0));
        assert_eq!(budget.hit(), Some("seconds"));
    }
}
