//! The Windows widget provider (#94): the COM server the Widgets board starts.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::bindings::Microsoft::Windows::Widgets::Providers::{
    IWidgetProvider, IWidgetProvider_Impl, IWidgetProvider2, IWidgetProvider2_Impl, WidgetActionInvokedArgs,
    WidgetContext, WidgetContextChangedArgs, WidgetCustomizationRequestedArgs, WidgetManager,
    WidgetUpdateRequestOptions,
};
use super::bindings::Microsoft::Windows::Widgets::WidgetSize;
use chrono::Utc;
use codexbar_store::widgets::{LoadError, WidgetRefresh, WidgetSnapshot, load_widget_snapshot};
use windows::Win32::Foundation::CLASS_E_NOAGGREGATION;
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoAddRefServerProcess, CoInitializeEx, CoRegisterClassObject,
    CoReleaseServerProcess, CoResumeClassObjects, CoRevokeClassObject, IClassFactory, IClassFactory_Impl, REGCLS,
    REGCLS_MULTIPLEUSE, REGCLS_SUSPENDED,
};
use windows::core::{BOOL, GUID, HSTRING, IUnknown, Interface, Ref, implement};

use super::cards;
use super::host::{self, Host, Outcome, Update};

/// The provider's COM class, `com:Class` in the manifest.
pub const CLSID: GUID = GUID::from_u128(0x6a9b22c1_0ca4_41f3_911a_fd4724d8e0b2);

/// The argument Windows starts the COM server with (`com:ExeServer Arguments` in the manifest).
pub const SERVER_ARG: &str = "-RegisterProcessAsComServer";

/// How often the snapshot file is checked while a widget is on screen. Cards also redraw once a minute for their
/// "Updated 3 min ago" text.
const POLL: Duration = Duration::from_secs(10);
const REDRAW: Duration = Duration::from_secs(60);
/// Started but never asked for a provider in this long, the process exits.
const UNUSED_EXIT: Duration = Duration::from_secs(60);

/// Whether this PC can show CodexBar's widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardSupport {
    /// Windows 10 has no Widgets board; Windows 11 (build 22000) is the only widget host.
    NeedsWindows11,
    /// The board lists widgets from a self-signed (sideloaded) package only with Developer Mode on.
    NeedsDeveloperMode,
    Ready,
}

impl BoardSupport {
    pub fn of(build: Option<u32>, developer_mode: bool) -> Self {
        match build {
            Some(build) if build < 22000 => Self::NeedsWindows11,
            _ if !developer_mode => Self::NeedsDeveloperMode,
            _ => Self::Ready,
        }
    }

    /// This PC's support, from the registry.
    pub fn current() -> Self {
        let build = read_string(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber")
            .and_then(|build| build.trim().parse().ok());
        let developer_mode = read_dword(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock",
            "AllowDevelopmentWithoutDevLicense",
        ) == Some(1);
        Self::of(build, developer_mode)
    }
}

fn read_value(key: &str, name: &str, flags: windows::Win32::System::Registry::REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RegGetValueW};
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    let mut size = 0u32;
    // SAFETY: valid null-terminated strings; the first call only asks for the size.
    unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &key, &name, flags, None, None, Some(&mut size)) }
        .ok()
        .ok()?;
    let mut data = vec![0u8; size as usize];
    // SAFETY: `data` holds `size` bytes, as Windows asked for.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &key,
            &name,
            flags,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    data.truncate(size as usize);
    Some(data)
}

fn read_string(key: &str, name: &str) -> Option<String> {
    let data = read_value(key, name, windows::Win32::System::Registry::RRF_RT_REG_SZ)?;
    let wide: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Some(String::from_utf16_lossy(&wide).trim_end_matches('\0').to_owned())
}

fn read_dword(key: &str, name: &str) -> Option<u32> {
    let data = read_value(key, name, windows::Win32::System::Registry::RRF_RT_REG_DWORD)?;
    Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?))
}

fn size(size: WidgetSize) -> cards::Size {
    match size {
        WidgetSize::Small => cards::Size::Small,
        WidgetSize::Large => cards::Size::Large,
        _ => cards::Size::Medium,
    }
}

/// Everything the provider's COM calls and the poll loop share.
struct Shared {
    dir: PathBuf,
    host: Mutex<Host>,
    /// Wakes the poll loop early (a widget was pinned or shown).
    wake: Condvar,
    last_active: Mutex<Instant>,
    /// A client was handed a provider object or locked the server.
    connected: AtomicBool,
    /// Every provider object and server lock was released: COM has stopped handing out new ones (the class is
    /// suspended), so the process ends and Windows starts a new one when a client asks again.
    released: AtomicBool,
}

impl Shared {
    fn snapshot(&self) -> Result<WidgetSnapshot, LoadError> {
        load_widget_snapshot(&self.dir)
    }

    fn host(&self) -> std::sync::MutexGuard<'_, Host> {
        self.host.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One more provider object or server lock held by a client.
    fn add_ref(&self) {
        self.connected.store(true, Ordering::SeqCst);
        // SAFETY: balanced by `release`.
        unsafe { CoAddRefServerProcess() };
    }

    fn release(&self) {
        // SAFETY: pairs with an earlier `add_ref`. At zero COM suspends the class objects.
        if unsafe { CoReleaseServerProcess() } == 0 {
            self.released.store(true, Ordering::SeqCst);
            self.wake.notify_all();
        }
    }

    fn touch(&self) {
        *self.last_active.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
        self.wake.notify_all();
    }
}

/// Sends a card to the widget host. A failure is dropped: the next poll sends a newer card anyway.
fn send(update: Update) {
    let result = (|| -> windows::core::Result<()> {
        let options = WidgetUpdateRequestOptions::CreateInstance(&HSTRING::from(&update.id))?;
        options.SetTemplate(&HSTRING::from(update.card.to_string()))?;
        options.SetData(&HSTRING::from("{}"))?;
        options.SetCustomState(&HSTRING::from(&update.custom_state))?;
        WidgetManager::GetDefault()?.UpdateWidget(&options)
    })();
    if let Err(err) = result {
        eprintln!("codexbar: widget update failed: {}", err.message());
    }
}

/// Starts CodexBar's window, as a separate process so it outlives the provider. A running CodexBar takes the request
/// over (`crate::handoff`) and shows the tapped account and metric.
fn open_codexbar(target: Option<&host::Target>) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(exe);
    if let Some(target) = target {
        command.arg(crate::handoff::FOCUS_ARG).arg(&target.account);
        if let Some(metric) = &target.metric {
            command.arg(crate::handoff::METRIC_ARG).arg(metric);
        }
    }
    let _ = codexbar_providers::platform::spawn(&mut command);
}

/// One provider object per client request; the server lives as long as any is held (out-of-process COM's rule).
#[implement(IWidgetProvider, IWidgetProvider2)]
struct Provider(Arc<Shared>);

impl Provider {
    fn new(shared: Arc<Shared>) -> Self {
        shared.add_ref();
        Self(shared)
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl IWidgetProvider_Impl for Provider_Impl {
    fn CreateWidget(&self, context: Ref<WidgetContext>) -> windows::core::Result<()> {
        let context = context.ok()?;
        let (id, definition, widget_size) = (context.Id()?, context.DefinitionId()?, context.Size()?);
        let update = self.0.host().create(
            &id.to_string(),
            &definition.to_string(),
            size(widget_size),
            &self.0.snapshot(),
            Utc::now(),
        );
        self.0.touch();
        update.into_iter().for_each(send);
        Ok(())
    }

    fn DeleteWidget(&self, id: &HSTRING, _custom_state: &HSTRING) -> windows::core::Result<()> {
        self.0.host().delete(&id.to_string());
        self.0.touch();
        Ok(())
    }

    fn OnActionInvoked(&self, args: Ref<WidgetActionInvokedArgs>) -> windows::core::Result<()> {
        let args = args.ok()?;
        let id = args.WidgetContext()?.Id()?.to_string();
        let (verb, data) = (args.Verb()?.to_string(), args.Data()?.to_string());
        let outcome = self.0.host().action(&id, &verb, &data, &self.0.snapshot(), Utc::now());
        match outcome {
            Outcome::Nothing => {}
            Outcome::Update(update) => send(update),
            Outcome::Open(target) => open_codexbar(target.as_ref()),
        }
        Ok(())
    }

    fn OnWidgetContextChanged(&self, args: Ref<WidgetContextChangedArgs>) -> windows::core::Result<()> {
        let context = args.ok()?.WidgetContext()?;
        let update = self.0.host().resize(
            &context.Id()?.to_string(),
            size(context.Size()?),
            &self.0.snapshot(),
            Utc::now(),
        );
        update.into_iter().for_each(send);
        Ok(())
    }

    fn Activate(&self, context: Ref<WidgetContext>) -> windows::core::Result<()> {
        let context = context.ok()?;
        let update = self.0.host().activate(
            &context.Id()?.to_string(),
            size(context.Size()?),
            &self.0.snapshot(),
            Utc::now(),
        );
        self.0.touch();
        update.into_iter().for_each(send);
        Ok(())
    }

    fn Deactivate(&self, id: &HSTRING) -> windows::core::Result<()> {
        self.0.host().deactivate(&id.to_string());
        self.0.touch();
        Ok(())
    }
}

impl IWidgetProvider2_Impl for Provider_Impl {
    fn OnCustomizationRequested(&self, args: Ref<WidgetCustomizationRequestedArgs>) -> windows::core::Result<()> {
        let id = args.ok()?.WidgetContext()?.Id()?.to_string();
        let update = self.0.host().customize(&id, &self.0.snapshot(), Utc::now());
        update.into_iter().for_each(send);
        Ok(())
    }
}

/// Hands out provider objects, all sharing the one widget state.
#[implement(IClassFactory)]
struct Factory(Arc<Shared>);

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let provider: IWidgetProvider = Provider::new(self.0.clone()).into();
        // SAFETY: COM passes a valid interface id and out-pointer.
        unsafe { provider.query(iid, object).ok() }
    }

    fn LockServer(&self, lock: BOOL) -> windows::core::Result<()> {
        if lock.as_bool() {
            self.0.add_ref();
        } else {
            self.0.release();
        }
        Ok(())
    }
}

/// The widgets the host already has, after the provider was restarted, with their saved settings.
fn restore(shared: &Shared) -> windows::core::Result<()> {
    for info in WidgetManager::GetDefault()?.GetWidgetInfos()?.iter().flatten() {
        let context = info.WidgetContext()?;
        shared.host().restore(
            &context.Id()?.to_string(),
            &context.DefinitionId()?.to_string(),
            size(context.Size()?),
            &info.CustomState()?.to_string(),
            context.IsActive()?,
        );
    }
    Ok(())
}

/// Whether visible widgets redraw now, under the builder's refresh choice (#95): with CodexBar, as soon as the
/// snapshot changes and once a minute for the ages; otherwise only every five or fifteen minutes.
fn redraw_due(refresh: WidgetRefresh, changed: bool, since_drawn: Duration) -> bool {
    match refresh {
        WidgetRefresh::WithCodexBar => changed || since_drawn >= REDRAW,
        other => since_drawn >= other.interval(),
    }
}

/// Runs the COM server until its clients have released every provider object and lock.
pub fn serve() -> windows::core::Result<()> {
    // SAFETY: the process's main thread joins the multithreaded apartment once, before any COM use.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
    let shared = Arc::new(Shared {
        dir: codexbar_store::settings::Settings::default_dir(),
        host: Mutex::new(Host::default()),
        wake: Condvar::new(),
        last_active: Mutex::new(Instant::now()),
        connected: AtomicBool::new(false),
        released: AtomicBool::new(false),
    });
    if let Err(err) = restore(&shared) {
        eprintln!("codexbar: couldn't list pinned widgets: {}", err.message());
    }
    let factory: IClassFactory = Factory(shared.clone()).into();
    // Registered suspended and then resumed, so the server-process count (`add_ref`/`release`) can suspend the class
    // when it reaches zero. SAFETY: a valid class id and factory; the registration is revoked before the factory is
    // dropped.
    let cookie = unsafe {
        CoRegisterClassObject(
            &CLSID,
            &factory,
            CLSCTX_LOCAL_SERVER,
            REGCLS(REGCLS_MULTIPLEUSE.0 | REGCLS_SUSPENDED.0),
        )
    }?;
    // SAFETY: resumes the class objects this process registered.
    unsafe { CoResumeClassObjects() }?;
    let started = Instant::now();

    let modified = || {
        std::fs::metadata(shared.dir.join(codexbar_store::widgets::WIDGETS_FILE))
            .and_then(|m| m.modified())
            .ok()
    };
    let mut seen: Option<SystemTime> = modified();
    let mut drawn = Instant::now();
    loop {
        {
            let host = shared.host();
            let _ = shared.wake.wait_timeout(host, POLL);
        }
        let changed = modified();
        let active = shared.host().any_active();
        if active {
            *shared
                .last_active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
            let snapshot = shared.snapshot();
            let refresh = snapshot.as_ref().map(|s| s.builder.refresh).unwrap_or_default();
            if redraw_due(refresh, changed != seen, drawn.elapsed()) {
                let updates = shared.host().refresh(&snapshot, Utc::now());
                updates.into_iter().for_each(send);
                drawn = Instant::now();
            }
        }
        seen = changed;
        if shared.released.load(Ordering::SeqCst)
            || (!shared.connected.load(Ordering::SeqCst) && started.elapsed() >= UNUSED_EXIT)
        {
            break;
        }
    }
    // SAFETY: the cookie came from CoRegisterClassObject above.
    unsafe { CoRevokeClassObject(cookie) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_declares_the_class_and_definition() {
        let manifest = include_str!("../../../../packaging/AppxManifest.xml").to_lowercase();
        let clsid = format!("{CLSID:?}").to_lowercase();
        assert_eq!(
            manifest.matches(&clsid).count(),
            2,
            "com:Class and CreateInstance ({clsid})"
        );
        assert!(manifest.contains(&format!(r#"arguments="{}""#, SERVER_ARG.to_lowercase())));
        assert!(manifest.contains(&format!(r#"<definition id="{}""#, host::DEFINITION.to_lowercase())));
    }

    #[test]
    fn the_refresh_choice_paces_redraws() {
        let minute = Duration::from_secs(60);
        assert!(
            redraw_due(WidgetRefresh::WithCodexBar, true, Duration::ZERO),
            "new usage shows at once"
        );
        assert!(!redraw_due(WidgetRefresh::WithCodexBar, false, Duration::from_secs(30)));
        assert!(
            redraw_due(WidgetRefresh::WithCodexBar, false, minute),
            "ages move on each minute"
        );
        assert!(
            !redraw_due(WidgetRefresh::FiveMinutes, true, minute),
            "new usage waits for the interval"
        );
        assert!(redraw_due(WidgetRefresh::FiveMinutes, false, 5 * minute));
        assert!(!redraw_due(WidgetRefresh::FifteenMinutes, true, 10 * minute));
        assert!(redraw_due(WidgetRefresh::FifteenMinutes, false, 15 * minute));
    }

    #[test]
    fn widgets_need_windows_11_and_developer_mode() {
        assert_eq!(BoardSupport::of(Some(19045), true), BoardSupport::NeedsWindows11);
        assert_eq!(BoardSupport::of(Some(26100), false), BoardSupport::NeedsDeveloperMode);
        assert_eq!(BoardSupport::of(Some(22000), true), BoardSupport::Ready);
        assert_eq!(
            BoardSupport::of(None, true),
            BoardSupport::Ready,
            "an unreadable build isn't held against it"
        );
    }

    #[test]
    fn the_registry_reads_this_windows_build() {
        let build = read_string(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber");
        assert!(build.is_some_and(|build| build.parse::<u32>().is_ok_and(|build| build >= 10240)));
        assert_eq!(
            read_dword(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "NoSuchValue"),
            None
        );
    }

    #[test]
    fn widget_sizes_map_to_card_sizes() {
        assert_eq!(size(WidgetSize::Small), cards::Size::Small);
        assert_eq!(size(WidgetSize::Medium), cards::Size::Medium);
        assert_eq!(size(WidgetSize::Large), cards::Size::Large);
    }
}
