//! Provider-independent usage model for CodexBar: accounts, metrics, severity and urgency ordering.

mod account;
pub mod alerts;
pub mod demo;
pub mod format;
pub mod layout;
mod metric;
mod severity;

pub use account::{AccountDetail, AccountId, AccountSnapshot, Provider, WindowCurve, sort_by_urgency};
pub use metric::{Currency, Metric, Money, Pace, format_credits, group_thousands};
pub use severity::{Assessment, Severity, assess, observed_severity};
