use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

/// Encrypt to base64(nonce || ciphertext) with AES-256-GCM.
pub fn encrypt(key: &[u8], plaintext: &str) -> String {
    let cipher = Aes256Gcm::new_from_slice(key).expect("key must be 32 bytes");
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).expect("entropy");
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .expect("encryption");
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    B64.encode(out)
}

/// Decrypt base64(nonce || ciphertext). None means wrong key or damaged blob.
pub fn decrypt(key: &[u8], blob: &str) -> Option<String> {
    let raw = B64.decode(blob).ok()?;
    if raw.len() < 28 {
        // nonce (12) + at least one block with its auth tag (16)
        return None;
    }
    let (nonce, ct) = raw.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let pt = cipher.decrypt(Nonce::from_slice(nonce), ct).ok()?;
    String::from_utf8(pt).ok()
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 {
        return None;
    }
    (0..32)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
        .collect()
}
