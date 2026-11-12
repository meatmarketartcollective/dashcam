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
