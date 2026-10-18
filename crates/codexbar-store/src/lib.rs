//! Local persistence for CodexBar: usage history (#85), bounded and deduplicated samples per account and metric in a
//! versioned JSON Lines file storing only account ids, metric keys, numbers and times; history summaries (#86); the
//! shared settings file; Rust-only dashboard preferences; and Windows Credential Manager access.

pub mod credentials;
mod demo;
mod enrich;
mod history;
mod lock;
pub mod prefs;
pub mod settings;
pub mod snapshots;
pub mod summary;
pub mod widgets;

pub use demo::demo_history;
pub use enrich::{TREND_DAYS, enrich};
pub use history::{HistoryStore, Sample, default_history_path};
