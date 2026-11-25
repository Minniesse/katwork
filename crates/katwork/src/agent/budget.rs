//! The run's token budget, shared by the loop and its tools.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Tokens charged against one run's `max_total_tokens`. Clones share one counter, so a tool's
/// charge (OCR through the gateway) counts against the budget the loop stops on.
#[derive(Debug, Clone)]
pub struct RunBudget {
    max: u64,
    charged: Arc<AtomicU64>,
}

impl RunBudget {
    pub fn new(max_total_tokens: u64) -> RunBudget {
        RunBudget {
            max: max_total_tokens,
            charged: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Adds `tokens`, saturating at `u64::MAX`.
    pub fn charge(&self, tokens: u64) {
        let mut charged = self.charged.load(Ordering::SeqCst);
        loop {
            let next = charged.saturating_add(tokens);
            match self.charged.compare_exchange_weak(
                charged,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual) => charged = actual,
            }
        }
    }

    pub fn charged(&self) -> u64 {
        self.charged.load(Ordering::SeqCst)
    }

    /// The tokens left, never below zero.
    pub fn remaining(&self) -> u64 {
        self.max.saturating_sub(self.charged())
    }
}

#[cfg(test)]
mod tests {
    use super::RunBudget;

    #[test]
    fn run_budget_charge_saturates() {
        let budget = RunBudget::new(10);
        budget.charge(u64::MAX);
        budget.charge(u64::MAX);
        assert_eq!(budget.charged(), u64::MAX);
    }

    #[test]
    fn run_budget_remaining_never_underflows() {
        let budget = RunBudget::new(10);
        let shared = budget.clone();
        shared.charge(4);
        assert_eq!(budget.remaining(), 6);
        shared.charge(100);
        assert_eq!(budget.remaining(), 0);
    }
}
