//! Settings → Groups (#89): create, rename, reorder and delete account groups. Accounts are assigned from the
//! focused account on the Usage view, where their dashboard ids are known.

use std::cell::RefCell;
use std::rc::Rc;

use codexbar_core::layout::Group;
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Entity, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use crate::prefs_hub::PrefsHub;

pub fn groups_page(cx: &App) -> SettingPage {
    let groups = PrefsHub::layout(cx).groups().to_vec();
    let count = groups.len();
    let mut list = SettingGroup::new()
        .title("Groups")
        .description("Groups organize the Usage table. Assign an account from its heading on the Usage view.");
    // A change that couldn't be saved stays in memory; say so here, where it was made.
    if let Some(error) = PrefsHub::error(cx) {
        list = list.item(SettingItem::render(move |_, _, cx| {
            div()
                .id("groups-error")
                .role(gpui_kit::Role::Alert)
                .test_support()
                .aria_label(error.clone())
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone())
        }));
    }
    if groups.is_empty() {
        list = list.item(SettingItem::render(|_, _, cx| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No groups yet. Every account is Ungrouped.")
        }));
    }
    for (ix, group) in groups.into_iter().enumerate() {
        list = list.item(group_item(group, ix, count));
    }
    list = list.item(SettingItem::render(|_, _, cx| {
        h_flex().w_full().justify_end().child(
            Button::new("group-new")
                .small()
                .label("New group…")
                .disabled(PrefsHub::is_read_only(cx))
                .on_click(|_, window, cx| open_name_dialog(None, window, cx)),
        )
    }));
    SettingPage::new("Groups")
        .icon(IconName::LayoutDashboard)
        .groups(vec![list])
}

/// One group row: its name, Move up / Move down, Rename… and Delete….
fn group_item(group: Group, ix: usize, count: usize) -> SettingItem {
    let keywords = [group.name.clone()];
    SettingItem::render(move |_, _, cx| {
        let read_only = PrefsHub::is_read_only(cx);
        let mover = |delta: isize, label: &'static str, disabled: bool| {
            let id = group.id.clone();
            Button::new(SharedString::from(format!("group-{}-{label}", group.id)))
                .small()
                .ghost()
                .label(label)
                .disabled(disabled || read_only)
                .on_click(move |_, _, cx| {
                    let _ = PrefsHub::update_layout(cx, |layout| {
                        Ok::<_, codexbar_core::layout::LayoutError>(layout.move_group(&id, delta))
                    });
                })
        };
        let rename = group.clone();
        let delete = group.clone();
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_semibold()
                    .child(group.name.clone()),
            )
            .child(mover(-1, "Move up", ix == 0))
            .child(mover(1, "Move down", ix + 1 >= count))
            .child(
                Button::new(SharedString::from(format!("group-{}-rename", group.id)))
                    .small()
                    .ghost()
                    .label("Rename…")
                    .disabled(read_only)
                    .on_click(move |_, window, cx| open_name_dialog(Some(rename.clone()), window, cx)),
            )
            .child(
                Button::new(SharedString::from(format!("group-{}-delete", group.id)))
                    .small()
                    .ghost()
                    .label("Delete…")
                    .disabled(read_only)
                    .on_click(move |_, window, cx| confirm_delete(delete.clone(), window, cx)),
            )
    })
    .keywords(keywords)
}

/// The New group / Rename dialog. A duplicate or empty name keeps it open with the reason.
fn open_name_dialog(existing: Option<Group>, window: &mut Window, cx: &mut App) {
    let name: Entity<InputState> = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Work, Personal…")
            .default_value(existing.as_ref().map(|group| group.name.clone()).unwrap_or_default())
    });
    let error: Rc<RefCell<Option<SharedString>>> = Rc::default();
    let renaming = existing.is_some();
    let name_field = name.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let shown_error = error.borrow().clone();
        let (name_for_ok, error_for_ok, existing) = (name.clone(), error.clone(), existing.clone());
        dialog
            .title(if renaming { "Rename group" } else { "New group" })
            .w(crate::zoom::scaled(420., cx))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child("Name"))
                    .child(Input::new(&name))
                    .children(shown_error.map(|error| div().text_sm().text_color(cx.theme().danger).child(error))),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.outline().label("Cancel")))
                    .child(
                        DialogAction::new().child(Button::new("group-save").primary().label(if renaming {
                            "Rename"
                        } else {
                            "Create"
                        })),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let value = name_for_ok.read(cx).value().to_string();
                let result = PrefsHub::update_layout(cx, |layout| match &existing {
                    Some(group) => layout.rename_group(&group.id, &value),
                    None => layout.create_group(&value).map(|_| ()),
                });
                match result {
                    Ok(()) => true,
                    Err(err) => {
                        *error_for_ok.borrow_mut() = Some(err.to_string().into());
                        window.refresh();
                        false
                    }
                }
            })
    });
    crate::settings_view::focus_and_select(&name_field, window, cx);
}

fn confirm_delete(group: Group, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |alert, _, _| {
        let id = group.id.clone();
        alert
            .title(format!("Delete “{}”?", group.name))
            .description("Its accounts move to Ungrouped. No account is removed.")
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Delete")
                    .ok_variant(ButtonVariant::Danger)
                    .show_cancel(true),
            )
            .on_ok(move |_, _, cx| {
                let _ = PrefsHub::update_layout(cx, |layout| layout.delete_group(&id));
                true
            })
    });
}
