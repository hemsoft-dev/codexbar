//! Which providers run, built from the account records in the shared settings file. Environment variables still win
//! for secrets; Credential Manager comes next; plaintext keys from the WPF app are the last fallback.

use std::collections::HashMap;
use std::sync::Arc;

use codexbar_providers::balance::{MoonshotProvider, OpenRouterProvider};
use codexbar_providers::claude::{self, ClaudeCliRenewer, ClaudeProvider};
use codexbar_providers::codex::{self, CodexCliRenewer, CodexProvider};
use codexbar_providers::copilot::{CopilotProvider, OrgBilling};
use codexbar_providers::cursor::{self, CursorProvider};
use codexbar_providers::greptile::GreptileProvider;
use codexbar_providers::opencode::OpenCodeProvider;
use codexbar_providers::opencode_console::OpenCodeConsoleProvider;
use codexbar_providers::{SystemCommandRunner, UreqClient, UsageProvider};
use codexbar_store::settings::{AccountRecord, names};

use crate::settings_hub::SettingsHub;

/// Enabled accounts for a provider, or its implicit legacy account when it has no records and is switched on.
fn enabled_accounts(hub: &SettingsHub, provider: &str) -> Vec<AccountRecord> {
    let settings = hub.settings();
    if !settings.is_enabled(provider) {
        return Vec::new();
    }
    let records: Vec<AccountRecord> = settings.accounts_for(provider).cloned().collect();
    if records.is_empty() {
        vec![SettingsHub::implicit_account(provider)]
    } else {
        records.into_iter().filter(|account| account.enabled).collect()
    }
}

pub fn enabled(hub: &SettingsHub) -> Vec<Arc<dyn UsageProvider>> {
    let mut providers: Vec<Arc<dyn UsageProvider>> = Vec::new();
    providers.extend(codex_adapters(hub));

    if let Some(copilot) = copilot(hub) {
        providers.push(copilot);
    }

    providers.extend(claude_adapters(hub));
    providers.extend(cursor_adapters(hub));

    let openrouter = enabled_accounts(hub, names::OPENROUTER);
    let multiple = openrouter.len() > 1;
    let configured = hub.settings().accounts_for(names::OPENROUTER).next().is_some();
    for account in openrouter {
        let key = hub.secret_for(&account).0;
        let provider = OpenRouterProvider::new(UreqClient::new(), key);
        // A configured account always reports under its own id, so history and preferences survive adding or
        // removing a second one; the implicit account (no records yet) keeps the legacy id until one is configured.
        let provider = if multiple {
            provider.with_account(account.id.clone(), account.label.clone())
        } else if configured {
            provider.with_id(account.id.clone())
        } else {
            provider
        };
        providers.push(Arc::new(provider));
    }

    providers.extend(opencode(hub));

    let moonshot = enabled_accounts(hub, names::MOONSHOT);
    let multiple = moonshot.len() > 1;
    let configured = hub.settings().accounts_for(names::MOONSHOT).next().is_some();
    for account in moonshot {
        let key = hub.secret_for(&account).0;
        let provider = MoonshotProvider::new(UreqClient::new(), key);
        // A configured account always reports under its own id, so history and preferences survive adding or
        // removing a second one; the implicit account (no records yet) keeps the legacy id until one is configured.
        let provider = if multiple {
            provider.with_account(account.id.clone(), account.label.clone())
        } else if configured {
            provider.with_id(account.id.clone())
        } else {
            provider
        };
        providers.push(Arc::new(provider));
    }

    // Greptile (#103) is new: every account reports under its own id from the start, with no legacy id to keep.
    for account in enabled_accounts(hub, names::GREPTILE) {
        let key = hub.secret_for(&account).0;
        providers.push(Arc::new(GreptileProvider::new(
            UreqClient::new(),
            key,
            account.id.clone(),
            account.label.clone(),
        )));
    }
    providers
}

/// One adapter per Codex home (#78): the Codex CLI's own, and one for each account CodexBar signed in. Homes signed in
/// to the same ChatGPT identity (the Codex CLI's and a CodexBar account, say) show it once, through the CodexBar
/// account that owns it, so one identity never appears twice or loses its owner.
fn codex_adapters(hub: &SettingsHub) -> Vec<Arc<dyn UsageProvider>> {
    let mut records = enabled_accounts(hub, names::CODEX);
    let several = records.len() > 1;
    // CodexBar's own accounts first: they own their identity.
    // Signed-in homes first, CodexBar's own first among those: a remembered identity never hides a live sign-in.
    records.sort_by_key(|record| {
        let signed_in = crate::codex_sign_in::auth_path(hub.dir(), record).is_file();
        (!signed_in, !crate::codex_sign_in::is_managed(record))
    });
    let mut homes = Vec::new();
    let mut identities = Vec::new();
    let mut adapters: Vec<Arc<dyn UsageProvider>> = Vec::new();
    for record in records {
        let path = crate::codex_sign_in::auth_path(hub.dir(), &record);
        let managed = crate::codex_sign_in::is_managed(&record);
        // The identity it was signed in to (a CodexBar account remembers it); before the first sign-in, none.
        let identity = if managed { record.external_id.clone() } else { None }
            .or_else(|| codex::signed_in_account(&path).map(|id| id.as_str().to_owned()));
        if homes.contains(&path) || identity.as_ref().is_some_and(|id| identities.contains(id)) {
            continue;
        }
        homes.push(path.clone());
        identities.extend(identity.clone());
        let mut provider = CodexProvider::new(UreqClient::new(), path).with_renewer(Arc::new(CodexCliRenewer));
        if managed {
            provider = provider.managed();
        }
        if managed || several {
            provider = provider.with_account(identity.unwrap_or_else(|| record.id.clone()), record.label.clone());
        }
        adapters.push(Arc::new(provider));
    }
    adapters
}

/// One adapter per Claude Code config folder (#80): Claude Code's own, and one for each account CodexBar signed in.
/// Every adapter renews through Claude Code.
///
/// Only CodexBar's own folders are named by the Claude account in their `.claude.json`: CodexBar alone writes them,
/// through the Claude Code it starts, so that file and the credentials always belong together. In Claude Code's own
/// folder other Claude apps can rewrite `.claude.json` for another account while the CLI's credentials stay on the
/// first (anthropics/claude-code#85294), so that folder keeps the account-neutral `claude` id it always had, and its
/// history stays where it was. Two CodexBar folders signed in to one account show it once.
fn claude_adapters(hub: &SettingsHub) -> Vec<Arc<dyn UsageProvider>> {
    let mut records = enabled_accounts(hub, names::CLAUDE);
    let several = records.len() > 1;
    // CodexBar's own folders first, signed-in ones first among them: a signed-out folder never stands in for a
    // signed-in one holding the same account.
    records.sort_by_key(|record| {
        let managed = crate::claude_sign_in::is_managed(record);
        let signed_in = crate::claude_sign_in::credentials_path(hub.dir(), record).is_file();
        (!signed_in, !managed)
    });
    let mut folders = Vec::new();
    let mut identities = Vec::new();
    let mut adapters: Vec<Arc<dyn UsageProvider>> = Vec::new();
    for record in records {
        let credentials = crate::claude_sign_in::credentials_path(hub.dir(), &record);
        let profile = crate::claude_sign_in::profile_path(hub.dir(), &record);
        let managed = crate::claude_sign_in::is_managed(&record);
        let identity = if managed {
            record
                .external_id
                .clone()
                .or_else(|| claude::signed_in_identity(&profile).map(|(id, _)| id.as_str().to_owned()))
        } else {
            None
        };
        // Signed-in folders come first, so an account already shown by one isn't shown again by a signed-out one.
        if folders.contains(&credentials) || identity.as_ref().is_some_and(|id| identities.contains(id)) {
            continue;
        }
        folders.push(credentials.clone());
        identities.extend(identity.clone());
        let mut provider = ClaudeProvider::new(UreqClient::new(), credentials).with_renewer(Arc::new(ClaudeCliRenewer));
        if managed {
            provider = provider.with_profile(profile).managed();
        }
        if managed || several {
            provider = provider.with_account(identity.unwrap_or_else(|| record.id.clone()), record.label.clone());
        }
        adapters.push(Arc::new(provider));
    }
    adapters
}

/// One adapter per Cursor sign-in (#81): the Cursor app's, and one for each account CodexBar signed in. Folders signed
/// in to the same Cursor account show it once, signed-in ones first.
fn cursor_adapters(hub: &SettingsHub) -> Vec<Arc<dyn UsageProvider>> {
    let mut records = enabled_accounts(hub, names::CURSOR);
    let several = records.len() > 1;
    records.sort_by_key(|record| {
        let managed = crate::cursor_sign_in::is_managed(record);
        (!crate::cursor_sign_in::auth_path(hub.dir(), record).is_file(), !managed)
    });
    let mut paths = Vec::new();
    let mut identities = Vec::new();
    let mut adapters: Vec<Arc<dyn UsageProvider>> = Vec::new();
    for record in records {
        let path = crate::cursor_sign_in::auth_path(hub.dir(), &record);
        let managed = crate::cursor_sign_in::is_managed(&record);
        let identity = if managed { record.external_id.clone() } else { None }
            .or_else(|| cursor::signed_in_account(&path).map(|id| id.as_str().to_owned()));
        if paths.contains(&path) || identity.as_ref().is_some_and(|id| identities.contains(id)) {
            continue;
        }
        paths.push(path.clone());
        identities.extend(identity.clone());
        let mut provider = CursorProvider::new(UreqClient::new(), path);
        if managed {
            provider = provider.managed();
        }
        if managed || several {
            provider = provider.with_account(identity.unwrap_or_else(|| record.id.clone()), record.label.clone());
        }
        adapters.push(Arc::new(provider));
    }
    adapters
}

/// The workspace the implicit OpenCode Go cookie setup tracks (from before accounts, or environment variables), when it
/// is in use: OpenCode Go is on, no cookie record exists (enabled or not), and the setup has a workspace and a cookie.
pub fn implicit_opencode_workspace(hub: &SettingsHub) -> Option<String> {
    let settings = hub.settings();
    let cookie_record = settings
        .accounts_for(names::OPENCODE_GO)
        .any(|record| !crate::opencode_sign_in::is_managed(record));
    if cookie_record || !settings.is_enabled(names::OPENCODE_GO) {
        return None;
    }
    let workspace = std::env::var("OPENCODE_GO_WORKSPACE_ID")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .or_else(|| settings.opencode_workspace_id())?;
    hub.secret_for(&SettingsHub::implicit_account(names::OPENCODE_GO))
        .0
        .map(|_| workspace)
}

/// The dashboard account each configured record owns (#85), by record id: OpenRouter and Moonshot records report under
/// their own id, a Copilot record for one username under that user's id, a Codex account CodexBar signed in under
/// the ChatGPT identity it holds (#78), and an OpenCode account on the Console under its own id (#102). Removing such
/// a record removes that dashboard account. Codex on the Codex CLI's sign-in, Claude, Cursor, OpenCode on a cookie,
/// and Copilot without a username keep showing through the provider's own sign-in after their record is removed, so
/// they own nothing here.
pub fn owned_account_ids(hub: &SettingsHub) -> HashMap<String, String> {
    // A Copilot organization card (#79) is owned while an enabled record has its org billing set, under a key of
    // its own beside the record's user, so removing the last such record forgets the card.
    let orgs = hub.settings().accounts().iter().filter_map(|record| {
        let set = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let billed = record.enabled
            && record.provider == names::COPILOT
            && set(&record.copilot_enterprise).is_some()
            && set(&record.external_id).is_some();
        let organization = set(&record.copilot_organization).filter(|_| billed)?;
        Some((
            format!("{}#org", record.id),
            codexbar_providers::copilot::org_account_id(&organization)
                .as_str()
                .to_owned(),
        ))
    });
    hub.settings()
        .accounts()
        .iter()
        .filter_map(|record| {
            let owned = match record.provider.as_str() {
                names::OPENROUTER | names::MOONSHOT | names::GREPTILE => record.id.clone(),
                names::CODEX if crate::codex_sign_in::is_managed(record) => record.external_id.clone()?,
                names::CLAUDE if crate::claude_sign_in::is_managed(record) => record.external_id.clone()?,
                names::CURSOR if crate::cursor_sign_in::is_managed(record) => record.external_id.clone()?,
                // An OpenCode account on the Console reports under its own id (#102).
                names::OPENCODE_GO if crate::opencode_sign_in::is_managed(record) => record.id.clone(),
                names::COPILOT => {
                    let user = record.external_id.as_deref()?.trim();
                    (!user.is_empty()).then(|| codexbar_providers::copilot::account_id(user).as_str().to_owned())?
                }
                _ => return None,
            };
            Some((record.id.clone(), owned))
        })
        .chain(orgs)
        .collect()
}

/// True when Copilot shows every account the GitHub CLI is signed in to: an enabled Copilot account without a username,
/// including the implicit one when there are no records. A Copilot account then keeps showing after a username record
/// for it is removed, so it isn't forgotten. Disabled records and a switched-off provider show nothing.
pub fn copilot_discovers_all(hub: &SettingsHub) -> bool {
    enabled_accounts(hub, names::COPILOT)
        .iter()
        // An account CodexBar signs in is only ever its own user, signed in or not.
        .filter(|record| !crate::github_sign_in::is_managed(record))
        .any(|record| record.external_id.as_deref().is_none_or(|user| user.trim().is_empty()))
}

/// Accounts that used to report under their provider's legacy id: a provider's only OpenRouter or Moonshot account
/// reported as `openrouter`/`moonshot` before it reported under its configured id. Each pair is (legacy, configured)
/// for moving stored history and preferences. The one configured account counts whether or not it is enabled now (a
/// disabled account keeps its history for when it is enabled again). With no records the implicit account still
/// reports under the legacy id, so nothing moves until the first account is configured; with several configured
/// accounts the legacy history's owner is unknown, so nothing moves.
pub fn legacy_ids(hub: &SettingsHub) -> Vec<(&'static str, String)> {
    let renames: Vec<(&'static str, String)> = [("openrouter", names::OPENROUTER), ("moonshot", names::MOONSHOT)]
        .into_iter()
        .filter_map(|(legacy, provider)| {
            let records: Vec<&AccountRecord> = hub.settings().accounts_for(provider).collect();
            match records.as_slice() {
                [only] => Some((legacy, only.id.clone())),
                _ => None,
            }
        })
        .collect();
    // Cursor's legacy `cursor` history isn't moved (#81): nothing shows which Cursor account it belonged to, and
    // the account signed in now may not be that one. It stays under the legacy id rather than be credited to someone
    // else.
    renames
}

/// One Copilot adapter for every Copilot account (#79): those CodexBar signed in fetch with their own token; those on
/// the GitHub CLI's sign-in are found through `gh` (only the configured usernames, or every gh account when one has
/// none). With only CodexBar's own accounts, the GitHub CLI isn't asked at all. Org billing is per username.
fn copilot(hub: &SettingsHub) -> Option<Arc<dyn UsageProvider>> {
    let records = enabled_accounts(hub, names::COPILOT);
    if records.is_empty() {
        return None;
    }
    let (managed, cli): (Vec<AccountRecord>, Vec<AccountRecord>) =
        records.into_iter().partition(crate::github_sign_in::is_managed);
    let mut provider = CopilotProvider::new(UreqClient::new(), SystemCommandRunner);
    if cli.is_empty() {
        provider = provider.without_cli();
    } else {
        let usernames: Vec<String> = cli.iter().filter_map(|a| a.external_id.clone()).collect();
        // Records without usernames mean "every gh account", as the WPF app's automatic discovery does.
        if usernames.len() == cli.len() {
            provider = provider.only_accounts(usernames);
        }
    }
    for record in &managed {
        let Some(user) = &record.external_id else {
            continue;
        };
        match crate::github_sign_in::token(hub, record) {
            Ok(Some(token)) => provider = provider.with_login(user.clone(), token),
            // Signed out: nothing to fetch.
            Ok(None) => {}
            Err(_) => provider = provider.with_unreadable_login(user.clone()),
        }
    }
    for record in managed.iter().chain(&cli) {
        let field = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        if let (Some(user), Some(enterprise), Some(organization)) = (
            field(&record.external_id),
            field(&record.copilot_enterprise),
            field(&record.copilot_organization),
        ) {
            provider = provider.with_billing(
                &user,
                OrgBilling {
                    enterprise,
                    organization,
                    pool_total: record.copilot_pool_total,
                },
            );
        }
    }
    Some(Arc::new(provider))
}

/// OpenCode accounts signed in through the Console (#102) each report their own workspace's Go limits and Zen balance.
/// The older dashboard-cookie setup is one more account where Go and Zen share one dashboard row; either half can be
/// switched off, and Zen falls back to Go's cookie. Once Console accounts exist, the cookie account runs only when it
/// is actually set up, so it doesn't add a "not signed in" row beside them.
fn opencode(hub: &SettingsHub) -> Vec<Arc<dyn UsageProvider>> {
    let mut adapters: Vec<Arc<dyn UsageProvider>> = Vec::new();
    let (console, go): (Vec<AccountRecord>, Vec<AccountRecord>) = enabled_accounts(hub, names::OPENCODE_GO)
        .into_iter()
        .partition(crate::opencode_sign_in::is_managed);
    for record in &console {
        let slot = crate::opencode_sign_in::Slot {
            store: hub.credentials(),
            account: record.id.clone(),
        };
        adapters.push(Arc::new(OpenCodeConsoleProvider::new(
            // Redirects are never followed: every exchange must stay on the Console.
            UreqClient::without_redirects(),
            Box::new(slot),
            record.id.clone(),
            record.label.clone(),
        )));
    }
    // Adding the first OAuth account doesn't drop a cookie setup that ran as the implicit account (environment variables,
    // or a cookie kept under it): with no cookie record at all, it still stands in when it is set up. A cookie record
    // switched off is never stood in for.
    let go = go.into_iter().next().or_else(|| {
        (!console.is_empty() && implicit_opencode_workspace(hub).is_some())
            .then(|| SettingsHub::implicit_account(names::OPENCODE_GO))
    });
    let zen = enabled_accounts(hub, names::OPENCODE_ZEN).into_iter().next();
    if go.is_none() && zen.is_none() {
        return adapters;
    }
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let workspace = env("OPENCODE_GO_WORKSPACE_ID")
        .or_else(|| go.as_ref().and_then(|a| a.workspace_id.clone()))
        .or_else(|| hub.settings().opencode_workspace_id());
    let go_cookie = go.as_ref().and_then(|account| hub.secret_for(account).0);
    let zen_cookie = zen
        .as_ref()
        .and_then(|account| hub.secret_for(account).0)
        .or_else(|| go_cookie.clone());
    let zen_cookie = zen.is_some().then_some(zen_cookie).flatten();
    if !console.is_empty() && (workspace.is_none() || (go_cookie.is_none() && zen_cookie.is_none())) {
        return adapters;
    }
    adapters.push(Arc::new(OpenCodeProvider::new(
        UreqClient::without_redirects(),
        workspace,
        go_cookie,
        zen_cookie,
    )));
    adapters
}
