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

    pub fn may_launch(&self, spend: &Spend, projected_unit_cost: f64) -> bool {
        if self.hit.get().is_some() {
            return false;
        }

        if self
            .max_dollars
            .is_some_and(|cap| spend.total_dollars() + projected_unit_cost > cap)
        {
            let _ = self.hit.set("dollars");
            return false;
        }

        if self
            .max_seconds
            .is_some_and(|cap| self.start.elapsed() > Duration::from_secs(cap))
        {
            let _ = self.hit.set("seconds");
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
