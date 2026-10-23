use std::time::SystemTime;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSection {
    /// Missing quota data must never masquerade as an unused allowance.
    pub available: bool,
    pub window_minutes: Option<u64>,
    pub percentage: f64,
    pub resets_at: Option<SystemTime>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageData {
    pub session: UsageSection,
    pub weekly: UsageSection,
    pub fable: UsageSection,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppUsageData {
    pub claude_code: Option<UsageData>,
    pub codex: Option<UsageData>,
    pub antigravity: Option<UsageData>,
}
