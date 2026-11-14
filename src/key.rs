use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Load the timeline key, creating and storing it (mode 0600) on first use.
///
/// The key is app-managed: nothing for the user to remember, and nothing
/// for another tool to guess. The database file is ciphertext everywhere it
/// travels; only the app can open it.
pub fn load_or_create(key_path: &Path) -> std::io::Result<String> {
    if key_path.exists() {
        return Ok(fs::read_to_string(key_path)?.trim().to_string());
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::File::create(key_path)?;
    f.write_all(hex.as_bytes())?;
    #[cfg(unix)]
    {
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key file is the whole trust root: it must be created once, never
    /// regenerated on reload, mode 0600, and read back trimmed.
    #[test]
    fn creates_a_0600_key_once_and_reloads_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("key");
        let k1 = load_or_create(&path).unwrap();
        assert_eq!(k1.len(), 64);
        assert!(k1.chars().all(|c| c.is_ascii_hexdigit()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "key file must be 0600");
        }
        let k2 = load_or_create(&path).unwrap();
        assert_eq!(k1, k2, "reload must never regenerate the key");
    }

    #[test]
    fn loads_an_existing_key_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, " abcdef0123456789 \n").unwrap();
        assert_eq!(load_or_create(&path).unwrap(), "abcdef0123456789");
    }
}
