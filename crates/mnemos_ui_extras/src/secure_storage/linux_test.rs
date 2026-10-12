use super::Error;
use super::SecureStorage;

// Mnemos IDE: the fallback key lives in the fallback dir, so every test needs one.
fn storage_in(dir: &tempfile::TempDir, service: &str) -> SecureStorage {
    SecureStorage::new_with_fallback(service, dir.path().to_path_buf())
}

#[test]
fn test_encrypt_decrypt_returns_same_value() {
    let dir = tempfile::tempdir().unwrap();
    let storage = storage_in(&dir, "darmok");

    let input = String::from("darmok and jalad at tanagra");
    let encrypted = storage.fallback_encrypt(&input).unwrap();
    let output = storage.fallback_decrypt(&encrypted).unwrap();

    assert_eq!(input, output)
}

#[test]
fn test_encrypt_decrypt_works_across_storage_instances() {
    let dir = tempfile::tempdir().unwrap();
    let storage_1 = storage_in(&dir, "darmok");
    let storage_2 = storage_in(&dir, "darmok");

    let input = String::from("shaka when the walls fell");
    let encrypted = storage_1.fallback_encrypt(&input).unwrap();
    let output = storage_2.fallback_decrypt(&encrypted).unwrap();

    assert_eq!(input, output)
}

#[test]
fn test_installs_do_not_share_a_key() {
    let dir_1 = tempfile::tempdir().unwrap();
    let dir_2 = tempfile::tempdir().unwrap();
    let storage_1 = storage_in(&dir_1, "darmok");
    let storage_2 = storage_in(&dir_2, "darmok");

    let encrypted = storage_1.fallback_encrypt("temba his arms wide").unwrap();
    assert!(storage_2.fallback_decrypt(&encrypted).is_err());
}

#[test]
fn test_no_fallback_dir_means_no_key() {
    let storage = SecureStorage::new("darmok");
    assert!(storage.fallback_encrypt("mirab").is_err());
}

#[test]
fn test_legacy_fallback_file_is_read_and_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let storage = storage_in(&dir, "darmok");

    // Seal a value the way upstream did, with the fixed legacy key.
    let legacy = storage.legacy_key().unwrap();
    let nonce_bytes = [3u8; ring::aead::NONCE_LEN];
    let mut data = b"kiteo his eyes closed".to_vec();
    legacy
        .seal_in_place_append_tag(
            ring::aead::Nonce::assume_unique_for_key(nonce_bytes),
            ring::aead::Aad::empty(),
            &mut data,
        )
        .unwrap();
    let mut legacy_file = nonce_bytes.to_vec();
    legacy_file.extend_from_slice(&data);
    let path = storage.fallback_file("token").unwrap();
    std::fs::write(&path, &legacy_file).unwrap();

    assert_eq!(storage.read_fallback_value("token").unwrap(), "kiteo his eyes closed");
    // The file is now sealed with the per-install key.
    let migrated = std::fs::read(&path).unwrap();
    assert_ne!(migrated, legacy_file);
    assert_eq!(storage.fallback_decrypt(&migrated).unwrap(), "kiteo his eyes closed");
}

#[test]
fn test_decrypt_fails_on_malformed_data() {
    let dir = tempfile::tempdir().unwrap();
    let storage = storage_in(&dir, "darmok");

    let bad_datas: [&[u8]; 4] = [&[], &[0; 1], &[0; 11], &[0; 12]];

    for bad_data in bad_datas {
        let result = storage.fallback_decrypt(bad_data);

        assert!(result.is_err());
        let error = result.unwrap_err();
        let Error::Unknown(err) = error else {
            panic!("Expected error variant to be Error::Unknown, but found {error:?}")
        };
        assert_eq!(
            format!("{err}"),
            "Attempting to decrypt too small value for fallback decryption"
        );
    }
}
