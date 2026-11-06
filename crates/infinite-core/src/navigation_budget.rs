use crate::PathError;

/// Provisional operation allowance, not a measured CPU-time guarantee.
pub const DEFAULT_NAVIGATION_WORK_LIMIT: u32 = 65_536;

/// Shared work accounting. Failed searches consume work too; there is no refund.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationBudget {
    limit: u32,
    used: u32,
}

impl Default for NavigationBudget {
    fn default() -> Self {
        Self::new(DEFAULT_NAVIGATION_WORK_LIMIT)
    }
}

impl NavigationBudget {
    pub const fn new(limit: u32) -> Self { Self { limit, used: 0 } }
    pub const fn limit(&self) -> u32 { self.limit }
    pub const fn used(&self) -> u32 { self.used }
    pub const fn remaining(&self) -> u32 { self.limit - self.used }

    pub(crate) fn restrict_remaining(&mut self, allowance: u32) -> u32 {
        let remaining = self.remaining().min(allowance);
        self.limit = self.used + remaining;
        remaining
    }

    /// A resumable operation must not mutate its cursor until its unit fits.
    pub(crate) fn try_charge(&mut self, work: u32) -> bool {
        if work > self.remaining() { return false; }
        self.used += work;
        true
    }

    pub(crate) fn charge(&mut self, work: u32) -> Result<(), PathError> {
        if work > self.remaining() {
            // Close this allowance even if a prepaid block did not fit.
            self.used = self.limit;
            return Err(PathError::BudgetExhausted);
        }
        self.used += work;
        Ok(())
    }
}
