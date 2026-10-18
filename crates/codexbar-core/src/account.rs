use chrono::{DateTime, Utc};

use crate::metric::Metric;
#[cfg(test)]
use crate::severity::Severity;
use crate::severity::{Assessment, assess};

/// An AI provider whose usage CodexBar reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    Codex,
    Claude,
    Copilot,
    Cursor,
    OpenRouter,
    OpenCode,
    Moonshot,
    Greptile,
    GitHubBilling,
    Gemini,
    Grok,
    XSubscription,
}

impl Provider {
    /// The product name shown before the account label.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "ChatGPT · Codex",
            Self::Claude => "Claude",
            Self::Copilot => "Copilot",
            Self::Cursor => "Cursor",
            Self::OpenRouter => "OpenRouter",
            Self::OpenCode => "OpenCode Go + Zen",
            Self::Moonshot => "Moonshot (Kimi)",
            Self::Greptile => "Greptile",
            Self::GitHubBilling => "GitHub Billing",
            Self::Gemini => "Gemini",
            Self::Grok => "Grok",
            Self::XSubscription => "X Subscription",
        }
    }

    pub const ALL: [Self; 12] = [
        Self::Codex,
        Self::Claude,
        Self::Copilot,
        Self::Cursor,
        Self::OpenRouter,
        Self::OpenCode,
        Self::Moonshot,
        Self::Greptile,
        Self::GitHubBilling,
        Self::Gemini,
        Self::Grok,
        Self::XSubscription,
    ];

    /// A stable key for storage; unlike the display name it never changes with wording.
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
            Self::OpenRouter => "openrouter",
            Self::OpenCode => "opencode",
            Self::Moonshot => "moonshot",
            Self::Greptile => "greptile",
            Self::GitHubBilling => "github-billing",
            Self::Gemini => "gemini",
            Self::Grok => "grok",
            Self::XSubscription => "x-subscription",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|provider| provider.key() == key)
    }

    /// The provider an adapter reports as, by its display name (`UsageProvider::name`).
    pub fn from_display_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|provider| provider.display_name() == name)
    }
}

/// Stable identity of one provider account, independent of display order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountId(String);

impl AccountId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Usage within one window, sampled over time, comparing the current window with the previous one.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowCurve {
    labels: Vec<String>,
    current: Vec<f64>,
    previous: Vec<f64>,
}

impl WindowCurve {
    /// Builds a curve. `current` may be shorter than `labels` while the window is still running.
    pub fn new(labels: Vec<String>, current: Vec<f64>, previous: Vec<f64>) -> Self {
        Self {
            labels,
            current,
            previous,
        }
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    pub fn current(&self) -> &[f64] {
        &self.current
    }

    pub fn previous(&self) -> &[f64] {
        &self.previous
    }
}

/// Detail series shown when an account is focused.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountDetail {
    window_curve: Option<WindowCurve>,
    requests_by_hour: Vec<u32>,
}

impl AccountDetail {
    pub fn with_window_curve(mut self, curve: WindowCurve) -> Self {
        self.window_curve = Some(curve);
        self
    }

    pub fn with_requests_by_hour(mut self, requests: Vec<u32>) -> Self {
        self.requests_by_hour = requests;
        self
    }

    pub fn window_curve(&self) -> Option<&WindowCurve> {
        self.window_curve.as_ref()
    }

    pub fn requests_by_hour(&self) -> &[u32] {
        &self.requests_by_hour
    }
}

/// The latest known state of one account.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountSnapshot {
    id: AccountId,
    provider: Provider,
    label: Option<String>,
    metrics: Vec<Metric>,
    trend: Vec<f64>,
    detail: AccountDetail,
    fetched_at: DateTime<Utc>,
    /// What the provider said about the account besides numbers ("Usage is delayed by up to an hour").
    messages: Vec<String>,
}

impl AccountSnapshot {
    /// Creates a snapshot. The first metric is the account's primary limit.
    pub fn new(id: AccountId, provider: Provider, metrics: Vec<Metric>, fetched_at: DateTime<Utc>) -> Self {
        Self {
            id,
            provider,
            label: None,
            metrics,
            trend: Vec::new(),
            detail: AccountDetail::default(),
            fetched_at,
            messages: Vec::new(),
        }
    }

    /// Adds a provider message, shown with the account and kept with its last-good snapshot.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.messages.push(message.into());
        self
    }

    pub fn messages(&self) -> &[String] {
        &self.messages
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Daily pressure for the trend sparkline, oldest first.
    pub fn with_trend(mut self, trend: Vec<f64>) -> Self {
        self.trend = trend;
        self
    }

    pub fn with_detail(mut self, detail: AccountDetail) -> Self {
        self.detail = detail;
        self
    }

    pub fn id(&self) -> &AccountId {
        &self.id
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// "Copilot · work", or the provider name alone when the account has no label.
    pub fn display_name(&self) -> String {
        match &self.label {
            Some(label) if self.provider == Provider::Codex => format!("{} ({label})", self.provider.display_name()),
            Some(label) => format!("{} · {label}", self.provider.display_name()),
            None => self.provider.display_name().to_owned(),
        }
    }

    pub fn metrics(&self) -> &[Metric] {
        &self.metrics
    }

    pub fn primary(&self) -> Option<&Metric> {
        self.metrics.first()
    }

    pub fn trend(&self) -> &[f64] {
        &self.trend
    }

    pub fn detail(&self) -> &AccountDetail {
        &self.detail
    }

    pub fn fetched_at(&self) -> DateTime<Utc> {
        self.fetched_at
    }

    /// The most urgent assessment across all of the account's metrics. A fully used window doesn't block an account
    /// that can still spend credits (#150).
    pub fn assess(&self, now: DateTime<Utc>) -> Assessment {
        self.metrics
            .iter()
            .filter(|metric| !self.credits_cover(metric))
            .map(|metric| assess(metric, now))
            .max()
            .unwrap_or_default()
    }

    /// True when `metric` is a used-up window the account's spendable credits carry it past (#150): it neither blocks
    /// the account nor raises severity alerts.
    pub fn credits_cover(&self, metric: &Metric) -> bool {
        matches!(metric, Metric::Window { .. })
            && metric.is_exhausted()
            && self.metrics.iter().any(Metric::has_spendable_credits)
    }

    /// Puts the account's credits where they belong (#150): first, as the headline, when a window is used up and the
    /// credits can still be spent; otherwise after the limits, as a secondary metric.
    pub fn with_credits_placed(mut self) -> Self {
        let Some(ix) = self
            .metrics
            .iter()
            .position(|metric| matches!(metric, Metric::Credits { .. }))
        else {
            return self;
        };
        let credits = self.metrics.remove(ix);
        let window_used_up = self
            .metrics
            .iter()
            .any(|metric| matches!(metric, Metric::Window { .. }) && metric.is_exhausted());
        if window_used_up && credits.has_spendable_credits() {
            self.metrics.insert(0, credits);
        } else {
            self.metrics.push(credits);
        }
        self
    }

    /// Sets the remembered starting balance on the account's credits, so their bar runs down from it (#150).
    pub fn with_credit_start(mut self, start: f64) -> Self {
        for metric in &mut self.metrics {
            if let Metric::Credits { start: slot, .. } = metric {
                *slot = Some(start);
            }
        }
        self
    }

    /// The account's credit balance, when it reports a counted one.
    pub fn credit_balance(&self) -> Option<f64> {
        self.metrics.iter().find_map(|metric| match metric {
            Metric::Credits { balance, .. } => *balance,
            _ => None,
        })
    }
}

/// Orders accounts so whatever blocks the user soonest comes first. Ties keep provider order stable by id.
pub fn sort_by_urgency(accounts: &mut [AccountSnapshot], now: DateTime<Utc>) {
    accounts.sort_by(|a, b| {
        b.assess(now)
            .cmp(&a.assess(now))
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
}

#[cfg(test)]
mod credits_tests {
    use chrono::Duration;

    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    fn weekly(used: f64) -> Metric {
        Metric::Window {
            label: "Weekly".into(),
            used,
            resets_at: now() + Duration::days(3),
            pace: None,
        }
    }

    fn credits(balance: Option<f64>) -> Metric {
        Metric::Credits {
            label: "Credits".into(),
            balance,
            start: None,
        }
    }

    fn account(metrics: Vec<Metric>) -> AccountSnapshot {
        AccountSnapshot::new(AccountId::new("codex"), Provider::Codex, metrics, now())
    }

    #[test]
    fn an_exhausted_window_with_credits_makes_credits_the_headline() {
        let placed = account(vec![credits(Some(500.0)), weekly(1.0)]).with_credits_placed();
        assert!(matches!(placed.primary(), Some(Metric::Credits { .. })));
        let unlimited = account(vec![weekly(1.0), credits(None)]).with_credits_placed();
        assert!(matches!(unlimited.primary(), Some(Metric::Credits { .. })));
    }

    #[test]
    fn an_open_window_or_no_credits_keeps_the_window_headline() {
        let open = account(vec![credits(Some(500.0)), weekly(0.6)]).with_credits_placed();
        assert_eq!(open.primary(), Some(&weekly(0.6)));
        assert!(matches!(open.metrics().last(), Some(Metric::Credits { .. })));
        let empty = account(vec![credits(Some(0.0)), weekly(1.0)]).with_credits_placed();
        assert_eq!(
            empty.primary(),
            Some(&weekly(1.0)),
            "zero credits can't carry the account"
        );
    }

    #[test]
    fn spendable_credits_keep_an_exhausted_window_from_blocking() {
        let on_credits = account(vec![weekly(1.0), credits(Some(500.0))]).with_credits_placed();
        let assessment = on_credits.assess(now());
        assert_eq!(assessment.severity(), Severity::Normal);
        assert!(!assessment.is_exhausted());
        let blocked = account(vec![weekly(1.0), credits(Some(0.0))]);
        assert_eq!(blocked.assess(now()).severity(), Severity::LimitSoon);
        assert!(blocked.assess(now()).is_exhausted());
        assert_eq!(blocked.assess(now()).label(), None, "no pill on a used-up limit");
    }

    #[test]
    fn credit_start_lands_on_the_credits_metric() {
        let account = account(vec![weekly(1.0), credits(Some(40.0))]).with_credit_start(100.0);
        assert_eq!(account.credit_balance(), Some(40.0));
        let progress = account
            .metrics()
            .iter()
            .find(|metric| matches!(metric, Metric::Credits { .. }))
            .and_then(Metric::progress);
        assert!((progress.unwrap() - 0.6).abs() < 1e-9);
    }
}
