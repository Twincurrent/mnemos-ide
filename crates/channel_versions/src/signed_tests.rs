use super::*;

/// A fixed test key. Never used for a real release.
fn test_key() -> SecretKey {
    SecretKey::from_parts([1, 2, 3, 4, 5, 6, 7, 8], [7u8; 32]).unwrap()
}

fn other_key() -> SecretKey {
    SecretKey::from_parts([9, 9, 9, 9, 9, 9, 9, 9], [42u8; 32]).unwrap()
}

fn payload() -> ReleasePayload {
    let mut assets = BTreeMap::new();
    assets.insert(
        "Mnemos-arm64.dmg".to_owned(),
        AssetDigest {
            sha256: "ab".repeat(32),
            size: 12,
        },
    );
    let mut platforms = BTreeMap::new();
    platforms.insert(
        "macos-aarch64".to_owned(),
        PlatformRelease {
            assets,
            ..PlatformRelease::new("v0.2026.10.09.12.00.stable_01")
        },
    );
    ReleasePayload {
        format: PAYLOAD_FORMAT,
        channel: "stable".to_owned(),
        issued_at: "2026-10-09T12:00:00Z".to_owned(),
        platforms,
    }
}

#[test]
fn key_text_forms_round_trip() {
    let key = test_key();
    let public = key.public_key();
    assert_eq!(PublicKey::parse(&public.to_string()).unwrap(), public);
    let again = SecretKey::parse(&key.to_secret_text()).unwrap();
    assert_eq!(again.public_key(), public);
    assert_eq!(public.key_id_hex(), "0102030405060708");
}

#[test]
fn parse_list_skips_comments_and_blank_lines() {
    let text = format!(
        "# the release key\n\n{}\n  # retired\n{}\n",
        test_key().public_key(),
        other_key().public_key()
    );
    assert_eq!(PublicKey::parse_list(&text).unwrap().len(), 2);
    assert!(PublicKey::parse_list("# nothing here\n")
        .unwrap()
        .is_empty());
    assert!(PublicKey::parse_list("mnemos-update-pub:not-base64!").is_err());
    assert!(PublicKey::parse(&test_key().to_secret_text()).is_err());
}

#[test]
fn signed_payload_verifies_and_round_trips() {
    let key = test_key();
    let envelope = sign_payload(&key, &payload()).unwrap();
    let verified =
        verify_envelope(&[other_key().public_key(), key.public_key()], &envelope).unwrap();
    assert_eq!(verified, payload());
}

#[test]
fn tampered_payload_is_rejected() {
    let key = test_key();
    let mut envelope = sign_payload(&key, &payload()).unwrap();
    let mut tampered = payload();
    tampered.platforms.get_mut("macos-aarch64").unwrap().version =
        "v0.2099.01.01.00.00.stable_01".to_owned();
    envelope.payload = BASE64.encode(serde_json::to_vec(&tampered).unwrap());
    assert!(verify_envelope(&[key.public_key()], &envelope).is_err());
}

#[test]
fn unknown_or_missing_keys_are_rejected() {
    let envelope = sign_payload(&test_key(), &payload()).unwrap();
    assert!(verify_envelope(&[other_key().public_key()], &envelope).is_err());
    assert!(verify_envelope(&[], &envelope).is_err());

    // A different key that claims the signer's id still fails the signature.
    let mut impostor = sign_payload(&other_key(), &payload()).unwrap();
    impostor.key_id = test_key().key_id_hex();
    assert!(verify_envelope(&[test_key().public_key()], &impostor).is_err());
}

#[test]
fn unknown_payload_format_is_rejected() {
    let mut future = payload();
    future.format = PAYLOAD_FORMAT + 1;
    let envelope = sign_payload(&test_key(), &future).unwrap();
    assert!(verify_envelope(&[test_key().public_key()], &envelope).is_err());
}

fn manifest_with(
    signed: Option<SignedEnvelope>,
    signed_next: Vec<SignedEnvelope>,
) -> crate::ChannelVersions {
    let legacy = r#"{
      "dev": { "version": "v0.2026.10.01.10.00.stable_01" },
      "preview": { "version": "v0.2026.10.01.10.00.stable_01" },
      "stable": { "version": "v0.2026.10.01.10.00.stable_01" }
    }"#;
    let mut versions: crate::ChannelVersions = serde_json::from_str(legacy).unwrap();
    versions.signed = signed;
    versions.signed_next = signed_next;
    versions
}

#[test]
fn a_newer_format_beside_format_1_does_not_strand_this_build() {
    let key = test_key();
    let mut future = payload();
    future.format = PAYLOAD_FORMAT + 1;
    let versions = manifest_with(
        Some(sign_payload(&key, &payload()).unwrap()),
        vec![sign_payload(&key, &future).unwrap()],
    );
    assert_eq!(
        verify_manifest(&[key.public_key()], &versions).unwrap(),
        payload()
    );

    // The serialized manifest keeps both slots, and a reader that knows only `signed` still
    // finds format 1 there.
    let text = serde_json::to_string(&versions).unwrap();
    let parsed: crate::ChannelVersions = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.signed_next.len(), 1);
    verify_envelope(&[key.public_key()], parsed.signed.as_ref().unwrap()).unwrap();

    // Only unknown formats, or nothing signed: refused.
    let only_future = manifest_with(None, vec![sign_payload(&key, &future).unwrap()]);
    assert!(verify_manifest(&[key.public_key()], &only_future).is_err());
    assert!(verify_manifest(&[key.public_key()], &manifest_with(None, vec![])).is_err());
    // A bad envelope in `signed_next` does not hide a good one in `signed`.
    let mut forged = sign_payload(&other_key(), &future).unwrap();
    forged.key_id = key.key_id_hex();
    let versions = manifest_with(Some(sign_payload(&key, &payload()).unwrap()), vec![forged]);
    verify_manifest(&[key.public_key()], &versions).unwrap();
}

#[test]
fn apply_to_drops_unsigned_hints() {
    let mut info = crate::VersionInfo::new("v0.2026.10.01.10.00.stable_01".to_owned());
    info.soft_cutoff = Some("v0.2099.01.01.00.00.stable_01".to_owned());
    info.update_by = Some(chrono::DateTime::parse_from_rfc3339("2026-10-10T00:00:00Z").unwrap());
    info.last_prominent_update = Some("v0.2099.01.01.00.00.stable_01".to_owned());
    info.is_rollback = Some(true);
    info.cli_version = Some("v0.2099.01.01.00.00.stable_01".to_owned());

    let release = payload().platforms["macos-aarch64"].clone();
    release.apply_to(&mut info);
    assert_eq!(info.version, release.version);
    assert_eq!(info.is_rollback, None);
    assert_eq!(info.soft_cutoff, None);
    assert_eq!(info.update_by, None);
    assert_eq!(info.last_prominent_update, None);
    assert_eq!(info.cli_version, None);
    assert_eq!(info.verified_assets.as_ref(), Some(&release.assets));

    let signed_hints = PlatformRelease {
        soft_cutoff: Some("v0.2026.09.01.00.00.stable_01".to_owned()),
        ..release
    };
    signed_hints.apply_to(&mut info);
    assert_eq!(info.soft_cutoff, signed_hints.soft_cutoff);
}

#[test]
fn contexts_do_not_cross() {
    let key = test_key();
    let message = b"abc  Mnemos-arm64.dmg\n";
    let line = sign_detached(&key, SHA256SUMS_CONTEXT, message);
    verify_detached(&[key.public_key()], SHA256SUMS_CONTEXT, message, &line).unwrap();
    assert!(verify_detached(&[key.public_key()], MANIFEST_CONTEXT, message, &line).is_err());
    assert!(verify_detached(&[key.public_key()], SHA256SUMS_CONTEXT, b"other", &line).is_err());
}

#[test]
fn sha256_and_digest_check() {
    let digest = sha256_reader(&b"abc"[..]).unwrap();
    assert_eq!(
        digest.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(digest.size, 3);
    check_digest("a", &digest, &digest).unwrap();
    let mut wrong_size = digest.clone();
    wrong_size.size = 4;
    assert!(check_digest("a", &wrong_size, &digest).is_err());
    let mut wrong_hash = digest.clone();
    wrong_hash.sha256 = "00".repeat(32);
    assert!(check_digest("a", &wrong_hash, &digest).is_err());
}

#[test]
fn old_manifests_still_parse_and_new_fields_are_optional() {
    // What every published manifest looked like before signing: the legacy fields only.
    let legacy = r#"{
      "dev": { "version": "v0.2026.10.01.10.00.stable_01" },
      "preview": { "version": "v0.2026.10.01.10.00.stable_01" },
      "stable": { "version": "v0.2026.10.01.10.00.stable_01" }
    }"#;
    let parsed: crate::ChannelVersions = serde_json::from_str(legacy).unwrap();
    assert!(parsed.signed.is_none());

    // A signed manifest keeps the legacy fields, so builds without signing support still read it.
    let mut value: serde_json::Value = serde_json::from_str(legacy).unwrap();
    value["signed"] = serde_json::to_value(sign_payload(&test_key(), &payload()).unwrap()).unwrap();
    let parsed: crate::ChannelVersions = serde_json::from_value(value).unwrap();
    assert!(parsed.signed.is_some());
}

#[test]
fn current_platform_has_os_and_arch() {
    let platform = current_platform();
    assert!(platform.starts_with(std::env::consts::OS));
    assert!(platform.ends_with(std::env::consts::ARCH));
}
