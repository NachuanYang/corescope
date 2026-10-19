use fs2::FileExt as Fs2FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const VAULT_FILE: &str = "chatgpt-credentials.dpapi";
const LOCK_FILE: &str = "chatgpt-credentials.lock";
const MAX_CIPHERTEXT_BYTES: usize = 384 * 1024;
const MAX_PLAINTEXT_BYTES: usize = 256 * 1024;
const DIRECT_USAGE_SCOPE: &str = "chatgpt.tokens.use.direct";

#[derive(Clone, Deserialize, Serialize)]
pub struct TokenSet {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub scopes: Vec<String>,
    pub expires_at_unix: i64,
}

impl Drop for TokenSet {
    fn drop(&mut self) {
        self.id_token.zeroize();
        self.access_token.zeroize();
        self.refresh_token.zeroize();
        self.token_type.zeroize();
        self.scopes.zeroize();
        self.expires_at_unix.zeroize();
    }
}

impl TokenSet {
    pub fn permits_direct_plan_usage(&self) -> bool {
        self.scopes.iter().any(|scope| scope == DIRECT_USAGE_SCOPE)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct CredentialProfile {
    pub client_id: String,
    pub subject: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub host_id: String,
    pub tokens: Option<TokenSet>,
}

#[derive(Clone, Deserialize, Serialize)]
struct VaultDocument {
    schema_version: u32,
    host_id: Option<String>,
    active_client_id: Option<String>,
    accounts: BTreeMap<String, CredentialProfile>,
}

impl Default for VaultDocument {
    fn default() -> Self {
        Self {
            schema_version: 1,
            host_id: None,
            active_client_id: None,
            accounts: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct CredentialVault {
    root: PathBuf,
}

impl CredentialVault {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root).map_err(|_| "AUTH_VAULT_DIRECTORY_UNAVAILABLE".to_owned())?;
        Ok(Self { root })
    }

    pub fn load_or_create_host_id(&self) -> Result<String, String> {
        self.update_document(|document| {
            if let Some(host_id) = &document.host_id {
                validate_host_id(host_id)?;
                return Ok(host_id.clone());
            }
            let host_id = format!("urn:uuid:{}", Uuid::new_v4().hyphenated());
            document.host_id = Some(host_id.clone());
            Ok(host_id)
        })
    }

    pub fn account_profiles(&self) -> Result<Vec<CredentialProfile>, String> {
        self.read_document(|document| Ok(document.accounts.values().cloned().collect()))
    }

    pub fn account_profile(&self, client_id: &str) -> Result<Option<CredentialProfile>, String> {
        self.read_document(|document| Ok(document.accounts.get(client_id).cloned()))
    }

    pub fn acquire_refresh_lock(&self, client_id: &str) -> Result<File, String> {
        if !valid_issued_client_id(client_id) {
            return Err("AUTH_CLIENT_ID_INVALID".to_owned());
        }
        fs::create_dir_all(&self.root)
            .map_err(|_| "AUTH_VAULT_DIRECTORY_UNAVAILABLE".to_owned())?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root.join(format!("refresh-{client_id}.lock")))
            .map_err(|_| "AUTH_REFRESH_LOCK_UNAVAILABLE".to_owned())?;
        file.lock_exclusive()
            .map_err(|_| "AUTH_REFRESH_LOCK_FAILED".to_owned())?;
        Ok(file)
    }

    pub fn active_account(&self) -> Result<Option<String>, String> {
        self.read_document(|document| Ok(document.active_client_id.clone()))
    }

    pub fn set_active_account(&self, client_id: Option<&str>) -> Result<(), String> {
        self.update_document(|document| {
            if let Some(client_id) = client_id {
                if !document.accounts.contains_key(client_id) {
                    return Err("AUTH_ACCOUNT_NOT_FOUND".to_owned());
                }
            }
            document.active_client_id = client_id.map(str::to_owned);
            Ok(())
        })
    }

    pub fn save_account(&self, profile: CredentialProfile) -> Result<(), String> {
        self.update_document(|document| {
            let host_id = document
                .host_id
                .as_deref()
                .ok_or_else(|| "AUTH_HOST_ID_NOT_INITIALIZED".to_owned())?;
            validate_profile(&profile, host_id)?;
            if let Some(previous) = document.accounts.get(&profile.client_id) {
                if previous.subject != profile.subject {
                    return Err("AUTH_ACCOUNT_IDENTITY_MISMATCH".to_owned());
                }
            }
            document.accounts.insert(profile.client_id.clone(), profile);
            Ok(())
        })
    }

    pub fn clear_account_tokens(&self, client_id: &str) -> Result<(), String> {
        self.update_document(|document| {
            let account = document
                .accounts
                .get_mut(client_id)
                .ok_or_else(|| "AUTH_ACCOUNT_NOT_FOUND".to_owned())?;
            account.tokens = None;
            if document.active_client_id.as_deref() == Some(client_id) {
                document.active_client_id = None;
            }
            Ok(())
        })
    }

    pub fn remove_account(&self, client_id: &str) -> Result<(), String> {
        self.update_document(|document| {
            if document.accounts.remove(client_id).is_none() {
                return Err("AUTH_ACCOUNT_NOT_FOUND".to_owned());
            }
            if document.active_client_id.as_deref() == Some(client_id) {
                document.active_client_id = None;
            }
            Ok(())
        })
    }

    fn read_document<T>(
        &self,
        read: impl FnOnce(&VaultDocument) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lock = self.lock()?;
        let document = self.read_unlocked()?;
        read(&document)
    }

    fn update_document<T>(
        &self,
        update: impl FnOnce(&mut VaultDocument) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lock = self.lock()?;
        let mut document = self.read_unlocked()?;
        let result = update(&mut document)?;
        self.write_unlocked(&document)?;
        Ok(result)
    }

    fn lock(&self) -> Result<File, String> {
        fs::create_dir_all(&self.root)
            .map_err(|_| "AUTH_VAULT_DIRECTORY_UNAVAILABLE".to_owned())?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root.join(LOCK_FILE))
            .map_err(|_| "AUTH_VAULT_LOCK_UNAVAILABLE".to_owned())?;
        file.lock_exclusive()
            .map_err(|_| "AUTH_VAULT_LOCK_FAILED".to_owned())?;
        Ok(file)
    }

    fn read_unlocked(&self) -> Result<VaultDocument, String> {
        let path = self.root.join(VAULT_FILE);
        let encrypted = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VaultDocument::default());
            }
            Err(_) => return Err("AUTH_VAULT_READ_FAILED".to_owned()),
        };
        if encrypted.is_empty() || encrypted.len() > MAX_CIPHERTEXT_BYTES {
            return Err("AUTH_VAULT_INVALID_SIZE".to_owned());
        }
        let plaintext = Zeroizing::new(unprotect_current_user(&encrypted)?);
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err("AUTH_VAULT_INVALID_SIZE".to_owned());
        }
        let document: VaultDocument = serde_json::from_slice(&plaintext)
            .map_err(|_| "AUTH_VAULT_DECODE_FAILED".to_owned())?;
        validate_document(&document)?;
        Ok(document)
    }

    fn write_unlocked(&self, document: &VaultDocument) -> Result<(), String> {
        validate_document(document)?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(document).map_err(|_| "AUTH_VAULT_ENCODE_FAILED".to_owned())?,
        );
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err("AUTH_VAULT_INVALID_SIZE".to_owned());
        }
        let encrypted = protect_current_user(&plaintext)?;
        if encrypted.len() > MAX_CIPHERTEXT_BYTES {
            return Err("AUTH_VAULT_INVALID_SIZE".to_owned());
        }
        atomic_write(&self.root.join(VAULT_FILE), &encrypted)
    }
}

fn validate_document(document: &VaultDocument) -> Result<(), String> {
    if document.schema_version != 1 {
        return Err("AUTH_VAULT_UNSUPPORTED_VERSION".to_owned());
    }
    if let Some(host_id) = &document.host_id {
        validate_host_id(host_id)?;
    }
    if let Some(active) = &document.active_client_id {
        if !document.accounts.contains_key(active) {
            return Err("AUTH_VAULT_ACTIVE_ACCOUNT_MISSING".to_owned());
        }
    }
    if document.accounts.len() > 16 {
        return Err("AUTH_VAULT_TOO_MANY_ACCOUNTS".to_owned());
    }
    if let Some(host_id) = document.host_id.as_deref() {
        for (key, profile) in &document.accounts {
            if key != &profile.client_id {
                return Err("AUTH_VAULT_ACCOUNT_KEY_MISMATCH".to_owned());
            }
            validate_profile(profile, host_id)?;
        }
    } else if !document.accounts.is_empty() {
        return Err("AUTH_VAULT_HOST_ID_MISSING".to_owned());
    }
    Ok(())
}

fn validate_profile(profile: &CredentialProfile, host_id: &str) -> Result<(), String> {
    if !valid_issued_client_id(&profile.client_id)
        || profile.subject.trim().is_empty()
        || profile.subject.len() > 512
        || profile.host_id != host_id
        || profile
            .email
            .as_ref()
            .is_some_and(|value| value.len() > 320)
        || profile
            .display_name
            .as_ref()
            .is_some_and(|value| value.len() > 160)
    {
        return Err("AUTH_VAULT_PROFILE_INVALID".to_owned());
    }
    if let Some(tokens) = &profile.tokens {
        if tokens.id_token.is_empty()
            || tokens.access_token.is_empty()
            || tokens.refresh_token.is_empty()
            || tokens.id_token.len() > 64 * 1024
            || tokens.access_token.len() > 64 * 1024
            || tokens.refresh_token.len() > 64 * 1024
            || !tokens.token_type.eq_ignore_ascii_case("bearer")
            || tokens.scopes.len() > 32
            || tokens
                .scopes
                .iter()
                .any(|scope| scope.is_empty() || scope.len() > 256)
        {
            return Err("AUTH_VAULT_TOKEN_SET_INVALID".to_owned());
        }
    }
    Ok(())
}

fn valid_issued_client_id(client_id: &str) -> bool {
    client_id.starts_with("oaiapp_")
        && client_id.len() <= 512
        && client_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn validate_host_id(host_id: &str) -> Result<(), String> {
    let uuid = host_id
        .strip_prefix("urn:uuid:")
        .ok_or_else(|| "AUTH_VAULT_HOST_ID_INVALID".to_owned())?;
    Uuid::parse_str(uuid)
        .map(|_| ())
        .map_err(|_| "AUTH_VAULT_HOST_ID_INVALID".to_owned())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "AUTH_VAULT_PATH_INVALID".to_owned())?;
    let temporary = parent.join(format!(".chatgpt-vault-{}.tmp", Uuid::new_v4().simple()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| "AUTH_VAULT_TEMP_CREATE_FAILED".to_owned())?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        let _ = fs::remove_file(&temporary);
        return Err("AUTH_VAULT_WRITE_FAILED".to_owned());
    }
    drop(file);
    let result = replace_file(&temporary, path);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err("AUTH_VAULT_ATOMIC_REPLACE_FAILED".to_owned())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    fs::rename(from, to).map_err(|_| "AUTH_VAULT_ATOMIC_REPLACE_FAILED".to_owned())
}

#[cfg(windows)]
fn protect_current_user(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };
    if plaintext.len() > u32::MAX as usize {
        return Err("AUTH_VAULT_INVALID_SIZE".to_owned());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len() as u32,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let protected = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if protected == 0 || output.pbData.is_null() {
        return Err("AUTH_DPAPI_PROTECT_FAILED".to_owned());
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(result)
}

#[cfg(not(windows))]
fn protect_current_user(_plaintext: &[u8]) -> Result<Vec<u8>, String> {
    Err("AUTH_DPAPI_UNAVAILABLE".to_owned())
}

#[cfg(windows)]
fn unprotect_current_user(encrypted: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };
    if encrypted.len() > u32::MAX as usize {
        return Err("AUTH_VAULT_INVALID_SIZE".to_owned());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let unprotected = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if unprotected == 0 || output.pbData.is_null() {
        return Err("AUTH_DPAPI_UNPROTECT_FAILED".to_owned());
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { zeroize_allocation(output.pbData, output.cbData as usize) };
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(result)
}

#[cfg(not(windows))]
fn unprotect_current_user(_encrypted: &[u8]) -> Result<Vec<u8>, String> {
    Err("AUTH_DPAPI_UNAVAILABLE".to_owned())
}

#[cfg(windows)]
unsafe fn zeroize_allocation(pointer: *mut u8, length: usize) {
    for index in 0..length {
        unsafe { std::ptr::write_volatile(pointer.add(index), 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}
