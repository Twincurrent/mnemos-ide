//! Mnemos IDE: the per-install key behind the Linux file fallback of secure storage.
//!
//! Upstream encrypted fallback secrets with a fixed string compiled into the binary, so
//! anyone with the binary could decrypt them. The key is now 32 random bytes generated
//! on first use and kept next to the fallback files, readable only by the owner (0600).
//! The legacy fixed key is still accepted for reading so existing fallback files can be
//! migrated (decrypted with it and rewritten with the per-install key).
//!
//! Only std is used here so the file handling can be unit-tested on every unix host,
//! not just Linux.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Length of an AES-256-GCM key.
pub(super) const KEY_LEN: usize = 32;

/// The fixed key upstream used for every install. Read-only: it decrypts fallback
/// files written before the per-install key existed, and is never used to encrypt.
pub(super) fn legacy_key_bytes() -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    let legacy = b"https://releases.warp.dev/channel_versions.json";
    key.copy_from_slice(&legacy[..KEY_LEN]);
    key
}

/// Where the per-install key for `service_name` lives inside the fallback dir. Fallback
/// secret files are named `<service>-<key>`, so the leading dot keeps the two apart.
pub(super) fn key_path(fallback_dir: &Path, service_name: &str) -> PathBuf {
    fallback_dir.join(format!(".{service_name}.fallback-key"))
}

/// Reads the key at `path`, or creates it with bytes from `fill_random` when it does not
/// exist yet. The file is created with mode 0600 and never overwritten, so two processes
/// racing on first use end up sharing whichever key was written first.
pub(super) fn load_or_create_key(
    path: &Path,
    fill_random: impl FnOnce(&mut [u8; KEY_LEN]),
) -> io::Result<[u8; KEY_LEN]> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match open_new_private(path) {
        Ok(mut file) => {
            let mut key = [0u8; KEY_LEN];
            fill_random(&mut key);
            file.write_all(&key)?;
            file.sync_all()?;
            Ok(key)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => read_key(path),
        Err(err) => Err(err),
    }
}

fn read_key(path: &Path) -> io::Result<[u8; KEY_LEN]> {
    let mut bytes = Vec::with_capacity(KEY_LEN);
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    <[u8; KEY_LEN]>::try_from(bytes.as_slice()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("fallback key file has {} bytes, expected {KEY_LEN}", bytes.len()),
        )
    })
}

/// Creates `path` exclusively, readable and writable by the owner only.
pub(super) fn open_new_private(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Writes `data` to `path` with owner-only permissions, replacing any existing file
/// through a temporary file and a rename so a crash never leaves a half-written secret.
pub(super) fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp-write");
    let _ = std::fs::remove_file(&tmp);
    let mut file = open_new_private(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_key_once_and_reuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(dir.path(), "dev.mnemos.Mnemos");
        let first = load_or_create_key(&path, |k| k.fill(7)).unwrap();
        assert_eq!(first, [7u8; KEY_LEN]);
        // A second call must read the stored key, not generate a new one.
        let second = load_or_create_key(&path, |k| k.fill(9)).unwrap();
        assert_eq!(second, first);
    }

    #[cfg(unix)]
    #[test]
    fn key_and_secret_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(dir.path(), "svc");
        load_or_create_key(&path, |k| k.fill(1)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let secret = dir.path().join("svc-token");
        write_private(&secret, b"one").unwrap();
        write_private(&secret, b"two").unwrap();
        assert_eq!(std::fs::read(&secret).unwrap(), b"two");
        let mode = std::fs::metadata(&secret).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn rejects_a_truncated_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(dir.path(), "svc");
        std::fs::write(&path, [1u8; 5]).unwrap();
        let err = load_or_create_key(&path, |k| k.fill(2)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn key_file_does_not_collide_with_secret_files() {
        let path = key_path(Path::new("/x"), "svc");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with('.'));
        assert!(!name.starts_with("svc-"));
    }

    #[test]
    fn legacy_key_is_the_upstream_constant() {
        let key = legacy_key_bytes();
        assert_eq!(&key[..], &b"https://releases.warp.dev/channel_versions.json"[..KEY_LEN]);
    }
}
