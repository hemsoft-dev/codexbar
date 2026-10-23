//! Explicit monthly prices and known scheduled billing changes, separate from account identity.
use crate::settings_hub::SettingsHub;
use chrono::{DateTime, Utc};
use codexbar_core::sus::Price;
use codexbar_store::subscription_price::{display_usd, parse_usd};
use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogClose, DialogFooter},
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::{
    App, AppContext as _, Entity, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use std::{cell::RefCell, rc::Rc};
fn save(id: &str, price: Option<Price>, cx: &mut App) -> Result<(), String> {
    let mut record = SettingsHub::global(cx)
        .settings()
        .accounts()
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .ok_or("The account was removed.")?;
    record.sus_price = price;
    SettingsHub::update(cx, |s| s.upsert(record)).map_err(|e| e.to_string())
}
pub fn open(id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    if hub.is_read_only() {
        return;
    }
    let Some(record) = hub.settings().accounts().iter().find(|r| r.id == id).cloned() else {
        return;
    };
    let price = record.sus_price.clone();
    let field = |value: String, hint: &'static str, window: &mut Window, cx: &mut App| -> Entity<InputState> {
        cx.new(|cx| InputState::new(window, cx).placeholder(hint).default_value(value))
    };
    let current = field(
        display_usd(price.as_ref().and_then(|p| p.monthly_cents)),
        "Current monthly USD",
        window,
        cx,
    );
    let next = field(
        display_usd(price.as_ref().and_then(|p| p.scheduled.map(|p| p.0))),
        "Next monthly USD",
        window,
        cx,
    );
    let at = field(
        price
            .as_ref()
            .and_then(|p| p.scheduled.map(|p| p.1.to_rfc3339()))
            .unwrap_or_default(),
        "Effective instant with timezone",
        window,
        cx,
    );
    let error: Rc<RefCell<Option<SharedString>>> = Rc::default();
    window.open_dialog(cx, move |dialog, _, cx| {
        let current = current.clone();
        let next = next.clone();
        let at = at.clone();
        let error_save = error.clone();
        let id = record.id.clone();
        let provider_id = id.clone();
        dialog.title(format!("SUS price: {}", record.label))
            .w(crate::zoom::scaled(520., cx))
            .child(v_flex().gap_3()
                .child(div().text_sm().child("Monthly USD prices weight fleet SUS. A blank current price means unknown and uses equal weights. Set a future price only with a known effective instant including its timezone."))
                .child(div().child("Current monthly USD"))
                .child(Input::new(&current).id("sus-current-price"))
                .child(div().child("Next monthly USD"))
                .child(Input::new(&next).id("sus-next-price"))
                .child(div().child("Effective instant, RFC3339 with timezone"))
                .child(Input::new(&at).id("sus-price-at"))
                .when_some(error.borrow().clone(), |this, error| this.child(div().text_color(cx.theme().danger).child(error))))
            .footer(DialogFooter::new()
                .child(Button::new("sus-price-provider").outline().label("Use provider price")
                    .on_click(move |_, window, cx| match save(&provider_id, None, cx) {
                        Ok(()) => window.close_dialog(cx),
                        Err(e) => SettingsHub::set_error(cx, Some(e.into())),
                    }))
                .child(DialogClose::new().trigger(|b| b.outline().label("Cancel")))
                .child(Button::new("sus-price-save").primary().label("Save price")
                    .on_click(move |_, window, cx| {
                        let result = parse(current.read(cx).value().as_str(), next.read(cx).value().as_str(), at.read(cx).value().as_str());
                        match result.map_err(str::to_owned).and_then(|price| save(&id, Some(price), cx)) {
                            Ok(()) => window.close_dialog(cx),
                            Err(e) => {
                                *error_save.borrow_mut() = Some(e.into());
                                window.refresh();
                            }
                        }
                    })))
    });
}
fn parse(current: &str, next: &str, at: &str) -> Result<Price, &'static str> {
    let monthly_cents = parse_usd(current)?;
    let target = parse_usd(next)?;
    let scheduled = match (target, at.trim().is_empty()) {
        (None, true) => None,
        (Some(cents), false) => Some((
            cents,
            DateTime::parse_from_rfc3339(at.trim())
                .map_err(|_| "Enter a valid effective instant with its timezone.")?
                .with_timezone(&Utc),
        )),
        _ => return Err("A scheduled price needs both a price and an effective instant."),
    };
    Ok(Price {
        monthly_cents,
        scheduled,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedules_require_a_known_instant_and_unknown_stays_unknown() {
        assert_eq!(parse("", "", "").unwrap().at(Utc::now()), None);
        assert!(parse("200", "100", "").is_err());
        assert!(parse("200", "", "2026-10-26T09:33:19-04:00").is_err());
        assert!(parse("200", "100", "2026-10-26").is_err());
        let price = parse("200", "100", "2026-10-26T09:33:19-04:00").unwrap();
        assert_eq!(price.monthly_cents, Some(20_000));
        assert_eq!(price.scheduled.unwrap().0, 10_000);
    }
}
