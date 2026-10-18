//! Accounts CodexBar signs in itself: ChatGPT through the Codex CLI (#78), Copilot through the GitHub CLI (#79),
//! Claude through Claude Code (#80), Cursor through its CLI (#81) and OpenCode through the Console's device approval
//! (#102). Settings uses this one place to sign them in and out, describe them and clean up after them.

use codexbar_store::settings::AccountRecord;
use gpui_kit::{App, Window};

use crate::settings_hub::SettingsHub;
use crate::{
    claude_sign_in, codex_sign_in, cursor_sign_in, gemini_sign_in, github_sign_in, grok_sign_in, opencode_sign_in,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Managed {
    Codex,
    GitHub,
    Claude,
    Cursor,
    OpenCode,
    Gemini,
    Grok,
}

impl Managed {
    /// The kind of CodexBar sign-in `record` uses, if any.
    pub fn of(record: &AccountRecord) -> Option<Self> {
        if codex_sign_in::is_managed(record) {
            Some(Self::Codex)
        } else if github_sign_in::is_managed(record) {
            Some(Self::GitHub)
        } else if claude_sign_in::is_managed(record) {
            Some(Self::Claude)
        } else if cursor_sign_in::is_managed(record) {
            Some(Self::Cursor)
        } else if opencode_sign_in::is_managed(record) {
            Some(Self::OpenCode)
        } else if gemini_sign_in::is_managed(record) {
            Some(Self::Gemini)
        } else if grok_sign_in::is_managed(record) {
            Some(Self::Grok)
        } else {
            None
        }
    }

    pub fn sign_in(self, record_id: &str, window: &mut Window, cx: &mut App) {
        match self {
            Self::Codex => codex_sign_in::sign_in(record_id, window, cx),
            Self::GitHub => github_sign_in::sign_in(record_id, window, cx),
            Self::Claude => claude_sign_in::sign_in(record_id, window, cx),
            Self::Cursor => cursor_sign_in::sign_in(record_id, window, cx),
            Self::OpenCode => opencode_sign_in::sign_in(record_id, window, cx),
            Self::Gemini => gemini_sign_in::sign_in(record_id, window, cx),
            Self::Grok => grok_sign_in::sign_in(record_id, window, cx),
        }
    }

    pub fn sign_out(self, record_id: &str, window: &mut Window, cx: &mut App) {
        match self {
            Self::Codex => codex_sign_in::sign_out(record_id, window, cx),
            Self::GitHub => github_sign_in::sign_out(record_id, cx),
            Self::Claude => claude_sign_in::sign_out(record_id, window, cx),
            Self::Cursor => cursor_sign_in::sign_out(record_id, window, cx),
            Self::OpenCode => opencode_sign_in::sign_out(record_id, cx),
            Self::Gemini => gemini_sign_in::sign_out(record_id, cx),
            Self::Grok => grok_sign_in::sign_out(record_id, cx),
        }
    }

    /// Who the account is signed in as, for its row; never a secret.
    pub fn describe(self, hub: &SettingsHub, record: &AccountRecord) -> String {
        match self {
            Self::Codex => codex_sign_in::describe(hub.dir(), record),
            Self::GitHub => github_sign_in::describe(hub, record),
            Self::Claude => claude_sign_in::describe(hub.dir(), record),
            Self::Cursor => cursor_sign_in::describe(hub.dir(), record),
            Self::OpenCode => opencode_sign_in::describe(hub, record),
            Self::Gemini => gemini_sign_in::describe(hub, record),
            Self::Grok => grok_sign_in::describe(hub.dir(), record),
        }
    }

    pub fn signed_in(self, hub: &SettingsHub, record: &AccountRecord) -> bool {
        match self {
            Self::Codex => codex_sign_in::auth_path(hub.dir(), record).exists(),
            Self::GitHub => matches!(github_sign_in::token(hub, record), Ok(Some(_))),
            Self::Claude => {
                codexbar_providers::claude::seems_signed_in(&claude_sign_in::credentials_path(hub.dir(), record))
            }
            Self::Cursor => cursor_sign_in::auth_path(hub.dir(), record).exists(),
            Self::OpenCode => matches!(opencode_sign_in::credential(hub, record), Ok(Some(_))),
            Self::Gemini => matches!(gemini_sign_in::apps_session(hub, record), Ok(Some(_))),
            Self::Grok => codexbar_providers::grok::read_auth(&grok_sign_in::home(hub.dir(), record)).is_some(),
        }
    }

    /// Cleans up after an account that was removed or left this sign-in: signs it out and deletes what CodexBar kept
    /// for it (a CLI folder, or the GitHub or OpenCode sign-in). Failures are shown in Settings.
    pub fn forget(self, record: AccountRecord, cx: &mut App) {
        match self {
            Self::Codex => codex_sign_in::forget(record, cx),
            Self::Claude => claude_sign_in::forget(record, cx),
            Self::Cursor => cursor_sign_in::forget(record, cx),
            Self::OpenCode => opencode_sign_in::forget(record, cx),
            Self::Gemini => gemini_sign_in::forget(record, cx),
            Self::Grok => grok_sign_in::forget(record, cx),
            Self::GitHub => {
                if let Err(err) =
                    codexbar_store::credentials::delete_long(SettingsHub::global(cx).credentials().as_ref(), &record.id)
                {
                    SettingsHub::set_error(
                        cx,
                        Some(format!("The account's GitHub token couldn't be deleted: {err}").into()),
                    );
                }
            }
        }
    }
}
