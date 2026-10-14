//! Account secrets (API keys, dashboard cookies) in the system's secret store, keyed by stable account id (#74).
//! On Windows each secret is a generic credential in Credential Manager named `CodexBar:account:<id>`, persisted for
//! the signed-in user on this machine. On macOS it is a Keychain password and on Linux a Secret Service item, both
//! under the service `CodexBar` with the account id as the user (#161).

use std::fmt;

#[cfg(windows)]
const TARGET_PREFIX: &str = "CodexBar:account:";

/// The secret store's name in the middle of a sentence ("Saved in …").
pub const STORE_NAME: &str = codexbar_core::secret_store_name!();

/// The secret store's short name, once it was named in full nearby.
#[cfg(windows)]
pub const STORE_SHORT_NAME: &str = "Credential Manager";
#[cfg(not(windows))]
pub const STORE_SHORT_NAME: &str = STORE_NAME;

/// The secret store's name at the start of a sentence.
#[cfg(windows)]
pub const STORE_TITLE: &str = "Windows Credential Manager";
#[cfg(target_os = "macos")]
pub const STORE_TITLE: &str = "The macOS Keychain";
#[cfg(not(any(windows, target_os = "macos")))]
pub const STORE_TITLE: &str = "The Secret Service keyring";

/// A store failure, kept distinct from "no secret stored" (`Ok(None)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialError {
    pub code: u32,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            VERIFY_FAILED => write!(f, "{STORE_TITLE} didn't return the saved secret."),
            #[cfg(not(windows))]
            ACCESS_DENIED => write!(f, "{STORE_TITLE} is locked or refused access."),
            #[cfg(not(windows))]
            UNAVAILABLE => write!(f, "{STORE_TITLE} isn't available."),
            code => write!(f, "{STORE_TITLE} failed (error {code})."),
        }
    }
}

impl std::error::Error for CredentialError {}

impl CredentialError {
    /// The store accepted a secret but returned something else when read back.
    pub fn verification() -> Self {
        Self { code: VERIFY_FAILED }
    }
}

/// Not a Windows error code: the code `CredentialError::verification` reports.
const VERIFY_FAILED: u32 = 0xC0DE_0001;
/// The keychain is locked or refused access; Windows' own code for access denied.
#[cfg(not(windows))]
const ACCESS_DENIED: u32 = 5;
/// No keychain could be reached, e.g. no Secret Service running.
#[cfg(not(windows))]
const UNAVAILABLE: u32 = 0xC0DE_0002;

/// Read, add/replace and delete secrets by account id.
pub trait CredentialStore: Send + Sync {
    fn read(&self, account_id: &str) -> Result<Option<String>, CredentialError>;
    fn write(&self, account_id: &str, secret: &str) -> Result<(), CredentialError>;
    /// Deleting a secret that isn't there is not an error.
    fn delete(&self, account_id: &str) -> Result<(), CredentialError>;
}

#[cfg(windows)]
pub fn target_name(account_id: &str) -> String {
    format!("{TARGET_PREFIX}{account_id}")
}

/// This platform's secret store.
#[cfg(windows)]
pub type SystemCredentialStore = WindowsCredentialStore;
#[cfg(not(windows))]
pub type SystemCredentialStore = keychain::KeychainCredentialStore;

/// Windows Credential Manager.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsCredentialStore {
    /// Prefix for tests so they never touch real CodexBar entries.
    namespace: Option<&'static str>,
}

#[cfg(windows)]
impl WindowsCredentialStore {
    pub fn new() -> Self {
        Self { namespace: None }
    }

    #[cfg(test)]
    fn testing() -> Self {
        Self {
            namespace: Some("CodexBarTest:"),
        }
    }

    fn target(&self, account_id: &str) -> Vec<u16> {
        let name = match self.namespace {
            Some(namespace) => format!("{namespace}{account_id}"),
            None => target_name(account_id),
        };
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows::Win32::Security::Credentials::{
        CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
        CredWriteW,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::{CredentialError, CredentialStore, WindowsCredentialStore};

    fn last_error() -> CredentialError {
        CredentialError {
            code: unsafe { GetLastError().0 },
        }
    }

    impl CredentialStore for WindowsCredentialStore {
        fn read(&self, account_id: &str) -> Result<Option<String>, CredentialError> {
            let target = self.target(account_id);
            let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `target` is NUL-terminated UTF-16 that outlives the call; on success the API allocates
            // `credential`, which is freed with CredFree below after its blob is copied.
            let found = unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut credential) };
            if let Err(err) = found {
                return if err.code() == ERROR_NOT_FOUND.to_hresult() {
                    Ok(None)
                } else {
                    Err(last_error())
                };
            }
            // SAFETY: CredReadW succeeded, so `credential` points to a valid CREDENTIALW whose blob holds
            // `CredentialBlobSize` bytes.
            let secret = unsafe {
                let blob =
                    std::slice::from_raw_parts((*credential).CredentialBlob, (*credential).CredentialBlobSize as usize);
                let units: Vec<u16> = blob
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .collect();
                CredFree(credential as *const _);
                String::from_utf16_lossy(&units)
            };
            Ok(Some(secret))
        }

        fn write(&self, account_id: &str, secret: &str) -> Result<(), CredentialError> {
            let mut target = self.target(account_id);
            let mut user: Vec<u16> = "CodexBar".encode_utf16().chain(std::iter::once(0)).collect();
            let mut blob: Vec<u8> = secret.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let credential = CREDENTIALW {
                Flags: CRED_FLAGS(0),
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(target.as_mut_ptr()),
                CredentialBlobSize: blob.len() as u32,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: PWSTR(user.as_mut_ptr()),
                ..Default::default()
            };
            // SAFETY: every pointer in `credential` refers to a live local buffer for the duration of the call.
            let result = unsafe { CredWriteW(&credential, 0) };
            blob.iter_mut().for_each(|byte| *byte = 0);
            result.map_err(|_| last_error())
        }

        fn delete(&self, account_id: &str) -> Result<(), CredentialError> {
            let target = self.target(account_id);
            // SAFETY: `target` is NUL-terminated UTF-16 that outlives the call.
            match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
                Ok(()) => Ok(()),
                Err(err) if err.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
                Err(_) => Err(last_error()),
            }
        }
    }
}

/// The macOS Keychain or the Linux Secret Service, through the keyring crates. Each secret is a password under the
/// service `CodexBar` with the account id as its user.
#[cfg(not(windows))]
pub mod keychain {
    use std::sync::{Arc, OnceLock};

    use keyring_core::{CredentialStore as Backend, Entry, Error};

    use super::{ACCESS_DENIED, CredentialError, CredentialStore, UNAVAILABLE};

    const SERVICE: &str = "CodexBar";

    #[derive(Clone, Copy, Debug, Default)]
    pub struct KeychainCredentialStore;

    impl KeychainCredentialStore {
        pub fn new() -> Self {
            Self
        }

        fn entry(&self, account_id: &str) -> Result<Entry, CredentialError> {
            backend()?.build(SERVICE, account_id, None).map_err(|err| error(&err))
        }
    }

    /// The platform store, connected once. A failed connection (no Secret Service running) is kept and reported on
    /// every use rather than retried on each read.
    fn backend() -> Result<Arc<Backend>, CredentialError> {
        static BACKEND: OnceLock<Result<Arc<Backend>, CredentialError>> = OnceLock::new();
        BACKEND
            .get_or_init(|| {
                #[cfg(target_os = "macos")]
                let store = apple_native_keyring_store::keychain::Store::new();
                #[cfg(not(target_os = "macos"))]
                let store = zbus_secret_service_keyring_store::Store::new();
                store.map(|store| store as Arc<Backend>).map_err(|err| error(&err))
            })
            .clone()
    }

    /// The error's kind only: a keyring error can carry the secret's bytes, which must never reach a message.
    pub(super) fn error(err: &Error) -> CredentialError {
        let code = match err {
            Error::NoStorageAccess(_) => ACCESS_DENIED,
            Error::NoDefaultStore | Error::NotSupportedByStore(_) => UNAVAILABLE,
            _ => 1,
        };
        CredentialError { code }
    }

    impl CredentialStore for KeychainCredentialStore {
        fn read(&self, account_id: &str) -> Result<Option<String>, CredentialError> {
            match self.entry(account_id)?.get_password() {
                Ok(secret) => Ok(Some(secret)),
                Err(Error::NoEntry) => Ok(None),
                Err(err) => Err(error(&err)),
            }
        }

        fn write(&self, account_id: &str, secret: &str) -> Result<(), CredentialError> {
            self.entry(account_id)?.set_password(secret).map_err(|err| error(&err))
        }

        fn delete(&self, account_id: &str) -> Result<(), CredentialError> {
            match self.entry(account_id)?.delete_credential() {
                Ok(()) | Err(Error::NoEntry) => Ok(()),
                Err(err) => Err(error(&err)),
            }
        }
    }
}

/// An in-memory store for tests and for machines without Credential Manager.
#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    secrets: std::sync::Mutex<std::collections::HashMap<String, String>>,
    fail: bool,
}

impl MemoryCredentialStore {
    /// A store whose every operation fails, to exercise access-failure paths.
    pub fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn read(&self, account_id: &str) -> Result<Option<String>, CredentialError> {
        if self.fail {
            return Err(CredentialError { code: 5 });
        }
        Ok(self
            .secrets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(account_id)
            .cloned())
    }

    fn write(&self, account_id: &str, secret: &str) -> Result<(), CredentialError> {
        if self.fail {
            return Err(CredentialError { code: 5 });
        }
        self.secrets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(account_id.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, account_id: &str) -> Result<(), CredentialError> {
        if self.fail {
            return Err(CredentialError { code: 5 });
        }
        self.secrets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(account_id);
        Ok(())
    }
}

/// Where an account's secret currently comes from, for the Settings screen. Never carries the secret itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretSource {
    Environment(&'static str),
    CredentialManager,
    /// Still plaintext in the WPF settings file; moves to Credential Manager at cutover (#74).
    SettingsFile,
    Missing,
    /// Credential Manager could not be read; distinct from Missing.
    StoreUnavailable(CredentialError),
}
/// Windows Credential Manager holds at most 2,560 bytes per credential, 1,280 characters as stored here (UTF-16).
/// Secrets longer than one credential (OAuth tokens, #77) are split across numbered parts.
pub const PART_CHARS: usize = 1200;
const CHUNKED_PREFIX: &str = "codexbar-parts:";

/// Writes a secret of any length. Short secrets are one credential, as before. Long ones are written as parts of a
/// new generation first, then the account's credential is switched to point at them, then the previous generation's
/// parts are deleted. An interrupted write leaves the previous secret readable.
pub fn write_long(store: &dyn CredentialStore, account_id: &str, secret: &str) -> Result<(), CredentialError> {
    let previous = parts_of(store.read(account_id)?.as_deref());
    if secret.chars().count() <= PART_CHARS && !secret.starts_with(CHUNKED_PREFIX) {
        store.write(account_id, secret)?;
    } else {
        let generation = previous.map_or(1, |(generation, _)| generation + 1);
        let chars: Vec<char> = secret.chars().collect();
        let parts: Vec<String> = chars.chunks(PART_CHARS).map(|part| part.iter().collect()).collect();
        for (ix, part) in parts.iter().enumerate() {
            store.write(&part_id(account_id, generation, ix), part)?;
        }
        store.write(account_id, &format!("{CHUNKED_PREFIX}{generation}:{}", parts.len()))?;
    }
    if let Some((generation, count)) = previous {
        for ix in 0..count {
            // Left-over parts are harmless (nothing points at them); a later write tries again.
            let _ = store.delete(&part_id(account_id, generation, ix));
        }
    }
    Ok(())
}

/// Reads a secret written by `write_long` (or a plain one).
pub fn read_long(store: &dyn CredentialStore, account_id: &str) -> Result<Option<String>, CredentialError> {
    let Some(head) = store.read(account_id)? else {
        return Ok(None);
    };
    let Some((generation, count)) = parts_of(Some(&head)) else {
        return Ok(Some(head));
    };
    let mut secret = String::new();
    for ix in 0..count {
        match store.read(&part_id(account_id, generation, ix))? {
            Some(part) => secret.push_str(&part),
            // A part is missing: the secret is incomplete, so it counts as not there.
            None => return Ok(None),
        }
    }
    Ok(Some(secret))
}

/// Deletes a secret written by `write_long`, with its parts.
pub fn delete_long(store: &dyn CredentialStore, account_id: &str) -> Result<(), CredentialError> {
    if let Some((generation, count)) = parts_of(store.read(account_id)?.as_deref()) {
        for ix in 0..count {
            store.delete(&part_id(account_id, generation, ix))?;
        }
    }
    store.delete(account_id)
}

fn parts_of(head: Option<&str>) -> Option<(u64, usize)> {
    let (generation, count) = head?.strip_prefix(CHUNKED_PREFIX)?.split_once(':')?;
    Some((generation.parse().ok()?, count.parse().ok()?))
}

fn part_id(account_id: &str, generation: u64, ix: usize) -> String {
    format!("{account_id}#part{generation}.{ix}")
}

/// Resolves a secret: environment variable, then Credential Manager, then the legacy settings file.
pub fn resolve_secret(
    env: Option<&'static str>,
    store: &dyn CredentialStore,
    account_id: &str,
    legacy: Option<String>,
) -> (Option<String>, SecretSource) {
    if let Some(name) = env
        && let Some(value) = std::env::var(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    {
        return (Some(value), SecretSource::Environment(name));
    }
    match store.read(account_id) {
        Ok(Some(secret)) if !secret.trim().is_empty() => (Some(secret), SecretSource::CredentialManager),
        Ok(_) => match legacy {
            Some(secret) => (Some(secret), SecretSource::SettingsFile),
            None => (None, SecretSource::Missing),
        },
        // A failed store still lets the legacy key work, but the status shows the failure.
        Err(err) => (legacy, SecretSource::StoreUnavailable(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store that, like Credential Manager, refuses secrets longer than one credential.
    #[derive(Default)]
    struct Limited(MemoryCredentialStore);

    impl CredentialStore for Limited {
        fn read(&self, id: &str) -> Result<Option<String>, CredentialError> {
            self.0.read(id)
        }

        fn write(&self, id: &str, secret: &str) -> Result<(), CredentialError> {
            if secret.encode_utf16().count() * 2 > 2560 {
                return Err(CredentialError { code: 1783 });
            }
            self.0.write(id, secret)
        }

        fn delete(&self, id: &str) -> Result<(), CredentialError> {
            self.0.delete(id)
        }
    }

    #[test]
    fn long_secrets_are_split_and_replaced_by_generation() {
        let store = Limited::default();
        let long: String = "a".repeat(3000) + &"b".repeat(1000);
        assert!(store.write("acct", &long).is_err(), "too long for one credential");
        write_long(&store, "acct", &long).unwrap();
        assert!(read_long(&store, "acct").unwrap().as_deref() == Some(long.as_str()));
        // A renewal replaces it; the previous parts are deleted.
        let renewed: String = "c".repeat(2500);
        write_long(&store, "acct", &renewed).unwrap();
        assert!(read_long(&store, "acct").unwrap().as_deref() == Some(renewed.as_str()));
        assert_eq!(store.read("acct#part1.0").unwrap(), None, "old generation gone");
        // Short secrets stay one credential, and deleting removes every part.
        write_long(&store, "short", "sk-short").unwrap();
        assert_eq!(store.read("short").unwrap().as_deref(), Some("sk-short"));
        delete_long(&store, "acct").unwrap();
        assert_eq!(read_long(&store, "acct").unwrap(), None);
        assert_eq!(store.read("acct#part2.0").unwrap(), None);
    }

    #[test]
    fn a_missing_part_reads_as_no_secret() {
        let store = MemoryCredentialStore::default();
        write_long(&store, "acct", &"x".repeat(2000)).unwrap();
        store.delete("acct#part1.1").unwrap();
        assert_eq!(read_long(&store, "acct").unwrap(), None);
    }

    #[test]
    fn memory_store_add_replace_read_delete() {
        let store = MemoryCredentialStore::default();
        assert_eq!(store.read("a").unwrap(), None);
        store.write("a", "one").unwrap();
        store.write("a", "two").unwrap();
        assert_eq!(store.read("a").unwrap().as_deref(), Some("two"));
        store.delete("a").unwrap();
        store.delete("a").unwrap();
        assert_eq!(store.read("a").unwrap(), None);
    }

    #[test]
    fn resolve_secret_prefers_store_over_settings_file() {
        let store = MemoryCredentialStore::default();
        store.write("acct", "from-store").unwrap();
        let (secret, source) = resolve_secret(None, &store, "acct", Some("from-file".into()));
        assert_eq!(
            (secret.as_deref(), source),
            (Some("from-store"), SecretSource::CredentialManager)
        );
    }

    #[test]
    fn resolve_secret_falls_back_to_settings_file_then_missing() {
        let store = MemoryCredentialStore::default();
        let (secret, source) = resolve_secret(None, &store, "acct", Some("from-file".into()));
        assert_eq!(
            (secret.as_deref(), source),
            (Some("from-file"), SecretSource::SettingsFile)
        );
        assert_eq!(resolve_secret(None, &store, "acct", None).1, SecretSource::Missing);
    }

    #[test]
    fn resolve_secret_store_failure_is_distinct_from_missing() {
        let (secret, source) = resolve_secret(None, &MemoryCredentialStore::failing(), "acct", Some("file".into()));
        assert_eq!(secret.as_deref(), Some("file"));
        assert!(matches!(source, SecretSource::StoreUnavailable(_)));
    }

    #[cfg(not(windows))]
    #[test]
    fn keychain_errors_keep_only_their_kind() {
        use keyring_core::Error;
        let locked = keychain::error(&Error::NoStorageAccess("locked".into()));
        assert_eq!(
            locked.to_string(),
            format!("{STORE_TITLE} is locked or refused access.")
        );
        let missing = keychain::error(&Error::NoDefaultStore);
        assert_eq!(missing.to_string(), format!("{STORE_TITLE} isn't available."));
        // The bytes of a badly encoded secret never reach the message.
        let bad = keychain::error(&Error::BadEncoding(b"sk-secret".to_vec()));
        assert!(!bad.to_string().contains("sk-secret") && !format!("{bad:?}").contains("sk-secret"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_store_round_trip() {
        let store = WindowsCredentialStore::testing();
        let id = format!("roundtrip-{}", std::process::id());
        store.delete(&id).unwrap();
        assert_eq!(store.read(&id).unwrap(), None);
        store.write(&id, "sk-test-ünïcode").unwrap();
        store.write(&id, "sk-test-replaced").unwrap();
        assert_eq!(store.read(&id).unwrap().as_deref(), Some("sk-test-replaced"));
        store.delete(&id).unwrap();
        assert_eq!(store.read(&id).unwrap(), None);
    }
}
