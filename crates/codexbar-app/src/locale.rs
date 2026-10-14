//! Dates and times in the user's own locale (#84): 12- or 24-hour time, local month and weekday names, and the
//! locale's own month-day order. Windows reads the user's current settings on every call, so a change in
//! Settings > Time & language shows on the next redraw. macOS and Linux read the locale once at start (#167). Tests use the fixed English style, so their output doesn't
//! depend on the machine running them.

#[cfg(windows)]
use chrono::{Datelike as _, Timelike as _};
use chrono::{NaiveDate, NaiveDateTime};
use codexbar_core::format::{DateStyle, English};

/// The style the app shows dates in.
pub fn style() -> &'static dyn DateStyle {
    if cfg!(test) {
        return &English;
    }
    #[cfg(windows)]
    return &WindowsLocale;
    #[cfg(not(windows))]
    {
        static SYSTEM: std::sync::LazyLock<LocaleStyle> = std::sync::LazyLock::new(LocaleStyle::system);
        &*SYSTEM
    }
}

/// A provider error's message with any time in the user's locale and timezone.
pub fn describe(error: &codexbar_providers::ProviderError) -> String {
    error.describe_with(&|at| style().time(at.with_timezone(&chrono::Local).naive_local()))
}

/// The Windows user locale, through `GetDateFormatEx` and `GetTimeFormatEx`; English if a call fails.
#[cfg(windows)]
pub struct WindowsLocale;

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::Globalization::{
        DATE_LONGDATE, DATE_MONTHDAY, ENUM_DATE_FORMATS_FLAGS, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS,
    };
    use windows::core::{HSTRING, PCWSTR};

    /// Runs a Windows formatting call twice: first to learn the length it needs, then into a buffer of that size.
    fn sized(call: impl Fn(Option<&mut [u16]>) -> i32) -> Option<String> {
        let needed = call(None);
        if needed <= 1 {
            return None;
        }
        let mut buffer = vec![0u16; needed as usize];
        let written = call(Some(&mut buffer));
        // `written` counts the terminating NUL.
        (written > 1).then(|| String::from_utf16_lossy(&buffer[..written as usize - 1]))
    }

    /// A date with a format picture ("ddd") or, without one, a format flag (`DATE_MONTHDAY`).
    pub fn date(when: &SYSTEMTIME, flags: ENUM_DATE_FORMATS_FLAGS, picture: Option<&str>) -> Option<String> {
        let picture = picture.map(HSTRING::from);
        let format = picture.as_ref().map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr()));
        sized(|buffer| {
            // SAFETY: `when`, `format` and `buffer` outlive the call; a null locale name is the user default locale.
            unsafe { GetDateFormatEx(PCWSTR::null(), flags, Some(when), format, buffer, PCWSTR::null()) }
        })
    }

    pub fn time(when: &SYSTEMTIME) -> Option<String> {
        sized(|buffer| {
            // SAFETY: as above; a null format uses the locale's own time format.
            unsafe { GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(when), PCWSTR::null(), buffer) }
        })
    }

    pub use windows::Win32::Globalization::ENUM_DATE_FORMATS_FLAGS as Flags;
    pub const NONE: Flags = ENUM_DATE_FORMATS_FLAGS(0);
    pub const MONTH_DAY: Flags = DATE_MONTHDAY;
    pub const LONG_DATE: Flags = DATE_LONGDATE;
}

#[cfg(windows)]
fn system_time(at: NaiveDateTime) -> windows::Win32::Foundation::SYSTEMTIME {
    windows::Win32::Foundation::SYSTEMTIME {
        wYear: at.year() as u16,
        wMonth: at.month() as u16,
        wDayOfWeek: at.weekday().num_days_from_sunday() as u16,
        wDay: at.day() as u16,
        wHour: at.hour() as u16,
        wMinute: at.minute() as u16,
        wSecond: 0,
        wMilliseconds: 0,
    }
}

#[cfg(windows)]
impl DateStyle for WindowsLocale {
    fn time(&self, at: NaiveDateTime) -> String {
        win::time(&system_time(at)).unwrap_or_else(|| English.time(at))
    }

    fn weekday(&self, at: NaiveDate) -> String {
        let at = at.and_hms_opt(12, 0, 0).unwrap_or_default();
        win::date(&system_time(at), win::NONE, Some("ddd")).unwrap_or_else(|| English.weekday(at.date()))
    }

    fn month_day(&self, at: NaiveDate) -> String {
        let at = at.and_hms_opt(12, 0, 0).unwrap_or_default();
        win::date(&system_time(at), win::MONTH_DAY, None).unwrap_or_else(|| English.month_day(at.date()))
    }

    // The composites use whole Windows date patterns, so the locale decides the order and punctuation; date before
    // time, as in .NET's general and full date-time patterns.
    fn month_day_time(&self, at: NaiveDateTime) -> String {
        match (
            win::date(&system_time(at), win::MONTH_DAY, None),
            win::time(&system_time(at)),
        ) {
            (Some(date), Some(time)) => format!("{date} {time}"),
            _ => English.month_day_time(at),
        }
    }

    fn full(&self, at: NaiveDateTime) -> String {
        match (
            win::date(&system_time(at), win::LONG_DATE, None),
            win::time(&system_time(at)),
        ) {
            (Some(date), Some(time)) => format!("{date} {time}"),
            _ => English.full(at),
        }
    }
}

/// Dates in a POSIX locale's own conventions, from the locale data glibc ships (#167): its time format without seconds,
/// its weekday and month names, and day-first or month-first order.
#[cfg(not(windows))]
pub struct LocaleStyle(pub chrono::Locale);

#[cfg(not(windows))]
impl LocaleStyle {
    /// The user's locale for dates: `LC_ALL`, `LC_TIME` or `LANG` when set (Linux, and anything started from a shell),
    /// otherwise the system's preferred language (a macOS app started from Finder has no `LANG`). US English when
    /// none can be read.
    pub fn system() -> Self {
        let from_env = ["LC_ALL", "LC_TIME", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty());
        let locale = from_env
            .or_else(sys_locale::get_locale)
            .and_then(|name| parse_locale(&name))
            .unwrap_or(chrono::Locale::en_US);
        Self(locale)
    }

    /// The locale's time format with minutes as its smallest unit, and no leading zero on a 12-hour clock.
    fn time_format(&self) -> String {
        let format = pure_rust_locales::locale_match!(self.0 => LC_TIME::T_FMT);
        let format = if format.is_empty() { "%H:%M:%S" } else { format };
        format
            .replace("%T", "%H:%M:%S")
            .replace("%r", "%I:%M:%S %p")
            .replace(":%S", "")
            .replace(".%S", "")
            .replace("%I", "%-I")
    }

    /// Whether the locale's date writes the day before the month ("1.11." rather than "11/1").
    fn day_first(&self) -> bool {
        let format = pure_rust_locales::locale_match!(self.0 => LC_TIME::D_FMT).replace("%D", "%m/%d/%y");
        match (format.find("%d").or_else(|| format.find("%e")), format.find("%m")) {
            (Some(day), Some(month)) => day < month,
            _ => false,
        }
    }
}

/// A locale name as the environment or the system gives it ("de_DE.UTF-8", "de-DE", "pt_BR@euro", "fr"), as a locale
/// the formats are known for. "C" and "POSIX" are no language and give none.
#[cfg(not(windows))]
pub fn parse_locale(name: &str) -> Option<chrono::Locale> {
    let base = name.split(['.', '@']).next()?.replace('-', "_");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    if let Ok(locale) = chrono::Locale::try_from(base.as_str()) {
        return Some(locale);
    }
    // A bare language ("fr", "de"): its main country, the way "fr" means French as written in France.
    let language = base.split('_').next()?;
    chrono::Locale::try_from(format!("{language}_{}", language.to_uppercase()).as_str()).ok()
}

#[cfg(not(windows))]
impl DateStyle for LocaleStyle {
    fn time(&self, at: NaiveDateTime) -> String {
        at.and_utc().format_localized(&self.time_format(), self.0).to_string()
    }

    fn weekday(&self, at: NaiveDate) -> String {
        at.format_localized("%a", self.0).to_string()
    }

    fn month_day(&self, at: NaiveDate) -> String {
        let format = if self.day_first() { "%-d %b" } else { "%b %-d" };
        at.format_localized(format, self.0).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thursday_nine() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 8)
            .unwrap()
            .and_hms_opt(9, 5, 0)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_locale_formats_every_part() {
        // Whatever this machine's locale, every part comes back non-empty and carries the right numbers.
        let at = thursday_nine();
        let time = WindowsLocale.time(at);
        assert!(time.contains('9') && time.contains("05"), "{time}");
        assert!(!WindowsLocale.weekday(at.date()).is_empty());
        let month_day = WindowsLocale.month_day(at.date());
        assert!(month_day.contains('8'), "{month_day}");
        assert!(WindowsLocale.month_day_time(at).contains(&month_day));
        let full = WindowsLocale.full(at);
        assert!(full.contains("2026") && full.ends_with(&time), "{full}");
    }

    #[cfg(not(windows))]
    #[test]
    fn posix_locales_set_the_clock_names_and_order() {
        let at = thursday_nine();
        let style = |name: &str| LocaleStyle(parse_locale(name).expect(name));
        let us = style("en_US.UTF-8");
        assert_eq!(us.time(at), "9:05 AM");
        assert_eq!(us.weekday(at.date()), "Thu");
        assert_eq!(us.month_day(at.date()), "Oct 8");
        let german = style("de-DE");
        assert_eq!(german.time(at), "09:05");
        assert_eq!(german.weekday(at.date()), "Do");
        assert_eq!(german.month_day(at.date()), "8 Okt");
        let british = style("en_GB");
        assert_eq!(british.time(at), "09:05");
        assert_eq!(british.month_day(at.date()), "8 Oct");
        assert_eq!(style("fr").weekday(at.date()), "jeu.");
        assert_eq!(parse_locale("C"), None);
        assert_eq!(parse_locale("POSIX"), None);
        assert_eq!(parse_locale("xx_YY"), None);
    }

    #[test]
    fn rate_limit_messages_take_the_time_from_the_style() {
        let at = chrono::Utc::now();
        let error = codexbar_providers::ProviderError::RateLimited { retry_at: at };
        let shown = error.describe_with(&|_| "9:05 AM".to_owned());
        assert_eq!(shown, "Rate-limited by the provider; retrying at 9:05 AM.");
        assert!(describe(&error).starts_with("Rate-limited by the provider; retrying at "));
        // Other errors read as before.
        let network = codexbar_providers::ProviderError::Network;
        assert_eq!(describe(&network), network.to_string());
    }

    #[test]
    fn tests_use_the_fixed_english_style() {
        assert_eq!(style().weekday_time(thursday_nine()), "Thu 09:05");
        assert_eq!(style().month_day(thursday_nine().date()), "Oct 8");
    }
}
