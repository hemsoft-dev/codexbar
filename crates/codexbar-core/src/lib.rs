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

/// The name of the platform's secret store, mid-sentence, as a string literal for `concat!` (#161).
#[cfg(windows)]
#[macro_export]
macro_rules! secret_store_name {
    () => {
        "Windows Credential Manager"
    };
}

/// The name of the platform's secret store, mid-sentence, as a string literal for `concat!` (#161).
#[cfg(target_os = "macos")]
#[macro_export]
macro_rules! secret_store_name {
    () => {
        "the macOS Keychain"
    };
}

/// The name of the platform's secret store, mid-sentence, as a string literal for `concat!` (#161).
#[cfg(not(any(windows, target_os = "macos")))]
#[macro_export]
macro_rules! secret_store_name {
    () => {
        "the Secret Service keyring"
    };
}
