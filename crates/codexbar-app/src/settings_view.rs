//! Settings (#91): General, Accounts, Alerts, Groups (#89), Appearance, Widgets and About, built on gpui-kit's
//! `Settings`. Changes apply and save immediately under the shared settings lock; account edits go through a dialog
//! with Cancel, and destructive actions ask first.

use std::cell::RefCell;
use std::rc::Rc;

use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsPanel};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, IconName, IndexPath, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::catalog::{self, PROVIDERS, SecretSpec};
use crate::notifications::{Notifications, NotifierStatus};
use crate::prefs_hub::PrefsHub;
use crate::settings_hub::{SettingsHub, describe_source};
use codexbar_store::credentials::SecretSource;

/// Refresh choices required by #91, in minutes; 0 is Off.
const REFRESH_CHOICES: [u64; 6] = [0, 1, 5, 15, 30, 60];

pub fn render(_: &mut Window, cx: &mut App) -> impl IntoElement {
    let notice = SettingsHub::global(cx).notice();
    v_flex()
        .size_full()
        .gap_3()
        .when_some(notice, |this, notice| {
            this.child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().warning)
                    .text_sm()
                    .child(
                        gpui_kit::component::Icon::new(IconName::TriangleAlert)
                            .small()
                            .text_color(cx.theme().warning),
                    )
                    .child(notice),
            )
        })
        .child(
            SettingsPanel::new("codexbar-settings")
                .sidebar_width(crate::zoom::scaled(200., cx))
                .pages(vec![
                    general_page(cx),
                    accounts_page(cx),
                    alerts_page(cx),
                    crate::groups_page::groups_page(cx),
                    appearance_page(),
                    // Widgets exist only on the Windows 11 Widgets board (#94).
                    #[cfg(windows)]
                    crate::widget_builder::widgets_page(cx),
                    about_page(),
                ]),
        )
}

/// What alerts arrive as, at the start of a sentence.
const NOTIFICATIONS: &str = if cfg!(windows) {
    "Windows notifications"
} else {
    "Notifications"
};

fn minutes_label(minutes: u64) -> SharedString {
    match minutes {
        0 => "Off".into(),
        1 => "Every minute".into(),
        m => format!("Every {m} minutes").into(),
    }
}

/// The stored interval in minutes, 0 for off. A value the WPF app wrote that isn't a standard choice is kept
/// and shown as its own option rather than misreported.
fn current_minutes(cx: &App) -> u64 {
    SettingsHub::global(cx)
        .settings()
        .refresh_interval_secs()
        .map_or(0, |secs| secs.div_ceil(60).max(1))
}

fn general_page(cx: &App) -> SettingPage {
    let current = current_minutes(cx);
    let mut choices: Vec<u64> = REFRESH_CHOICES.to_vec();
    if !choices.contains(&current) {
        choices.push(current);
        choices.sort_unstable();
    }
    let options = choices
        .iter()
        .map(|minutes| (SharedString::from(minutes.to_string()), minutes_label(*minutes)))
        .collect();
    let page = SettingPage::new("General")
        .icon(IconName::Settings)
        .default_open(true)
        .group(
            SettingGroup::new().title("Refresh").item(
                SettingItem::new(
                    "Auto refresh",
                    SettingField::dropdown(
                        options,
                        |cx: &App| SharedString::from(current_minutes(cx).to_string()),
                        |value: SharedString, cx: &mut App| {
                            let minutes: u64 = value.parse().unwrap_or(2);
                            let secs = (minutes > 0).then_some(minutes * 60);
                            let _ = SettingsHub::update(cx, |settings| {
                                settings.set_refresh_interval_secs(secs);
                                Ok(())
                            });
                        },
                    ),
                )
                .description("How often usage is fetched in the background. Refresh now is always in the title bar."),
            ),
        );
    // The package's startup task (#93); the build from source has none.
    if crate::package::installed().is_none() {
        return page;
    }
    {
        use crate::package::StartupSetting;
        let note = StartupSetting::get(cx).map_or(
            "Windows didn't say whether CodexBar starts with it.",
            crate::package::Startup::note,
        );
        page.group(
            SettingGroup::new().title("Startup").item(
                SettingItem::new(
                    "Start with Windows",
                    SettingField::switch(
                        |cx: &App| StartupSetting::get(cx).is_some_and(crate::package::Startup::is_on),
                        |on: bool, cx: &mut App| StartupSetting::set(cx, on),
                    ),
                )
                .description(note),
            ),
        )
    }
}

fn accounts_page(cx: &App) -> SettingPage {
    let hub = SettingsHub::global(cx);
    let mut groups = Vec::new();
    for info in &PROVIDERS {
        let records: Vec<AccountRecord> = hub.settings().accounts_for(info.id).cloned().collect();
        let mut group = SettingGroup::new().title(info.display).description(info.sign_in);
        if records.is_empty() {
            group = group.item(SettingItem::render(move |_, _, cx| {
                let enabled = SettingsHub::global(cx).settings().is_enabled(info.id);
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if enabled {
                                "Uses the default sign-in. Add an account to name or configure it."
                            } else {
                                "Off"
                            }),
                    )
                    .child(add_button(info.id))
            }));
        } else {
            for record in records {
                group = group.item(account_item(record));
            }
            if info.multi_account {
                group = group.item(SettingItem::render(move |_, _, _| {
                    h_flex().w_full().justify_end().child(add_button(info.id))
                }));
            }
        }
        groups.push(group);
    }
    groups.push(
        SettingGroup::new().title("Reset").item(
            SettingItem::render(|_, _, _| {
                h_flex().w_full().justify_end().child(
                    Button::new("reset-accounts")
                        .danger()
                        .small()
                        .label("Reset accounts…")
                        .on_click(|_, window, cx| confirm_reset(window, cx)),
                )
            })
            .description("Removes every account and its saved keys. Usage history and provider sign-ins are kept."),
        ),
    );
    SettingPage::new("Accounts").icon(IconName::CircleUser).groups(groups)
}

fn add_button(provider: &'static str) -> impl IntoElement {
    Button::new(SharedString::from(format!("add-{provider}")))
        .small()
        .label("Add account…")
        .on_click(move |_, window, cx| open_account_dialog(provider, None, window, cx))
}

/// One account row: name, method and secret status, an on/off switch, Edit… and Remove….
fn account_item(record: AccountRecord) -> SettingItem {
    let keywords = [record.label.clone(), record.provider.clone()];
    SettingItem::render(move |_, _, cx| {
        let hub = SettingsHub::global(cx);
        let current = hub
            .settings()
            .accounts()
            .iter()
            .find(|a| a.id == record.id)
            .cloned()
            .unwrap_or_else(|| record.clone());
        let info = catalog::info(&current.provider);
        let secret = info
            .secret
            .as_ref()
            .map(|_| describe_source(&hub.secret_for(&current).1));
        // An account CodexBar signs in (#78-#80) shows who it is signed in as; its external id is internal.
        let managed = crate::managed::Managed::of(&current);
        let detail = match (&current.external_id, &secret) {
            _ if managed.is_some() => format!(
                "{} · {}",
                current.method.label(),
                managed.map(|kind| kind.describe(hub, &current)).unwrap_or_default()
            ),
            (Some(user), _) => format!("{} · {user}", current.method.label()),
            (None, Some(source)) => format!("{} · {source}", current.method.label()),
            (None, None) => current.method.label().to_owned(),
        };
        let signed_in = managed.is_some_and(|kind| kind.signed_in(hub, &current));
        let sign_in_id = current.id.clone();
        let sign_out_id = current.id.clone();
        let read_only = hub.is_read_only();
        let toggle_id = current.id.clone();
        let edit_id = current.id.clone();
        let remove = current.clone();
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .children(
                                crate::brand::from_settings_name(&current.provider)
                                    .map(|provider| crate::brand::badge(provider, current.id.clone(), cx)),
                            )
                            .child(div().font_semibold().child(current.label.clone()))
                            .when(!current.enabled, |this| {
                                this.child(Tag::secondary().small().child("Off"))
                            }),
                    )
                    .child(
                        // Method and where the secret comes from (never the secret itself), also for screen readers.
                        div()
                            .id(SharedString::from(format!("account-detail-{}", current.id)))
                            .role(Role::Note)
                            .test_support()
                            .aria_label(detail.clone())
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(detail),
                    ),
            )
            .child(
                gpui_kit::component::switch::Switch::new(SharedString::from(format!("enabled-{}", current.id)))
                    .checked(current.enabled)
                    .disabled(read_only)
                    .on_click(move |checked: &bool, _, cx| {
                        let checked = *checked;
                        let id = toggle_id.clone();
                        let _ = SettingsHub::update(cx, |settings| {
                            let Some(record) = settings.accounts().iter().find(|a| a.id == id).cloned() else {
                                return Ok(());
                            };
                            settings.upsert(AccountRecord {
                                enabled: checked,
                                ..record
                            })
                        });
                    }),
            )
            .when_some(managed, |this, kind| {
                this.child(
                    Button::new(SharedString::from(format!("sign-in-{}", current.id)))
                        .small()
                        .ghost()
                        .label(if signed_in { "Sign in again…" } else { "Sign in…" })
                        .disabled(read_only)
                        .on_click(move |_, window, cx| kind.sign_in(&sign_in_id, window, cx)),
                )
                .when(signed_in, |this| {
                    this.child(
                        Button::new(SharedString::from(format!("sign-out-{}", current.id)))
                            .small()
                            .ghost()
                            .label("Sign out")
                            .disabled(read_only)
                            .on_click(move |_, window, cx| kind.sign_out(&sign_out_id, window, cx)),
                    )
                })
            })
            .child(
                Button::new(SharedString::from(format!("edit-{}", current.id)))
                    .small()
                    .ghost()
                    .label("Edit…")
                    .disabled(read_only)
                    .on_click(move |_, window, cx| {
                        let record = SettingsHub::global(cx)
                            .settings()
                            .accounts()
                            .iter()
                            .find(|a| a.id == edit_id)
                            .cloned();
                        if let Some(record) = record {
                            open_account_dialog(catalog::info(&record.provider).id, Some(record), window, cx);
                        }
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("remove-{}", current.id)))
                    .small()
                    .ghost()
                    .label("Remove…")
                    .disabled(read_only)
                    .on_click(move |_, window, cx| confirm_remove(remove.clone(), window, cx)),
            )
    })
    .keywords(keywords)
}

/// The fields of the add/edit dialog, created once per dialog so typing survives re-renders.
struct AccountForm {
    providers: Entity<SelectState<Vec<SharedString>>>,
    methods: Entity<SelectState<Vec<SharedString>>>,
    label: Entity<InputState>,
    secret: Entity<InputState>,
    username: Entity<InputState>,
    workspace: Entity<InputState>,
    enterprise: Entity<InputState>,
    organization: Entity<InputState>,
    pool_total: Entity<InputState>,
    /// GitHub Billing (#104): the organization to follow, blank for the signed-in user's own billing.
    billing_org: Entity<InputState>,
    /// Gemini (#107): whether to show the Antigravity CLI's coding quotas.
    gemini_coding: Entity<SelectState<Vec<SharedString>>>,
    error: Rc<RefCell<Option<SharedString>>>,
}

fn open_account_dialog(provider: &'static str, existing: Option<AccountRecord>, window: &mut Window, cx: &mut App) {
    if SettingsHub::global(cx).is_read_only() {
        return;
    }
    let provider_ix = PROVIDERS.iter().position(|info| info.id == provider).unwrap_or(0);
    let method = existing
        .as_ref()
        .map_or(catalog::info(provider).setup_method, |r| r.method);
    let method_ix = AuthMethod::ALL.iter().position(|m| *m == method).unwrap_or(0);
    let form = Rc::new(AccountForm {
        providers: cx.new(|cx| {
            SelectState::new(
                PROVIDERS.iter().map(|i| SharedString::from(i.display)).collect(),
                Some(IndexPath::default().row(provider_ix)),
                window,
                cx,
            )
        }),
        methods: cx.new(|cx| {
            SelectState::new(
                AuthMethod::ALL.iter().map(|m| SharedString::from(m.label())).collect(),
                Some(IndexPath::default().row(method_ix)),
                window,
                cx,
            )
        }),
        label: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Work, Personal…")
                .default_value(
                    existing
                        .as_ref()
                        .map_or_else(|| catalog::info(provider).display.to_owned(), |r| r.label.clone()),
                )
        }),
        secret: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Leave blank to keep the current one")
        }),
        username: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("GitHub username, blank for every gh account")
                .default_value(
                    existing
                        .as_ref()
                        .and_then(|r| r.external_id.clone())
                        .unwrap_or_default(),
                )
        }),
        workspace: cx.new(|cx| {
            InputState::new(window, cx).placeholder("wrk_…").default_value(
                existing
                    .as_ref()
                    .and_then(|r| r.workspace_id.clone())
                    .unwrap_or_default(),
            )
        }),
        enterprise: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enterprise slug, for org billing")
                .default_value(
                    existing
                        .as_ref()
                        .and_then(|r| r.copilot_enterprise.clone())
                        .unwrap_or_default(),
                )
        }),
        organization: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Organization slug, for org billing")
                .default_value(
                    existing
                        .as_ref()
                        .and_then(|r| r.copilot_organization.clone())
                        .unwrap_or_default(),
                )
        }),
        pool_total: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Blank: seats × monthly allowance")
                .default_value(
                    existing
                        .as_ref()
                        .and_then(|r| r.copilot_pool_total)
                        .map(|total| total.to_string())
                        .unwrap_or_default(),
                )
        }),
        billing_org: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Blank for your own billing")
                .default_value(
                    existing
                        .as_ref()
                        .and_then(|r| r.billing_organization.clone())
                        .unwrap_or_default(),
                )
        }),
        gemini_coding: cx.new(|cx| {
            let on = existing.as_ref().is_some_and(|r| r.gemini_coding);
            SelectState::new(
                vec![
                    SharedString::from("Off"),
                    SharedString::from("From the Antigravity CLI (agy)"),
                ],
                Some(IndexPath::default().row(usize::from(on))),
                window,
                cx,
            )
        }),
        error: Rc::default(),
    });
    let editing = existing.is_some();
    let title = if editing { "Edit account" } else { "Add account" };

    let name_field = form.label.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let form_for_ok = form.clone();
        let existing = existing.clone();
        let provider_ix = form
            .providers
            .read(cx)
            .selected_index(cx)
            .map_or(provider_ix, |ix| ix.row);
        let info = &PROVIDERS[provider_ix.min(PROVIDERS.len() - 1)];
        let oauth = form
            .methods
            .read(cx)
            .selected_index(cx)
            .is_some_and(|ix| AuthMethod::ALL.get(ix.row) == Some(&AuthMethod::OAuth));
        let error = form.error.borrow().clone();
        let field = |label: &'static str, control: AnyElement| {
            v_flex()
                .gap_1()
                .child(div().text_sm().font_semibold().child(label))
                .child(control)
        };
        dialog
            .title(title)
            .w(crate::zoom::scaled(520., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(field(
                        "Provider",
                        Select::new(&form.providers)
                            .disabled(editing)
                            .accessibility_label("Provider")
                            .into_any_element(),
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(info.sign_in),
                    )
                    .child(field("Name", Input::new(&form.label).into_any_element()))
                    .child(field(
                        "Sign-in method",
                        Select::new(&form.methods)
                            .accessibility_label("Sign-in method")
                            .into_any_element(),
                    ))
                    // An OpenCode account on OAuth gets its workspace from the sign-in and needs no cookie.
                    .when_some(info.secret.as_ref().filter(|_| !(oauth && info.id == names::OPENCODE_GO)), |this, spec| {
                        this.child(field(
                            spec.label,
                            Input::new(&form.secret).mask_toggle().into_any_element(),
                        ))
                        .child({
                            // Where the secret goes and what the variable does, also for screen readers; never
                            // the variable's value.
                            let hub = SettingsHub::global(cx);
                            let hint = secret_hint(spec, &hub.prospective_secret_source(info.id, existing.as_ref()));
                            div()
                                .id("account-secret-hint")
                                .role(Role::Note)
                                .test_support()
                                .aria_label(hint.clone())
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(hint)
                        })
                    })
                    .when(info.id == names::CODEX, |this| {
                        this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                            "OAuth: after adding, CodexBar opens your browser to sign this account in to ChatGPT \
                             through the Codex CLI. Automatic: uses the sign-in from `codex`.",
                        ))
                    })
                    .when(info.id == names::CURSOR, |this| {
                        this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                            "OAuth: after adding, CodexBar opens your browser to sign this account in to Cursor through \
                             the Cursor CLI, apart from the Cursor app's sign-in. Automatic: uses the Cursor app's.",
                        ))
                    })
                    .when(info.id == names::CLAUDE, |this| {
                        this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                            "Browser session: after adding, CodexBar opens a Claude Code window to sign this account \
                             in to Claude, apart from Claude Code's own sign-in. OAuth or Automatic: uses the sign-in \
                             from `claude`.",
                        ))
                    })
                    .when(info.id == names::COPILOT && !oauth, |this| {
                        this.child(field("GitHub username", Input::new(&form.username).into_any_element()))
                    })
                    .when(info.id == names::GEMINI, |this| {
                        this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                            "Browser session: after adding, CodexBar opens a temporary browser window for the Google \
                             sign-in of Gemini Apps.",
                        ))
                        .child(field(
                            "Coding quotas",
                            Select::new(&form.gemini_coding)
                                .accessibility_label("Coding quotas")
                                .into_any_element(),
                        ))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                            "From the Antigravity CLI shows the Gemini Models and Other models quotas of the account \
                             agy is signed in to. Choose it only if that is this same Google account: CodexBar can't \
                             check. One Gemini account can show them.",
                        ))
                    })
                    .when(info.id == names::GITHUB_BILLING, |this| {
                        this.child(field("Organization", Input::new(&form.billing_org).into_any_element()))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                                "After adding, CodexBar signs this account in with your browser through the GitHub \
                                 CLI, apart from Copilot. Your own billing needs GitHub's user permission; an \
                                 organization's needs you to be an owner or billing manager there.",
                            ))
                    })
                    .when(info.id == names::COPILOT, |this| {
                        this.when(oauth, |this| {
                            this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                                "OAuth: after adding, CodexBar signs this account in with your browser through the \
                                 GitHub CLI, apart from the GitHub CLI's own accounts.",
                            ))
                        })
                        .child(field("Enterprise", Input::new(&form.enterprise).into_any_element()))
                        .child(field("Organization", Input::new(&form.organization).into_any_element()))
                        .child(field(
                            "Pool total (AI credits a month)",
                            Input::new(&form.pool_total).into_any_element(),
                        ))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                            "Org billing: for a Copilot Enterprise seat, shows the organization's AI credits this \
                             month and this account's share. Leave blank to skip.",
                        ))
                    })
                    .when(info.id == names::OPENCODE_GO && !oauth, |this| {
                        this.child(field("Workspace id", Input::new(&form.workspace).into_any_element()))
                    })
                    .when(info.id == names::OPENCODE_GO && oauth, |this| {
                        this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                            "OAuth: after adding, CodexBar opens OpenCode's approval page in your browser. Approve \
                             CodexBar there and pick the workspace; nothing needs copying. Signing out only removes \
                             CodexBar's copy of the sign-in.",
                        ))
                    })
                    .when_some(error, |this, error| {
                        this.child(
                            div()
                                .id("account-error")
                                .role(Role::Alert)
                                .test_support()
                                .aria_label(error.clone())
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
                    }),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.outline().label("Cancel")))
                    .child(
                        DialogAction::new().child(Button::new("account-save").primary().label(if editing {
                            "Save"
                        } else {
                            "Add"
                        })),
                    ),
            )
            .on_ok(move |_, window, cx| save_account(&form_for_ok, existing.clone(), window, cx))
    });
    focus_and_select(&name_field, window, cx);
}

/// Puts the keyboard in a dialog's first text field with its text selected, so typing replaces the default.
pub fn focus_and_select(field: &Entity<InputState>, window: &mut Window, cx: &mut App) {
    field.update(cx, |state, cx| {
        state.focus(window, cx);
        state.select_all(window, cx);
    });
}

/// Validates and saves the dialog. Returns false (keeping the dialog open) on any failure.
/// Why an X Subscription account can't be saved, if it can't (#172): it reads this computer's one Grok CLI and Cursor
/// sign-in, so it is Automatic, and there is one. `editing` is the account being edited, if any.
fn x_subscription_refusal(
    method: AuthMethod,
    editing: Option<&str>,
    accounts: &[AccountRecord],
) -> Option<&'static str> {
    if method != AuthMethod::Automatic {
        Some("An X Subscription account uses the Grok CLI's and the Cursor app's own sign-ins: choose Automatic.")
    } else if accounts
        .iter()
        .any(|other| other.provider == names::X_SUBSCRIPTION && Some(other.id.as_str()) != editing)
    {
        Some("There is already an X Subscription account; it reads this computer's Grok and Cursor sign-ins.")
    } else {
        None
    }
}

/// Under the secret field: where a saved secret goes, and what the environment variable does. When the variable is set
/// right now, the user is told that it is in use until a secret is saved here (#186). Never the value.
fn secret_hint(spec: &SecretSpec, source: &SecretSource) -> String {
    let store = codexbar_store::credentials::STORE_NAME;
    let label = spec.label.to_lowercase();
    match source {
        SecretSource::Environment(name) => format!(
            "{name} is set in the environment and is used while this account has no saved {label}. Saving one here, \
             in {store}, replaces it for this account."
        ),
        // Its own saved key, or the key the provider's first account inherits (#74).
        SecretSource::CredentialManager | SecretSource::SettingsFile => {
            format!("An existing {label} is used for this account; one entered here replaces it, in {store}.")
        }
        SecretSource::Missing | SecretSource::StoreUnavailable(_) => format!(
            "Saved in {store}. The {} environment variable is used only when none is saved.",
            spec.env
        ),
    }
}

fn save_account(form: &AccountForm, existing: Option<AccountRecord>, window: &mut Window, cx: &mut App) -> bool {
    let provider_ix = form.providers.read(cx).selected_index(cx).map_or(0, |ix| ix.row);
    let info = &PROVIDERS[provider_ix.min(PROVIDERS.len() - 1)];
    let method_ix = form.methods.read(cx).selected_index(cx).map_or(0, |ix| ix.row);
    let method = AuthMethod::ALL[method_ix.min(AuthMethod::ALL.len() - 1)];
    let label = form.label.read(cx).value().trim().to_owned();
    let secret = form.secret.read(cx).value().trim().to_owned();
    let username = form.username.read(cx).value().trim().to_owned();
    let workspace = form.workspace.read(cx).value().trim().to_owned();
    let enterprise = form.enterprise.read(cx).value().trim().to_owned();
    let organization = form.organization.read(cx).value().trim().to_owned();
    let billing_org = form.billing_org.read(cx).value().trim().to_owned();
    let gemini_coding = form
        .gemini_coding
        .read(cx)
        .selected_index(cx)
        .is_some_and(|ix| ix.row == 1);
    if info.id == names::GITHUB_BILLING
        && !billing_org.is_empty()
        && !codexbar_providers::github_billing::valid_login(&billing_org)
    {
        *form.error.borrow_mut() = Some("That isn't a GitHub organization name.".into());
        window.refresh();
        return false;
    }
    let pool_total = form.pool_total.read(cx).value().trim().replace([',', '_'], "");
    let pool_total = match pool_total.as_str() {
        "" => None,
        text => match text.parse::<u64>() {
            Ok(total) if total > 0 => Some(total),
            _ => {
                *form.error.borrow_mut() = Some("The pool total is a whole number of AI credits.".into());
                window.refresh();
                return false;
            }
        },
    };

    if info.id == names::X_SUBSCRIPTION
        && let Some(error) = x_subscription_refusal(
            method,
            existing.as_ref().map(|record| record.id.as_str()),
            SettingsHub::global(cx).settings().accounts(),
        )
    {
        *form.error.borrow_mut() = Some(error.into());
        window.refresh();
        return false;
    }

    let previous = existing.clone();
    let mut record = existing.unwrap_or_else(|| AccountRecord::new(info.id, &label, method));
    record.label = label;
    record.method = method;
    if info.id == names::COPILOT
        && method != AuthMethod::OAuth
        && username.is_empty()
        && (!enterprise.is_empty() || !organization.is_empty() || pool_total.is_some())
    {
        *form.error.borrow_mut() = Some("Org billing needs this account's GitHub username.".into());
        window.refresh();
        return false;
    }
    if info.id == names::COPILOT {
        // An account CodexBar signs in gets its username from the sign-in.
        if method != AuthMethod::OAuth {
            record.external_id = (!username.is_empty()).then_some(username);
        }
        record.copilot_enterprise = (!enterprise.is_empty()).then_some(enterprise);
        record.copilot_organization = (!organization.is_empty()).then_some(organization);
        record.copilot_pool_total = pool_total;
    }
    // The Antigravity CLI has one sign-in, so one Gemini account shows its coding quotas (#107).
    if info.id == names::GEMINI {
        if gemini_coding
            && let Some(other) = SettingsHub::global(cx)
                .settings()
                .accounts()
                .iter()
                .find(|other| other.id != record.id && other.gemini_coding)
        {
            *form.error.borrow_mut() = Some(
                format!(
                    "“{}” already shows the Antigravity CLI's coding quotas. It has one sign-in.",
                    other.label
                )
                .into(),
            );
            window.refresh();
            return false;
        }
        record.gemini_coding = gemini_coding;
    }
    // A billing account only signs in through the GitHub CLI (#104). Following another owner needs a new sign-in, which
    // checks access to that owner's billing. The account becomes a new one, under a new id, so the old owner's spend
    // history and last result are deleted with the old id rather than shown as the new owner's; the old token goes
    // once the settings are saved.
    let mut billing_owner_changed = false;
    let mut replaced_billing_id = None;
    if info.id == names::GITHUB_BILLING {
        record.method = AuthMethod::OAuth;
        let organization = (!billing_org.is_empty()).then_some(billing_org);
        billing_owner_changed = previous.is_some()
            && !record
                .billing_organization
                .as_deref()
                .unwrap_or_default()
                .eq_ignore_ascii_case(organization.as_deref().unwrap_or_default());
        record.billing_organization = organization;
        if billing_owner_changed {
            let new_id = AccountRecord::new(info.id, &record.label, method).id;
            replaced_billing_id = Some(std::mem::replace(&mut record.id, new_id));
            record.external_id = None;
        }
    }
    // Gemini Apps only signs in through the temporary browser (#106); another method would leave the account with no
    // way to sign in.
    if info.id == names::GEMINI {
        record.method = AuthMethod::BrowserSession;
    }
    // An OpenCode account on OAuth keeps the workspace its sign-in set (#102).
    let opencode_oauth = info.id == names::OPENCODE_GO && method == AuthMethod::OAuth;
    if info.id == names::OPENCODE_GO && !opencode_oauth {
        record.workspace_id = (!workspace.is_empty()).then_some(workspace);
        // Only one OpenCode Go account reads a dashboard cookie; more workspaces are added as OAuth accounts.
        let other_cookie = SettingsHub::global(cx).settings().accounts().iter().any(|other| {
            other.id != record.id
                && other.provider.eq_ignore_ascii_case(names::OPENCODE_GO)
                && !crate::opencode_sign_in::is_managed(other)
        });
        if other_cookie {
            *form.error.borrow_mut() = Some(
                "Only one OpenCode Go account can use a dashboard cookie. Use OAuth to add another workspace.".into(),
            );
            window.refresh();
            return false;
        }
        // A workspace an OAuth account already tracks would show twice.
        if let Some(workspace) = record.workspace_id.clone()
            && let Some(other) = SettingsHub::global(cx).settings().accounts().iter().find(|other| {
                other.id != record.id
                    && other.provider.eq_ignore_ascii_case(names::OPENCODE_GO)
                    && other.workspace_id.as_deref() == Some(workspace.as_str())
            })
        {
            *form.error.borrow_mut() =
                Some(format!("Workspace {workspace} is already added as “{}”.", other.label).into());
            window.refresh();
            return false;
        }
    }
    // An OpenCode account leaving OAuth with a new cookie: its saved sign-in, which may span several credential
    // parts, is replaced by the cookie, so nothing is left to forget after saving.
    let replaces_opencode_sign_in = info.id == names::OPENCODE_GO
        && !opencode_oauth
        && !secret.is_empty()
        && previous.as_ref().is_some_and(crate::opencode_sign_in::is_managed);

    // The secret is written first: if Credential Manager fails, the account isn't saved without its key. An OpenCode
    // account on OAuth keeps its sign-in there instead, so no cookie is written over it.
    // The sign-in a cookie replaces, put back if the settings can't be saved after it.
    let mut replaced_sign_in = None;
    if info.secret.is_some() && !secret.is_empty() && !opencode_oauth {
        let store = SettingsHub::global(cx).credentials();
        let written = if replaces_opencode_sign_in {
            // One write that leaves the old sign-in readable until the cookie is in place, with no renewal writing
            // at the same time.
            codexbar_providers::opencode_console::exclusive(|| {
                replaced_sign_in = codexbar_store::credentials::read_long(store.as_ref(), &record.id)?;
                codexbar_store::credentials::write_long(store.as_ref(), &record.id, &secret)
            })
        } else {
            store.write(&record.id, &secret)
        };
        if let Err(err) = written {
            *form.error.borrow_mut() = Some(err.to_string().into());
            window.refresh();
            return false;
        }
    }
    use crate::managed::Managed;
    let kind = Managed::of(&record);
    // A new account on a CodexBar sign-in, or one switched to it, is signed in right after saving.
    let sign_in_now = billing_owner_changed
        || kind.is_some()
            && !SettingsHub::global(cx)
                .settings()
                .accounts()
                .iter()
                .any(|existing| existing.id == record.id && Managed::of(existing) == kind);
    // An account leaving a CodexBar sign-in (OAuth to Automatic, say) is signed out and what CodexBar kept is deleted.
    let left = previous.as_ref().and_then(|old| {
        Managed::of(old)
            .filter(|old_kind| Some(*old_kind) != kind)
            .filter(|_| !replaces_opencode_sign_in)
            .map(|old_kind| (old_kind, old.clone()))
    });
    let record_id = record.id.clone();
    // The first Codex account CodexBar signs in joins the Codex CLI's own sign-in rather than replacing it: that one
    // was showing as the implicit account and stays, as an account of its own the user can switch off or remove.
    // The same holds for Claude Code's own sign-in when the first Claude account CodexBar signs in is added (#80).
    let own_sign_in = match kind {
        Some(Managed::Codex) => Some(names::CODEX),
        Some(Managed::Claude) => Some(names::CLAUDE),
        Some(Managed::Cursor) => Some(names::CURSOR),
        _ => None,
    };
    let keep_cli_account = own_sign_in.filter(|provider| {
        let settings = SettingsHub::global(cx).settings();
        previous.is_none() && settings.is_enabled(provider) && settings.accounts_for(provider).next().is_none()
    });
    let saved = SettingsHub::update(cx, |settings| {
        if let Some(provider) = keep_cli_account {
            settings.upsert(SettingsHub::implicit_account(provider))?;
        }
        if let Some(old_id) = &replaced_billing_id {
            settings.remove(old_id);
        }
        settings.upsert(record)
    });
    match saved {
        Ok(()) => {
            if let Some(old_id) = &replaced_billing_id
                && let Err(err) =
                    codexbar_store::credentials::delete_long(SettingsHub::global(cx).credentials().as_ref(), old_id)
            {
                SettingsHub::set_error(
                    cx,
                    Some(format!("The previous owner's sign-in couldn't be deleted: {err}").into()),
                );
            }
            if let Some((old_kind, old)) = left {
                old_kind.forget(old, cx);
            }
            if let Some(kind) = kind.filter(|_| sign_in_now) {
                // After this dialog has closed.
                window.defer(cx, move |window, cx| kind.sign_in(&record_id, window, cx));
            }
            true
        }
        Err(err) => {
            // The account is still on OAuth, so it gets its sign-in back.
            if let Some(previous) = replaced_sign_in {
                let store = SettingsHub::global(cx).credentials();
                let _ = codexbar_providers::opencode_console::exclusive(|| {
                    codexbar_store::credentials::write_long(store.as_ref(), &record_id, &previous)
                });
            }
            *form.error.borrow_mut() = Some(err.to_string().into());
            window.refresh();
            false
        }
    }
}

fn confirm_remove(record: AccountRecord, window: &mut Window, cx: &mut App) {
    let label = record.label.clone();
    // Only accounts that hold a pasted secret have anything in Credential Manager to delete. An account that is its own
    // dashboard account loses its stored usage too (#85); one that keeps showing through the provider's sign-in keeps it.
    let owns_account = crate::providers::owned_account_ids(SettingsHub::global(cx)).contains_key(&record.id);
    let description = SharedString::from(match (catalog::info(&record.provider).secret.is_some(), owns_account) {
        _ if crate::managed::Managed::of(&record).is_some() => {
            "It is signed out and CodexBar's sign-in for it is deleted, with its usage history. The provider's own sign-in is kept.".to_owned()
        }
        (true, true) => format!(
            "Its saved key is deleted from {}, and its usage history is deleted.",
            codexbar_store::credentials::STORE_SHORT_NAME
        ),
        (true, false) => format!(
            "Its saved key is deleted from {}. Usage history is kept.",
            codexbar_store::credentials::STORE_SHORT_NAME
        ),
        (false, true) => "Its usage history is deleted. The provider's own sign-in is kept.".to_owned(),
        (false, false) => "The provider's own sign-in and usage history are kept.".to_owned(),
    });
    window.open_alert_dialog(cx, move |alert, _, _| {
        let record = record.clone();
        alert
            .title(format!("Remove “{label}”?"))
            .description(description.clone())
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Remove")
                    .ok_variant(ButtonVariant::Danger)
                    .show_cancel(true),
            )
            .on_ok(move |_, _, cx| {
                let id = record.id.clone();
                // The provider's first account also uses the key kept under its implicit account (#74); removing it
                // deletes that one too, so the provider doesn't go on signing in with it.
                let first = SettingsHub::global(cx)
                    .settings()
                    .accounts_for(&record.provider)
                    .next()
                    .is_some_and(|account| account.id == id);
                let result = SettingsHub::update(cx, |settings| {
                    settings.remove(&id);
                    Ok(())
                });
                if result.is_ok()
                    && let Some(kind) = crate::managed::Managed::of(&record)
                {
                    kind.forget(record.clone(), cx);
                }
                let mut ids = vec![id.clone()];
                if first && catalog::info(&record.provider).secret.is_some() {
                    ids.push(codexbar_store::settings::legacy_id(
                        catalog::info(&record.provider).id,
                        "",
                    ));
                }
                if result.is_ok()
                    && let Some(err) = ids.iter().find_map(|id| {
                        // Tokens may be split across parts (#77); delete them all.
                        codexbar_store::credentials::delete_long(SettingsHub::global(cx).credentials().as_ref(), id)
                            .err()
                    })
                {
                    SettingsHub::set_error(
                        cx,
                        Some(format!("The account was removed, but its key couldn't be deleted: {err}").into()),
                    );
                }
                true
            })
    });
}

fn confirm_reset(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _, _| {
        alert
            .title("Reset all accounts?")
            .description("Every account and its saved keys are removed. OpenRouter and Moonshot accounts, and ChatGPT, Copilot, Claude, Cursor and OpenCode accounts CodexBar signed in, also lose their usage history; providers that fall back to their default sign-in, including Copilot, keep theirs. This can't be undone.")
            .button_props(DialogButtonProps::default().ok_text("Reset accounts").ok_variant(ButtonVariant::Danger).show_cancel(true))
            .on_ok(|_, _, cx| {
                // Every account's key, and each key-based provider's implicit one (#74).
                let records: Vec<AccountRecord> = SettingsHub::global(cx).settings().accounts().to_vec();
                let mut ids: Vec<String> = records.iter().map(|a| a.id.clone()).collect();
                ids.extend(
                    PROVIDERS
                        .iter()
                        .filter(|info| info.secret.is_some())
                        .map(|info| codexbar_store::settings::legacy_id(info.id, "")),
                );
                if SettingsHub::update(cx, |settings| {
                    settings.clear_accounts();
                    Ok(())
                })
                .is_ok()
                {
                    // Codex accounts CodexBar signed in are signed out and their folders deleted (#78).
                    for record in records.clone() {
                        if let Some(kind) = crate::managed::Managed::of(&record) {
                            kind.forget(record, cx);
                        }
                    }
                    let store = SettingsHub::global(cx).credentials();
                    let failed = ids.iter().filter(|id| codexbar_store::credentials::delete_long(store.as_ref(), id).is_err()).count();
                    if failed > 0 {
                        SettingsHub::set_error(cx, Some(format!("Accounts were reset, but {failed} saved keys couldn't be deleted.").into()));
                    }
                }
                true
            })
    });
}

#[cfg(windows)]
pub(crate) fn info_item(title: &'static str, body: &'static str) -> SettingItem {
    SettingItem::render(move |_, _, cx| {
        v_flex()
            .gap_1()
            .child(div().font_semibold().child(title))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(body))
    })
}

/// Usage alert choices, in percent used.
const USAGE_ALERT_CHOICES: [u32; 10] = [50, 55, 60, 65, 70, 75, 80, 85, 90, 95];
/// Balance alert choices, in dollars left.
const BALANCE_ALERT_CHOICES: [u32; 6] = [1, 2, 5, 10, 20, 50];

fn usage_percent(settings: &codexbar_core::alerts::AlertSettings) -> u32 {
    (settings.usage_threshold * 100.0).round() as u32
}

/// The balance threshold as its option value: whole dollars without decimals, others with cents.
fn balance_value(settings: &codexbar_core::alerts::AlertSettings) -> String {
    let dollars = settings.balance_threshold;
    if dollars.fract() == 0.0 {
        format!("{dollars:.0}")
    } else {
        format!("{dollars:.2}")
    }
}

fn alerts_page(cx: &App) -> SettingPage {
    let settings = PrefsHub::alert_settings(cx);
    let off = !settings.enabled;
    // A stored value that isn't a standard choice (written by hand or an older build) is kept and shown as its own
    // option rather than misreported, as the refresh interval does.
    let mut usage_choices: Vec<u32> = USAGE_ALERT_CHOICES.to_vec();
    let usage_current = usage_percent(&settings);
    if !usage_choices.contains(&usage_current) {
        usage_choices.push(usage_current);
        usage_choices.sort_unstable();
    }
    let usage_options = usage_choices
        .iter()
        .map(|percent| {
            (
                SharedString::from(percent.to_string()),
                SharedString::from(format!("{percent}% used")),
            )
        })
        .collect();
    let mut balance_choices: Vec<String> = BALANCE_ALERT_CHOICES
        .iter()
        .map(|dollars| dollars.to_string())
        .collect();
    let balance_current = balance_value(&settings);
    if !balance_choices.contains(&balance_current) {
        balance_choices.push(balance_current);
        balance_choices.sort_by(|a, b| {
            a.parse::<f64>()
                .unwrap_or(0.0)
                .total_cmp(&b.parse::<f64>().unwrap_or(0.0))
        });
    }
    let balance_options = balance_choices
        .iter()
        .map(|dollars| {
            (
                SharedString::from(dollars.clone()),
                SharedString::from(format!("Under ${dollars}")),
            )
        })
        .collect();
    SettingPage::new("Alerts")
        .icon(IconName::Bell)
        .group(
            SettingGroup::new()
                .title("Usage alerts")
                .description(format!("{NOTIFICATIONS} when an account needs attention. Each one notifies once, and again only after it recovers."))
                .item(
                    SettingItem::new(
                        "Alerts",
                        SettingField::switch(
                            |cx: &App| PrefsHub::alert_settings(cx).enabled,
                            |on: bool, cx: &mut App| PrefsHub::update_alert_settings(cx, |s| s.enabled = on),
                        ),
                    )
                    .description("Checked after every successful refresh."),
                )
                .item(
                    SettingItem::new(
                        "Usage alert",
                        SettingField::dropdown(
                            usage_options,
                            |cx: &App| SharedString::from(usage_percent(&PrefsHub::alert_settings(cx)).to_string()),
                            |value: SharedString, cx: &mut App| {
                                if let Ok(percent) = value.parse::<f64>() {
                                    PrefsHub::update_alert_settings(cx, |s| s.usage_threshold = percent / 100.0);
                                }
                            },
                        ),
                    )
                    .description("Notify when a limit reaches this much of its allowance.")
                    .disabled(off),
                )
                .item(
                    SettingItem::new(
                        "Balance alert",
                        SettingField::dropdown(
                            balance_options,
                            |cx: &App| SharedString::from(balance_value(&PrefsHub::alert_settings(cx))),
                            |value: SharedString, cx: &mut App| {
                                if let Ok(dollars) = value.parse::<f64>() {
                                    PrefsHub::update_alert_settings(cx, |s| s.balance_threshold = dollars);
                                }
                            },
                        ),
                    )
                    .description("Notify when prepaid credit drops below this.")
                    .disabled(off),
                )
                .item(
                    SettingItem::new(
                        "At risk",
                        SettingField::switch(
                            |cx: &App| PrefsHub::alert_settings(cx).warning,
                            |on: bool, cx: &mut App| PrefsHub::update_alert_settings(cx, |s| s.warning = on),
                        ),
                    )
                    .description("Warning: a limit is on pace to run out before it resets.")
                    .disabled(off),
                )
                .item(
                    SettingItem::new(
                        "Limit soon",
                        SettingField::switch(
                            |cx: &App| PrefsHub::alert_settings(cx).critical,
                            |on: bool, cx: &mut App| PrefsHub::update_alert_settings(cx, |s| s.critical = on),
                        ),
                    )
                    .description("Critical: a limit is about to run out.")
                    .disabled(off),
                ),
        )
        .group(SettingGroup::new().title("Notifications").item(SettingItem::render(|_, _, cx| {
            let status = match Notifications::status(cx) {
                Some(NotifierStatus::Ready) | None => None,
                Some(NotifierStatus::Blocked(reason)) => Some(reason),
            };
            let problem = PrefsHub::error(cx)
                .or_else(|| Notifications::problem(cx))
                .map(|problem| problem.to_string())
                .or(status);
            let active = Notifications::active(cx).len();
            let summary = match (&problem, active) {
                (Some(problem), _) => problem.clone(),
                (None, 0) => format!("{NOTIFICATIONS} are on. No alerts are active."),
                (None, 1) => format!("{NOTIFICATIONS} are on. 1 alert is active until it recovers."),
                (None, n) => format!("{NOTIFICATIONS} are on. {n} alerts are active until they recover."),
            };
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .id("alerts-status")
                        .role(gpui_kit::Role::Status)
                        .test_support()
                        .aria_label(summary.clone())
                        .text_sm()
                        .when(problem.is_some(), |this| this.text_color(cx.theme().warning))
                        .child(summary),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("alerts-test")
                                .label("Send a test notification")
                                .outline()
                                .small()
                                .on_click(|_, _, cx| Notifications::send_test(cx)),
                        )
                        .child(
                            Button::new("alerts-reset")
                                .label("Reset active alerts")
                                .outline()
                                .small()
                                .disabled(active == 0)
                                .on_click(|_, _, cx| Notifications::reset(cx)),
                        ),
                )
        })))
}

fn appearance_page() -> SettingPage {
    use crate::theme::Appearance;
    let options = Appearance::ALL
        .iter()
        .map(|appearance| {
            (
                SharedString::from(appearance.key()),
                SharedString::from(appearance.label()),
            )
        })
        .collect();
    SettingPage::new("Appearance").icon(IconName::Palette).group(
        SettingGroup::new().title("Theme").item(
            SettingItem::new(
                "Theme",
                SettingField::dropdown(
                    options,
                    |cx: &App| SharedString::from(crate::prefs_hub::PrefsHub::appearance(cx).key()),
                    |value: SharedString, cx: &mut App| {
                        if let Some(appearance) = Appearance::from_key(&value) {
                            crate::prefs_hub::PrefsHub::set_appearance(cx, appearance);
                        }
                    },
                ),
            )
            .description(if cfg!(windows) {
                "System follows the Windows app mode. While Windows high contrast is on, CodexBar uses its colors."
            } else {
                "System follows the system's light or dark appearance."
            }),
        ),
    )
}

fn about_page() -> SettingPage {
    use crate::package;
    #[cfg(windows)]
    use crate::package::Updates;
    let installed = package::installed();
    let version = installed.map_or_else(
        || SharedString::from(env!("CARGO_PKG_VERSION")),
        |installed| SharedString::from(format!("{} (package {})", env!("CARGO_PKG_VERSION"), installed.version)),
    );
    let build = SharedString::from(package::BUILD.unwrap_or("Development build"));
    let source = SharedString::from(match installed {
        None if cfg!(windows) => "Built from source with run.ps1".to_owned(),
        None => "Built from source".to_owned(),
        Some(package::Installed {
            app_installer: Some(channel),
            ..
        }) => format!("MSIX package, updates from {channel}"),
        Some(_) => "MSIX package, installed without an update channel".to_owned(),
    });
    let group = SettingGroup::new()
        .title(if cfg!(windows) {
            "CodexBar for Windows"
        } else {
            "CodexBar"
        })
        .item(SettingItem::new(
            "Version",
            SettingField::render(move |_, _, _| version.clone()),
        ))
        .item(SettingItem::new(
            "Build",
            SettingField::render(move |_, _, _| build.clone()),
        ))
        .item(SettingItem::new(
            "Installed as",
            SettingField::render(move |_, _, _| source.clone()),
        ));
    // Only the Windows package has an update channel (#93).
    #[cfg(windows)]
    let group = group.item(SettingItem::render(|_, _, cx| {
        let state = Updates::state(cx);
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_semibold().child("Updates"))
                    .child(
                        div()
                            .id("update-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(SharedString::from(state.label()))
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(state.label()),
                    ),
            )
            .when(state.can_install(), |this| {
                this.child(
                    Button::new("install-update")
                        .small()
                        .primary()
                        .label("Install and restart")
                        .on_click(|_, _, cx| Updates::install_now(cx)),
                )
            })
            .child(
                Button::new("check-updates")
                    .small()
                    .outline()
                    .label("Check now")
                    .disabled(!state.can_check())
                    .on_click(|_, _, cx| Updates::check_now(cx)),
            )
    }));
    let group = group
        .item(SettingItem::new(
            "Settings file",
            SettingField::render(|_, _, cx| {
                SharedString::from(SettingsHub::global(cx).settings().path().display().to_string())
            }),
        ))
        .item(SettingItem::new(
            "Support",
            SettingField::render(|_, _, _| {
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("report-problem")
                            .small()
                            .outline()
                            .label("Report a problem…")
                            .on_click(|_, _, cx| cx.open_url("https://github.com/hemsoft-dev/codexbar/issues/new")),
                    )
                    .child(
                        Button::new("open-repo")
                            .small()
                            .outline()
                            .label("GitHub…")
                            .on_click(|_, _, cx| cx.open_url("https://github.com/hemsoft-dev/codexbar")),
                    )
            }),
        ));
    SettingPage::new("About").icon(IconName::Info).group(group)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_x_subscription_account_is_automatic_and_only_one() {
        let first = AccountRecord::new(names::X_SUBSCRIPTION, "Me", AuthMethod::Automatic);
        let grok = AccountRecord::new(names::GROK, "Me", AuthMethod::Automatic);
        assert_eq!(
            x_subscription_refusal(AuthMethod::Automatic, None, std::slice::from_ref(&grok)),
            None
        );
        for method in [
            AuthMethod::OAuth,
            AuthMethod::ApiKey,
            AuthMethod::CommandLine,
            AuthMethod::BrowserSession,
        ] {
            assert!(x_subscription_refusal(method, None, &[]).is_some(), "{method:?}");
        }
        let accounts = [grok, first.clone()];
        assert!(
            x_subscription_refusal(AuthMethod::Automatic, None, &accounts).is_some(),
            "a second one is refused"
        );
        assert_eq!(
            x_subscription_refusal(AuthMethod::Automatic, Some(&first.id), &accounts),
            None,
            "the one there can still be edited"
        );
    }
}
