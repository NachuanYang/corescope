#![cfg(windows)]

use corescope_lib::auth::{CredentialProfile, CredentialVault, TokenSet};

fn profile(client_id: &str, subject: &str, marker: &str, host_id: &str) -> CredentialProfile {
    CredentialProfile {
        client_id: client_id.to_owned(),
        subject: subject.to_owned(),
        email: Some(format!("{subject}@example.invalid")),
        display_name: None,
        host_id: host_id.to_owned(),
        tokens: Some(TokenSet {
            id_token: format!("fixture-id-token-{marker}"),
            access_token: format!("fixture-access-token-{marker}"),
            refresh_token: format!("fixture-refresh-token-{marker}"),
            token_type: "Bearer".to_owned(),
            scopes: vec!["openid".to_owned(), "chatgpt.tokens.use.direct".to_owned()],
            expires_at_unix: 1_900_000_000,
        }),
    }
}

#[test]
fn vault_keeps_multiple_account_credentials_separate_and_never_writes_plaintext() {
    let directory = tempfile::tempdir().unwrap();
    let vault = CredentialVault::open(directory.path().to_path_buf()).unwrap();
    let host_id = vault.load_or_create_host_id().unwrap();

    vault
        .save_account(profile("oaiapp_first", "subject-first", "first", &host_id))
        .unwrap();
    vault
        .save_account(profile(
            "oaiapp_second",
            "subject-second",
            "second",
            &host_id,
        ))
        .unwrap();
    vault.set_active_account(Some("oaiapp_second")).unwrap();

    let saved_files = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    let saved_bytes = saved_files.iter().flatten().copied().collect::<Vec<_>>();
    let saved_text = String::from_utf8_lossy(&saved_bytes);
    for token in [
        "fixture-id-token",
        "fixture-access-token",
        "fixture-refresh-token",
    ] {
        assert!(!saved_text.contains(token));
    }

    let restored = CredentialVault::open(directory.path().to_path_buf()).unwrap();
    let profiles = restored.account_profiles().unwrap();
    assert_eq!(profiles.len(), 2);
    assert_eq!(
        restored.active_account().unwrap().as_deref(),
        Some("oaiapp_second")
    );
    assert_eq!(restored.load_or_create_host_id().unwrap(), host_id);
    assert_eq!(
        profiles
            .iter()
            .find(|item| item.client_id == "oaiapp_first")
            .unwrap()
            .tokens
            .as_ref()
            .unwrap()
            .refresh_token,
        "fixture-refresh-token-first"
    );
    assert_eq!(
        profiles
            .iter()
            .find(|item| item.client_id == "oaiapp_second")
            .unwrap()
            .tokens
            .as_ref()
            .unwrap()
            .refresh_token,
        "fixture-refresh-token-second"
    );
}

#[test]
fn local_sign_out_clears_tokens_but_retains_registration_and_host_identity() {
    let directory = tempfile::tempdir().unwrap();
    let vault = CredentialVault::open(directory.path().to_path_buf()).unwrap();
    let host_id = vault.load_or_create_host_id().unwrap();
    vault
        .save_account(profile("oaiapp_saved", "subject-saved", "saved", &host_id))
        .unwrap();
    vault.set_active_account(Some("oaiapp_saved")).unwrap();

    vault.clear_account_tokens("oaiapp_saved").unwrap();

    let saved = vault.account_profiles().unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0].tokens.is_none());
    assert_eq!(saved[0].client_id, "oaiapp_saved");
    assert_eq!(vault.active_account().unwrap(), None);
    assert_eq!(vault.load_or_create_host_id().unwrap(), host_id);
}

#[test]
fn refresh_replaces_rotating_tokens_only_within_the_same_registration() {
    let directory = tempfile::tempdir().unwrap();
    let vault = CredentialVault::open(directory.path().to_path_buf()).unwrap();
    let host_id = vault.load_or_create_host_id().unwrap();
    vault
        .save_account(profile(
            "oaiapp_rotate",
            "subject-rotate",
            "before",
            &host_id,
        ))
        .unwrap();

    vault
        .save_account(profile(
            "oaiapp_rotate",
            "subject-rotate",
            "after",
            &host_id,
        ))
        .unwrap();

    let saved = vault.account_profile("oaiapp_rotate").unwrap().unwrap();
    assert_eq!(
        saved.tokens.unwrap().refresh_token,
        "fixture-refresh-token-after"
    );
    assert!(vault.account_profile("oaiapp_other").unwrap().is_none());
}

#[test]
fn tampered_vault_fails_closed_instead_of_becoming_empty_accounts() {
    let directory = tempfile::tempdir().unwrap();
    let vault = CredentialVault::open(directory.path().to_path_buf()).unwrap();
    vault.load_or_create_host_id().unwrap();
    let path = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "dpapi")
        })
        .unwrap();
    std::fs::write(path, b"not protected data").unwrap();

    assert!(
        CredentialVault::open(directory.path().to_path_buf())
            .unwrap()
            .account_profiles()
            .is_err()
    );
}
