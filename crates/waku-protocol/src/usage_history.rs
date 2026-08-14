use std::time::Duration;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const WINDOW_CHOICES: [UsageWindow; 5] = [
    UsageWindow::TrailingDays(7),
    UsageWindow::TrailingDays(30),
    UsageWindow::TrailingDays(90),
    UsageWindow::ThisMonth,
    UsageWindow::LastMonth,
];

pub const MONTHLY_WINDOW: UsageWindow = UsageWindow::Months(12);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum UsageWindow {
    TrailingDays(u32),
    Months(u32),
    ThisMonth,
    LastMonth,
}

impl UsageWindow {
    pub fn bounds(self, today: NaiveDate) -> (NaiveDate, NaiveDate) {
        match self {
            UsageWindow::TrailingDays(days) => (
                today - chrono::Days::new(u64::from(days.saturating_sub(1))),
                today,
            ),
            UsageWindow::Months(months) => (
                first_of_month(today)
                    .checked_sub_months(chrono::Months::new(months.saturating_sub(1)))
                    .unwrap_or_else(|| first_of_month(today)),
                today,
            ),
            UsageWindow::ThisMonth => (first_of_month(today), today),
            UsageWindow::LastMonth => {
                let this_first = first_of_month(today);
                (
                    this_first
                        .checked_sub_months(chrono::Months::new(1))
                        .unwrap_or(this_first),
                    this_first.pred_opt().unwrap_or(today),
                )
            }
        }
    }
}

pub fn first_of_month(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

pub fn enumerate_days(since_day: NaiveDate, until_day: NaiveDate) -> Vec<NaiveDate> {
    let mut days = Vec::new();
    let mut cursor = since_day;
    while cursor <= until_day {
        days.push(cursor);
        cursor = cursor + chrono::Days::new(1);
    }
    days
}

pub fn enumerate_months(since_day: NaiveDate, until_day: NaiveDate) -> Vec<NaiveDate> {
    let mut months = Vec::new();
    let mut cursor = first_of_month(since_day);
    let last = first_of_month(until_day);
    while cursor <= last {
        months.push(cursor);
        let Some(next) = cursor.checked_add_months(chrono::Months::new(1)) else {
            break;
        };
        cursor = next;
    }
    months
}

pub fn days_in_month(first_day: NaiveDate) -> u32 {
    first_day
        .checked_add_months(chrono::Months::new(1))
        .and_then(|next| next.pred_opt())
        .map(|last| last.day())
        .unwrap_or(31)
}

use chrono::Datelike as _;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum UsageProvider {
    Claude,
    Codex,
}

impl UsageProvider {
    pub const ALL: [UsageProvider; 2] = [UsageProvider::Claude, UsageProvider::Codex];

    pub fn label(self) -> &'static str {
        match self {
            UsageProvider::Claude => "Claude Code",
            UsageProvider::Codex => "Codex",
        }
    }

    pub fn index(self) -> usize {
        match self {
            UsageProvider::Claude => 0,
            UsageProvider::Codex => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TokenTotals {
    pub uncached_input: u64,
    pub cached_input: u64,
    pub cache_creation: u64,
    pub output: u64,
    pub reasoning: u64,
}

impl TokenTotals {
    pub fn total(&self) -> u64 {
        self.uncached_input + self.cached_input + self.cache_creation + self.output
    }

    pub fn add(&mut self, other: &TokenTotals) {
        self.uncached_input += other.uncached_input;
        self.cached_input += other.cached_input;
        self.cache_creation += other.cache_creation;
        self.output += other.output;
        self.reasoning += other.reasoning;
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PricingStatus {
    Fresh,
    Cached,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSlice {
    pub provider: UsageProvider,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelSlice {
    pub provider: UsageProvider,
    pub model: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub cost_share: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDay {
    pub cost_usd: f64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DaySlice {
    pub day: NaiveDate,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: [ProviderDay; 2],
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CostQuality {
    pub provider_reported_share: f64,
    pub model_priced_share: f64,
    pub unpriced_share: f64,
    pub cache_savings_usd: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MonthSlice {
    pub first_day: NaiveDate,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: [ProviderDay; 2],
    pub sessions: u64,
    pub active_days: u32,
    pub top_models: Vec<(String, f64)>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSlice {
    pub path: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: [ProviderDay; 2],
    pub sessions: u64,
    pub cost_share: f64,
    pub last_day: Option<NaiveDate>,
    pub top_models: Vec<(String, f64)>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageHistory {
    pub window: UsageWindow,
    pub since_day: NaiveDate,
    pub until_day: NaiveDate,
    pub totals: TokenTotals,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub records: u64,
    pub sessions: u64,
    pub providers: Vec<ProviderSlice>,
    pub models: Vec<ModelSlice>,
    pub daily: Vec<DaySlice>,
    pub months: Vec<MonthSlice>,
    pub projects: Vec<ProjectSlice>,
    pub quality: CostQuality,
    pub pricing: PricingStatus,
    pub scanned_files: usize,
    pub skipped_files: usize,
    pub errors: Vec<String>,
    #[ts(type = "{ secs: number; nanos: number }")]
    pub scan_duration: Duration,
}

impl UsageHistory {
    pub fn day(&self, day: NaiveDate) -> Option<&DaySlice> {
        self.daily
            .binary_search_by_key(&day, |slice| slice.day)
            .ok()
            .map(|index| &self.daily[index])
    }

    pub fn month(&self, first_day: NaiveDate) -> Option<&MonthSlice> {
        self.months
            .binary_search_by_key(&first_day, |slice| slice.first_day)
            .ok()
            .map(|index| &self.months[index])
    }
}
