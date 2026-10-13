//! The MSIX package (#93): whether CodexBar runs from its package, which version, and Windows' own update check for a
//! package installed through an App Installer file (`package.ps1` writes one into the local channel folder). The
//! build from source (`run.ps1`) has no package; everything here reports that instead of failing.

use std::sync::OnceLock;

use gpui_kit::{App, Global};

/// The `Application Id` in `packaging/AppxManifest.xml`; with the package family name it forms the app's
/// AppUserModelID.
pub const APPLICATION_ID: &str = "CodexBar";

/// The build `package.ps1` stamps into a packaged executable (its commit), if any.
pub const BUILD: Option<&str> = option_env!("CODEXBAR_BUILD");

/// The package CodexBar runs from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// `major.minor.build.revision`.
    pub version: String,
    pub family: String,
    /// The App Installer file Windows checks for updates, when the package was installed from one.
    pub app_installer: Option<String>,
}

impl Installed {
    /// The AppUserModelID Windows gives the packaged app.
    pub fn app_user_model_id(&self) -> String {
        format!("{}!{APPLICATION_ID}", self.family)
    }
}

/// Keeps the Windows Runtime's multithreaded apartment alive for the rest of the process, so the calls here work on any
/// thread, including a background worker that never joined an apartment (it is an implicit member).
///
/// The apartment must never end while the process runs: the `windows` crate caches activation factories for the whole
/// process, and once the last thread left the apartment (by uninitializing, or by exiting) a cached factory pointed
/// into the torn-down apartment, so the next call crashed the process with an access violation (#157).
struct Apartment;

impl Apartment {
    fn enter() -> Self {
        static KEPT: std::sync::Once = std::sync::Once::new();
        KEPT.call_once(|| {
            // SAFETY: the usage cookie is deliberately never passed to `CoDecrementMTAUsage`, so the apartment lasts
            // until the process exits. Should it fail, calls here fail as Windows errors, as before.
            let _ = unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() };
        });
        Self
    }
}

/// The package of this process, read once. `None` for the unpackaged build.
pub fn installed() -> Option<&'static Installed> {
    static INSTALLED: OnceLock<Option<Installed>> = OnceLock::new();
    INSTALLED.get_or_init(read_installed).as_ref()
}

fn read_installed() -> Option<Installed> {
    use windows::ApplicationModel::Package;
    // Unpackaged processes get APPMODEL_ERROR_NO_PACKAGE here.
    let package = Package::Current().ok()?;
    let id = package.Id().ok()?;
    let version = id.Version().ok()?;
    let family = id.FamilyName().ok()?.to_string();
    let app_installer = package
        .GetAppInstallerInfo()
        .ok()
        .and_then(|info| info.Uri().ok())
        .and_then(|uri| uri.DisplayUri().ok())
        .map(|uri| uri.to_string())
        .filter(|uri| !uri.is_empty());
    Some(Installed {
        version: format!(
            "{}.{}.{}.{}",
            version.Major, version.Minor, version.Build, version.Revision
        ),
        family,
        app_installer,
    })
}

/// Where the update check stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateState {
    /// Built from source: update with `git pull` and `run.ps1`.
    NotPackaged,
    /// Installed from an `.msix` directly, so Windows has no channel to check.
    NoChannel,
    NotChecked,
    Checking,
    UpToDate,
    /// A newer version is in the channel; `required` when the App Installer file marks it mandatory.
    Available {
        required: bool,
    },
    Installing,
    /// The check or the install failed; the message is Windows' own.
    Failed(String),
}

impl UpdateState {
    /// The state before the first check.
    pub fn initial(installed: Option<&Installed>) -> Self {
        match installed {
            None => Self::NotPackaged,
            Some(installed) if installed.app_installer.is_none() => Self::NoChannel,
            Some(_) => Self::NotChecked,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::NotPackaged => "Built from source. Update with git pull and run.ps1.".into(),
            Self::NoChannel => "Installed from a package file, so there is no update channel to check.".into(),
            Self::NotChecked => "Not checked yet.".into(),
            Self::Checking => "Checking for updates…".into(),
            Self::UpToDate => "CodexBar is up to date.".into(),
            Self::Available { required: false } => "A new version is ready to install.".into(),
            Self::Available { required: true } => "A required update is ready to install.".into(),
            Self::Installing => "Installing the update; CodexBar restarts when it's done.".into(),
            Self::Failed(message) => format!("Couldn't update: {message}"),
        }
    }

    pub fn can_check(&self) -> bool {
        matches!(
            self,
            Self::NotChecked | Self::UpToDate | Self::Available { .. } | Self::Failed(_)
        )
    }

    pub fn can_install(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// How long one update check may take; Windows has been seen never to answer one.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Waits for `operation` up to `timeout`, then cancels it. `None` means it didn't finish in time.
fn finish<T>(
    operation: &windows_future::IAsyncOperation<T>,
    timeout: std::time::Duration,
) -> windows::core::Result<Option<T>>
where
    T: windows::core::RuntimeType + 'static,
{
    let started = std::time::Instant::now();
    while operation.Status()? == windows_future::AsyncStatus::Started {
        if started.elapsed() >= timeout {
            let _ = operation.Cancel();
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    operation.GetResults().map(Some)
}

/// Asks Windows whether the App Installer channel has a newer version, once more after a short pause if Windows
/// reports an error (seen right after an install). Blocking: run it off the UI thread.
pub fn check() -> UpdateState {
    let _apartment = Apartment::enter();
    match check_once() {
        UpdateState::Failed(_) if installed().is_some() => {
            std::thread::sleep(std::time::Duration::from_secs(3));
            check_once()
        }
        state => state,
    }
}

fn check_once() -> UpdateState {
    use windows::ApplicationModel::{Package, PackageUpdateAvailability};
    use windows::Management::Deployment::PackageManager;
    let result = (|| -> windows::core::Result<UpdateState> {
        // Windows refuses the check on `Package::Current()` (access denied); the same package found through the
        // package manager may ask (an empty user security id means the current user).
        let full_name = Package::Current()?.Id()?.FullName()?;
        let package = PackageManager::new()?
            .FindPackageByUserSecurityIdPackageFullName(&windows::core::HSTRING::new(), &full_name)?;
        let Some(result) = finish(&package.CheckUpdateAvailabilityAsync()?, CHECK_TIMEOUT)? else {
            return Ok(UpdateState::Failed("Windows didn't answer within a minute.".into()));
        };
        Ok(match result.Availability()? {
            PackageUpdateAvailability::Available => UpdateState::Available { required: false },
            PackageUpdateAvailability::Required => UpdateState::Available { required: true },
            PackageUpdateAvailability::NoUpdates => UpdateState::UpToDate,
            PackageUpdateAvailability::Error => {
                UpdateState::Failed(windows::core::Error::from_hresult(result.ExtendedError()?).message())
            }
            _ => UpdateState::Failed("Windows couldn't tell whether an update is available.".into()),
        })
    })();
    result.unwrap_or_else(|err| UpdateState::Failed(err.message()))
}

/// Whether Windows started CodexBar for its startup task (Start with Windows) rather than the user starting it.
pub fn launched_at_startup() -> bool {
    use windows::ApplicationModel::Activation::ActivationKind;
    installed().is_some()
        && windows::ApplicationModel::AppInstance::GetActivatedEventArgs()
            .and_then(|args| args.Kind())
            .is_ok_and(|kind| kind == ActivationKind::StartupTask)
}

/// Installs the channel's newer version. Windows closes CodexBar to replace it and starts it again afterwards, so on
/// success this call doesn't return; it returns the error otherwise. Blocking: run it off the UI thread.
pub fn install() -> Result<(), String> {
    let _apartment = Apartment::enter();
    use windows::ApplicationModel::Package;
    use windows::Management::Deployment::{AddPackageByAppInstallerOptions, PackageManager, PackageVolume};
    use windows::Win32::System::Recovery::{REGISTER_APPLICATION_RESTART_FLAGS, RegisterApplicationRestart};
    use windows::core::PCWSTR;
    let result = (|| -> windows::core::Result<Option<String>> {
        let uri = Package::Current()?.GetAppInstallerInfo()?.Uri()?;
        // Windows only restarts a process that has run for at least a minute.
        let ran = started().elapsed();
        if ran < MIN_RUN_FOR_RESTART {
            std::thread::sleep(MIN_RUN_FOR_RESTART - ran);
        }
        // Asks Windows to start CodexBar again once the update has replaced it. A null command line would cancel
        // the registration, so it carries an argument CodexBar ignores.
        let restart = windows::core::HSTRING::from(RESTART_ARG);
        // SAFETY: a valid null-terminated command line and no flags.
        unsafe { RegisterApplicationRestart(PCWSTR(restart.as_ptr()), REGISTER_APPLICATION_RESTART_FLAGS(0)) }?;
        let deployment = PackageManager::new()?
            .AddPackageByAppInstallerFileAsync(
                &uri,
                AddPackageByAppInstallerOptions::ForceTargetAppShutdown,
                None::<&PackageVolume>,
            )?
            .join()?;
        // The result code says whether it worked; the text only describes a failure.
        let code = deployment.ExtendedErrorCode()?;
        if code.is_ok() {
            return Ok(None);
        }
        let text = deployment.ErrorText()?.to_string();
        Ok(Some(if text.is_empty() {
            windows::core::Error::from_hresult(code).message()
        } else {
            text
        }))
    })();
    match result {
        Ok(None) => Ok(()),
        Ok(Some(text)) => Err(text),
        Err(err) => Err(err.message()),
    }
}

/// The `StartupTask` in the manifest: Start with Windows for the package.
pub const STARTUP_TASK: &str = "CodexBarStartup";
/// `package.ps1 -Install` leaves this file in the settings folder when it moved run.ps1's `Run` entry to the package.
pub const STARTUP_MIGRATION: &str = "start-with-windows.migrate";

/// Whether the package starts with Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Startup {
    On,
    Off,
    /// Turned off in Task Manager; only the user can turn it on again there.
    OffByUser,
    /// Set by the organization's policy, on or off.
    ByPolicy(bool),
}

impl Startup {
    pub fn is_on(self) -> bool {
        matches!(self, Self::On | Self::ByPolicy(true))
    }

    pub fn note(self) -> &'static str {
        match self {
            Self::On | Self::Off => "Starts CodexBar in the notification area when you sign in to Windows.",
            Self::OffByUser => {
                "Turned off in Task Manager › Startup apps. Turn CodexBar on there to start it with Windows again."
            }
            Self::ByPolicy(_) => "Set by your organization's policy.",
        }
    }
}

fn startup_task() -> windows::core::Result<windows::ApplicationModel::StartupTask> {
    windows::ApplicationModel::StartupTask::GetAsync(&windows::core::HSTRING::from(STARTUP_TASK))?.join()
}

fn startup_from(state: windows::ApplicationModel::StartupTaskState) -> Startup {
    use windows::ApplicationModel::StartupTaskState;
    match state {
        StartupTaskState::Enabled => Startup::On,
        StartupTaskState::DisabledByUser => Startup::OffByUser,
        StartupTaskState::EnabledByPolicy => Startup::ByPolicy(true),
        StartupTaskState::DisabledByPolicy => Startup::ByPolicy(false),
        _ => Startup::Off,
    }
}

/// The package's Start with Windows state; `None` for the unpackaged build or when Windows can't say.
pub fn startup() -> Option<Startup> {
    let _apartment = Apartment::enter();
    installed()?;
    Some(startup_from(startup_task().ok()?.State().ok()?))
}

/// Turns Start with Windows on or off and returns the state Windows ends up in (a user's or a policy's choice wins).
pub fn set_startup(on: bool) -> Option<Startup> {
    let _apartment = Apartment::enter();
    installed()?;
    let task = startup_task().ok()?;
    if on {
        task.RequestEnableAsync().ok()?.join().ok().map(startup_from)
    } else {
        task.Disable().ok()?;
        Some(startup_from(task.State().ok()?))
    }
}

/// Turns Start with Windows on once, if `package.ps1` moved it over from run.ps1's `Run` entry.
pub fn migrate_startup(dir: &std::path::Path) {
    let marker = dir.join(STARTUP_MIGRATION);
    if installed().is_some() && marker.exists() && set_startup(true).is_some() {
        let _ = std::fs::remove_file(marker);
    }
}

/// The Start with Windows state Settings shows, read once and after each change.
pub struct StartupSetting(pub Option<Startup>);

impl Global for StartupSetting {}

impl StartupSetting {
    pub fn init(cx: &mut App) {
        cx.set_global(Self(startup()));
    }

    pub fn get(cx: &App) -> Option<Startup> {
        cx.try_global::<Self>().and_then(|setting| setting.0)
    }

    pub fn set(cx: &mut App, on: bool) {
        let task = cx.background_executor().spawn(async move { set_startup(on) });
        cx.spawn(async move |cx| {
            let state = task.await;
            cx.update(|cx| {
                cx.set_global(Self(state));
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

/// `codexbar --package-status <file>`: writes the package identity and a fresh update check to `file` as JSON and
/// exits, so the package can be verified without opening a window (`scripts/Test-MsixChannel.ps1`).
pub fn write_status(path: &std::path::Path, trigger: Option<&std::path::Path>) -> std::io::Result<()> {
    // A marker first, so a caller waiting on the status can tell "never started" from "still checking".
    let _ = std::fs::write(path.with_extension("started"), "");
    if let Some(trigger) = trigger {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
        while !trigger.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    let status = match installed() {
        None => serde_json::json!({ "packaged": false }),
        Some(installed) => {
            let update = match UpdateState::initial(Some(installed)) {
                UpdateState::NotChecked => check(),
                state => state,
            };
            serde_json::json!({
                "packaged": true,
                "version": installed.version,
                "family": installed.family,
                "channel": installed.app_installer,
                "update": update.label(),
                "updateAvailable": update.can_install(),
            })
        }
    };
    std::fs::write(path, status.to_string())
}

/// How long a process must have run before `RegisterApplicationRestart` restarts it, with a margin.
const MIN_RUN_FOR_RESTART: std::time::Duration = std::time::Duration::from_secs(62);

/// When CodexBar started, as near as it knows: set when the update state is first set up at startup.
fn started() -> std::time::Instant {
    static STARTED: OnceLock<std::time::Instant> = OnceLock::new();
    *STARTED.get_or_init(std::time::Instant::now)
}

/// The argument Windows starts CodexBar with after Install and restart; nothing reads it.
pub const RESTART_ARG: &str = "--after-update";

/// How often the background check runs while CodexBar is open.
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
/// The first check waits until startup is over.
const FIRST_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// The update state the About page shows.
pub struct Updates {
    pub state: UpdateState,
}

impl Global for Updates {}

impl Updates {
    pub fn init(cx: &mut App, background: bool) {
        started();
        let state = UpdateState::initial(installed());
        let checks = background && state == UpdateState::NotChecked;
        cx.set_global(Self { state });
        if checks {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FIRST_CHECK_DELAY).await;
                loop {
                    cx.update(Self::check_now);
                    cx.background_executor().timer(CHECK_INTERVAL).await;
                }
            })
            .detach();
        }
    }

    pub fn state(cx: &App) -> UpdateState {
        cx.try_global::<Self>()
            .map_or(UpdateState::NotPackaged, |updates| updates.state.clone())
    }

    #[cfg(test)]
    pub fn set_for_test(cx: &mut App, state: UpdateState) {
        Self::set(cx, state);
    }

    fn set(cx: &mut App, state: UpdateState) {
        cx.set_global(Self { state });
        cx.refresh_windows();
    }

    /// Starts a check, unless one is running or the app has no channel.
    pub fn check_now(cx: &mut App) {
        if !Self::state(cx).can_check() {
            return;
        }
        Self::set(cx, UpdateState::Checking);
        let task = cx.background_executor().spawn(async { check() });
        cx.spawn(async move |cx| {
            let state = task.await;
            cx.update(|cx| Self::set(cx, state));
        })
        .detach();
    }

    pub fn install_now(cx: &mut App) {
        if !Self::state(cx).can_install() {
            return;
        }
        Self::set(cx, UpdateState::Installing);
        let task = cx.background_executor().spawn(async { install() });
        cx.spawn(async move |cx| {
            let state = match task.await {
                Ok(()) => UpdateState::UpToDate,
                Err(message) => UpdateState::Failed(message),
            };
            cx.update(|cx| Self::set(cx, state));
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packaged(app_installer: Option<&str>) -> Installed {
        Installed {
            version: "0.1.0.42".into(),
            family: "HemSoft.CodexBar_abc123".into(),
            app_installer: app_installer.map(str::to_owned),
        }
    }

    #[test]
    fn tests_run_unpackaged() {
        assert_eq!(installed(), None);
        assert_eq!(UpdateState::initial(installed()), UpdateState::NotPackaged);
    }

    #[test]
    fn only_a_package_from_an_app_installer_file_can_check() {
        assert_eq!(UpdateState::initial(Some(&packaged(None))), UpdateState::NoChannel);
        let channel = UpdateState::initial(Some(&packaged(Some(r"C:\channel\CodexBar.appinstaller"))));
        assert_eq!(channel, UpdateState::NotChecked);
        assert!(channel.can_check());
        assert!(!UpdateState::NotPackaged.can_check());
        assert!(!UpdateState::NoChannel.can_check());
        assert!(!UpdateState::Checking.can_check(), "one check at a time");
        assert!(!UpdateState::Installing.can_check());
    }

    #[test]
    fn only_an_available_update_can_be_installed() {
        assert!(UpdateState::Available { required: false }.can_install());
        assert!(UpdateState::Available { required: true }.can_install());
        for state in [
            UpdateState::NotPackaged,
            UpdateState::NoChannel,
            UpdateState::NotChecked,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Installing,
            UpdateState::Failed("offline".into()),
        ] {
            assert!(!state.can_install(), "{state:?}");
        }
    }

    #[test]
    fn labels_say_what_to_do() {
        assert!(UpdateState::NotPackaged.label().contains("run.ps1"));
        assert_eq!(
            UpdateState::Failed("The network path was not found.".into()).label(),
            "Couldn't update: The network path was not found."
        );
        assert!(UpdateState::Available { required: true }.label().contains("required"));
    }

    #[test]
    fn the_app_user_model_id_is_family_and_application() {
        assert_eq!(packaged(None).app_user_model_id(), "HemSoft.CodexBar_abc123!CodexBar");
    }

    #[test]
    fn the_manifest_declares_the_application_id() {
        let manifest = include_str!("../../../packaging/AppxManifest.xml");
        assert!(manifest.contains(&format!(r#"<Application Id="{APPLICATION_ID}""#)));
    }

    #[test]
    fn the_manifest_declares_the_startup_task_off() {
        let manifest = include_str!("../../../packaging/AppxManifest.xml");
        assert!(manifest.contains(&format!(
            r#"<desktop:StartupTask TaskId="{STARTUP_TASK}" Enabled="false""#
        )));
    }

    #[test]
    fn startup_is_unknown_unpackaged_and_the_migration_marker_stays() {
        assert_eq!(startup(), None);
        assert_eq!(set_startup(true), None);
        let dir = std::env::temp_dir().join(format!("codexbar-startup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(STARTUP_MIGRATION), "").unwrap();
        migrate_startup(&dir);
        let kept = dir.join(STARTUP_MIGRATION).exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(kept, "only the package moves Start with Windows over");
    }

    #[test]
    fn startup_states_read_as_on_or_off() {
        assert!(Startup::On.is_on() && Startup::ByPolicy(true).is_on());
        assert!(!Startup::Off.is_on() && !Startup::OffByUser.is_on() && !Startup::ByPolicy(false).is_on());
        assert!(Startup::OffByUser.note().contains("Task Manager"));
    }

    #[test]
    fn the_status_file_says_unpackaged() {
        let path = std::env::temp_dir().join(format!("codexbar-package-status-{}.json", std::process::id()));
        write_status(&path, None).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(written, r#"{"packaged":false}"#);
    }

    #[test]
    fn unpackaged_checks_and_installs_fail_without_panicking() {
        assert!(matches!(check(), UpdateState::Failed(_)));
        assert!(install().is_err());
    }

    #[test]
    fn calls_on_threads_that_come_and_go_never_crash() {
        // Each thread ends before the next starts, as background workers do; the cached activation factories must
        // stay usable (#157).
        for _ in 0..3 {
            std::thread::spawn(|| {
                assert_eq!(startup(), None);
                assert!(matches!(check(), UpdateState::Failed(_)));
                assert!(install().is_err());
            })
            .join()
            .unwrap();
        }
    }
}
