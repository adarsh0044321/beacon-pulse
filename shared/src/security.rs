use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rcgen::generate_simple_self_signed;
use ring::hmac;

/// Generates a simple self-signed X.509 certificate and private key DER bytes.
/// Useful for securing LAN signaling/control channels via TLS.
pub fn generate_self_signed_cert(subject_alt_names: Vec<String>) -> Result<(Vec<u8>, Vec<u8>)> {
    let cert_key = generate_simple_self_signed(subject_alt_names)
        .map_err(|e| anyhow!("rcgen error generating self-signed cert: {}", e))?;

    let cert_der = cert_key.cert.der().to_vec();
    let key_der = cert_key.key_pair.serialize_der();

    Ok((cert_der, key_der))
}

/// Solves an HMAC challenge using the pairing PIN.
/// Computes: HMAC-SHA256(key = pairing_code, data = challenge_bytes)
pub fn solve_pairing_challenge(pairing_code: &str, challenge_b64: &str) -> Result<String> {
    let challenge = B64
        .decode(challenge_b64)
        .map_err(|e| anyhow!("Failed to decode base64 challenge: {}", e))?;

    let key = hmac::Key::new(hmac::HMAC_SHA256, pairing_code.as_bytes());
    let tag = hmac::sign(&key, &challenge);

    Ok(B64.encode(tag.as_ref()))
}

/// Verifies an HMAC response against the pairing PIN and raw challenge bytes.
pub fn verify_pairing_response(
    pairing_code: &str,
    challenge_bytes: &[u8],
    response_b64: &str,
) -> bool {
    let response = match B64.decode(response_b64) {
        Ok(r) => r,
        Err(_) => return false,
    };

    let key = hmac::Key::new(hmac::HMAC_SHA256, pairing_code.as_bytes());
    hmac::verify(&key, challenge_bytes, &response).is_ok()
}
