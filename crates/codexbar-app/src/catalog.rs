//! What the app knows about each provider: its name, how it signs in, and which secret (if any) it needs.

use codexbar_store::settings::{AuthMethod, names};

/// A secret the user can paste into Settings.
pub struct SecretSpec {
    /// Field label, e.g. "API key".
    pub label: &'static str,
    /// Environment variable that overrides the stored secret.
    pub env: &'static str,
    /// True when an account's saved secret wins over the environment variable, which then only stands in for an
    /// account with none saved: each Greptile key belongs to one organization (#103), so one variable must not answer
    /// for several accounts.
    pub saved_first: bool,
}

pub struct ProviderInfo {
    /// The WPF `ProviderId` name used in settings.
    pub id: &'static str,
    pub display: &'static str,
    /// How this provider signs in when it needs no pasted secret.
    pub sign_in: &'static str,
    pub secret: Option<SecretSpec>,
    pub default_method: AuthMethod,
    /// The method a newly added account starts with; the default sign-in stays `default_method`.
    pub setup_method: AuthMethod,
    /// Whether more than one account can be fetched (key-based providers).
    pub multi_account: bool,
}

pub const PROVIDERS: [ProviderInfo; 11] = [
    ProviderInfo {
        id: names::CODEX,
        display: "ChatGPT · Codex",
        sign_in: "Automatic uses the Codex CLI's own ChatGPT sign-in (`codex`). OAuth signs an account in with your browser \
                  through the Codex CLI and keeps it separate, so several ChatGPT accounts can be added.",
        secret: None,
        default_method: AuthMethod::Automatic,
        setup_method: AuthMethod::Automatic,
        multi_account: true,
    },
    ProviderInfo {
        id: names::CLAUDE,
        display: "Claude",
        sign_in: "OAuth and Automatic use Claude Code's own sign-in (`claude`). Browser session signs an account in \
                  through a Claude Code window and keeps it separate, so several Claude accounts can be added.",
        secret: None,
        default_method: AuthMethod::OAuth,
        setup_method: AuthMethod::OAuth,
        multi_account: true,
    },
    ProviderInfo {
        id: names::COPILOT,
        display: "Copilot",
        sign_in: "Automatic and Command line use the GitHub CLI's accounts (`gh auth login`); set a username to fetch \
                  one. OAuth signs an account in with your browser through the GitHub CLI, kept apart from its own \
                  accounts. Enterprise seats can also show their organization's AI credits.",
        secret: None,
        default_method: AuthMethod::CommandLine,
        setup_method: AuthMethod::CommandLine,
        multi_account: true,
    },
    ProviderInfo {
        id: names::CURSOR,
        display: "Cursor",
        sign_in: "Automatic uses the Cursor app's sign-in. OAuth signs an account in with your browser through the \
                  Cursor CLI (cursor-agent) and keeps it separate, so several Cursor accounts can be added.",
        secret: None,
        default_method: AuthMethod::Automatic,
        setup_method: AuthMethod::Automatic,
        multi_account: true,
    },
    ProviderInfo {
        id: names::OPENROUTER,
        display: "OpenRouter",
        sign_in: "Needs an API key from openrouter.ai/keys.",
        secret: Some(SecretSpec {
            label: "API key",
            env: "OPENROUTER_API_KEY",
            saved_first: false,
        }),
        default_method: AuthMethod::ApiKey,
        setup_method: AuthMethod::ApiKey,
        multi_account: true,
    },
    ProviderInfo {
        id: names::OPENCODE_GO,
        display: "OpenCode Go",
        sign_in: "OAuth signs an account in with your browser through OpenCode's own approval page, where you pick the \
                  workspace; it shows that workspace's Go limits and Zen balance. Add an account per workspace. Browser \
                  session reads a copied dashboard auth cookie and workspace id, the older setup.",
        secret: Some(SecretSpec {
            label: "Dashboard auth cookie",
            env: "OPENCODE_GO_AUTH_COOKIE",
            saved_first: false,
        }),
        default_method: AuthMethod::BrowserSession,
        setup_method: AuthMethod::OAuth,
        multi_account: true,
    },
    ProviderInfo {
        id: names::OPENCODE_ZEN,
        display: "OpenCode Zen",
        sign_in: "Reads a copied opencode.ai dashboard auth cookie (OpenCode Go's when unset). An OpenCode Go account \
                  signed in with OAuth already shows its workspace's Zen balance.",
        secret: Some(SecretSpec {
            label: "Dashboard auth cookie",
            env: "OPENCODE_ZEN_AUTH_COOKIE",
            saved_first: false,
        }),
        default_method: AuthMethod::BrowserSession,
        setup_method: AuthMethod::BrowserSession,
        multi_account: false,
    },
    ProviderInfo {
        id: names::MOONSHOT,
        display: "Moonshot (Kimi)",
        sign_in: "Needs an API key from platform.kimi.ai.",
        secret: Some(SecretSpec {
            label: "API key",
            env: "MOONSHOT_API_KEY",
            saved_first: false,
        }),
        default_method: AuthMethod::ApiKey,
        setup_method: AuthMethod::ApiKey,
        multi_account: true,
    },
    ProviderInfo {
        id: names::GREPTILE,
        display: "Greptile",
        sign_in: "Needs a Greptile organization API key (app.greptile.com > Settings > API). Shows the organization's \
                  completed code reviews, read with Greptile's read-only review listing; not remaining credits.",
        secret: Some(SecretSpec {
            label: "Organization API key",
            env: "GREPTILE_API_KEY",
            saved_first: true,
        }),
        default_method: AuthMethod::ApiKey,
        setup_method: AuthMethod::ApiKey,
        multi_account: true,
    },
    ProviderInfo {
        id: names::GITHUB_BILLING,
        display: "GitHub Billing",
        sign_in: "Signs in with your browser through the GitHub CLI, apart from Copilot and the GitHub CLI's own \
                  accounts, and shows this month's spend across GitHub products for you or an organization you can \
                  read billing for.",
        secret: None,
        default_method: AuthMethod::OAuth,
        setup_method: AuthMethod::OAuth,
        multi_account: true,
    },
    ProviderInfo {
        id: names::GEMINI,
        display: "Gemini",
        sign_in: "Experimental. Browser session opens Gemini in a separate, temporary Edge or Chrome window where you \
                  sign in to Google yourself; CodexBar keeps only the two Google session cookies Gemini Apps usage \
                  needs, and deletes the window's data. Shows the consumer Gemini Apps five-hour and weekly limits.",
        secret: None,
        default_method: AuthMethod::BrowserSession,
        setup_method: AuthMethod::BrowserSession,
        multi_account: true,
    },
];

pub fn info(provider: &str) -> &'static ProviderInfo {
    PROVIDERS
        .iter()
        .find(|info| info.id.eq_ignore_ascii_case(provider))
        .unwrap_or(&PROVIDERS[0])
}
