use std::fmt;

/// Why a provider could not report usage. Messages are user-facing and never contain credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// No local sign-in was found; `hint` says how to create one.
    NotSignedIn { hint: &'static str },
    /// The provider rejected the stored credentials.
    Expired { hint: &'static str },
    /// The provider answered with an unexpected HTTP status.
    Http { status: u16 },
    /// The request never completed (DNS, TLS, timeout, offline).
    Network,
    /// The provider asked us to wait; no request is made before `retry_at`.
    RateLimited { retry_at: chrono::DateTime<chrono::Utc> },
    /// The response did not have the expected shape.
    Unexpected { detail: &'static str },
    /// The provider understood the credentials but refused this request; `reason` says why, as a full sentence.
    Refused { reason: &'static str },
}

impl ProviderError {
    /// The message, with any time in it formatted by `time` (the app passes the user's locale, #84). `Display` uses
    /// a 24-hour clock in the local timezone.
    pub fn describe_with(&self, time: &dyn Fn(chrono::DateTime<chrono::Utc>) -> String) -> String {
        match self {
            Self::RateLimited { retry_at } => {
                format!("Rate-limited by the provider; retrying at {}.", time(*retry_at))
            }
            other => other.to_string(),
        }
    }

    /// The sign-in or key is the problem (missing, expired, or rejected by the provider), so the fix is in Settings
    /// rather than a retry (#188). A 403 isn't one: providers use it for refusals that no key fixes (Claude's
    /// organization policy, Copilot's enterprise billing permission), and an expired token is reported as `Expired`.
    pub fn needs_sign_in(&self) -> bool {
        matches!(
            self,
            Self::NotSignedIn { .. } | Self::Expired { .. } | Self::Http { status: 401 }
        )
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSignedIn { hint } => write!(f, "Not signed in. {hint}"),
            Self::Expired { hint } => write!(f, "Sign-in expired. {hint}"),
            Self::Http { status } => write!(f, "The usage service returned HTTP {status}."),
            Self::Network => write!(f, "Couldn't reach the usage service. Check your connection."),
            Self::RateLimited { retry_at } => write!(
                f,
                "Rate-limited by the provider; retrying at {}.",
                retry_at.with_timezone(&chrono::Local).format("%H:%M")
            ),
            Self::Unexpected { detail } => write!(f, "Unexpected usage response: {detail}."),
            Self::Refused { reason } => f.write_str(reason),
        }
    }
}

impl std::error::Error for ProviderError {}
