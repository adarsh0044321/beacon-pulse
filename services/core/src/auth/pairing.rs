//! Pairing code management with HMAC-SHA256 challenge-response (Phase 4d).
//!
//! Protocol upgrade over Phase 3 (plain code comparison):
//!   1. Host generates 6-digit code  →  displayed in UI
//!   2. On JoinRequest, host generates a random 32-byte challenge,
//!      base64-encodes it, and sends it in `PairingRequired { challenge }`.
//!   3. Client computes HMAC-SHA256(key = pairing_code, data = challenge)
//!      and sends `PairingCode { hmac: base64(result) }`.
//!   4. Host verifies with `ring::hmac::verify`.
//!      Challenge is invalidated on success or expiry to prevent replay.
//!
//! Why this is better than Phase 3:
//!   - Plain code never travels over the network → no trivial replay attacks.
//!   - HMAC binds code to the specific challenge session.
//!   - ring's constant-time verify prevents timing side-channels.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::Rng;
use ring::hmac;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const CODE_EXPIRY_SECS: u64 = 120;
const CHALLENGE_LEN: usize = 32;

pub struct PairingManager {
    current_code: Option<String>,
    issued_at: Option<Instant>,
    /// Server-side challenge bytes; cleared after first use or expiry.
    active_challenge: Option<[u8; CHALLENGE_LEN]>,
    is_custom: bool,
}

impl PairingManager {
    pub fn new() -> Self {
        Self {
            current_code: None,
            issued_at: None,
            active_challenge: None,
            is_custom: false,
        }
    }

    /// Generate a new 6-digit pairing code (expires after 120 s).
    pub fn generate_code(&mut self) -> String {
        let code: String = rand::thread_rng()
            .gen_range(100_000u32..=999_999u32)
            .to_string();
        self.current_code = Some(code.clone());
        self.issued_at = Some(Instant::now());
        self.active_challenge = None; // reset any stale challenge
        self.is_custom = false;
        info!(
            "Pairing code generated: {} (expires in {}s)",
            code, CODE_EXPIRY_SECS
        );
        code
    }

    /// Set a custom pairing code.
    pub fn set_code(&mut self, code: String) {
        self.current_code = Some(code.clone());
        self.issued_at = Some(Instant::now());
        self.active_challenge = None;
        self.is_custom = true;
        info!("Custom pairing code set: {}", code);
    }

    /// Generate a cryptographic challenge for the current pairing session.
    ///
    /// Returns `Some(base64_challenge)` if a valid code is active, `None` otherwise.
    /// The raw bytes are stored in `active_challenge` for later verification.
    pub fn generate_challenge(&mut self) -> Option<String> {
        if !self.has_valid_code() {
            warn!("generate_challenge called with no active pairing code");
            return None;
        }
        let mut challenge = [0u8; CHALLENGE_LEN];
        rand::thread_rng().fill(&mut challenge);
        let encoded = B64.encode(challenge);
        self.active_challenge = Some(challenge);
        Some(encoded)
    }

    /// Verify the HMAC-SHA256 response sent by the client.
    ///
    /// `response_b64` must be base64(HMAC-SHA256(key=pairing_code, data=challenge)).
    /// The challenge is invalidated after the first call (success or failure)
    /// to prevent replay attacks within the same session.
    pub fn verify_hmac(&mut self, response_b64: &str) -> bool {
        let (code, issued_at, challenge) =
            match (&self.current_code, self.issued_at, &self.active_challenge) {
                (Some(c), Some(i), Some(ch)) => (c.clone(), i, *ch),
                _ => {
                    warn!("verify_hmac: no active pairing session");
                    return false;
                }
            };

        // Check expiry before doing any crypto
        if !self.is_custom
            && Instant::now().saturating_duration_since(issued_at)
                > Duration::from_secs(CODE_EXPIRY_SECS)
        {
            warn!("Pairing: HMAC challenge expired");
            self.active_challenge = None;
            return false;
        }

        // Always clear challenge after one attempt (replay prevention)
        self.active_challenge = None;

        let response = match B64.decode(response_b64) {
            Ok(r) => r,
            Err(_) => {
                warn!("Pairing: invalid base64 in HMAC response");
                return false;
            }
        };

        let key = hmac::Key::new(hmac::HMAC_SHA256, code.as_bytes());
        match hmac::verify(&key, &challenge, &response) {
            Ok(()) => {
                info!("Pairing: HMAC verification succeeded");
                true
            }
            Err(_) => {
                warn!("Pairing: HMAC mismatch — rejecting client");
                false
            }
        }
    }

    /// Invalidate the current pairing code and challenge.
    pub fn invalidate(&mut self) {
        self.current_code = None;
        self.issued_at = None;
        self.active_challenge = None;
        self.is_custom = false;
    }

    pub fn has_valid_code(&self) -> bool {
        if let (Some(_), Some(issued_at)) = (&self.current_code, self.issued_at) {
            self.is_custom
                || Instant::now().saturating_duration_since(issued_at)
                    <= Duration::from_secs(CODE_EXPIRY_SECS)
        } else {
            false
        }
    }

    /// Generate a stateless cryptographic challenge without mutating active_challenge.
    pub fn generate_challenge_stateless(&self) -> Option<String> {
        if !self.has_valid_code() {
            warn!("generate_challenge_stateless called with no active pairing code");
            return None;
        }
        let mut challenge = [0u8; CHALLENGE_LEN];
        rand::thread_rng().fill(&mut challenge);
        Some(B64.encode(challenge))
    }

    /// Verify an HMAC-SHA256 response against a stateless challenge.
    pub fn verify_hmac_with_challenge(&self, challenge_b64: &str, response_b64: &str) -> bool {
        let (code, issued_at) = match (&self.current_code, self.issued_at) {
            (Some(c), Some(i)) => (c.clone(), i),
            _ => {
                warn!("verify_hmac_with_challenge: no active pairing session");
                return false;
            }
        };

        // Check expiry
        if !self.is_custom
            && Instant::now().saturating_duration_since(issued_at)
                > Duration::from_secs(CODE_EXPIRY_SECS)
        {
            warn!("Pairing: HMAC challenge expired");
            return false;
        }

        let challenge = match B64.decode(challenge_b64) {
            Ok(c) => c,
            Err(_) => {
                warn!("Pairing: invalid base64 in challenge");
                return false;
            }
        };

        let response = match B64.decode(response_b64) {
            Ok(r) => r,
            Err(_) => {
                warn!("Pairing: invalid base64 in HMAC response");
                return false;
            }
        };

        let key = hmac::Key::new(hmac::HMAC_SHA256, code.as_bytes());
        match hmac::verify(&key, &challenge, &response) {
            Ok(()) => {
                info!("Pairing: HMAC verification succeeded");
                true
            }
            Err(_) => {
                warn!("Pairing: HMAC mismatch — rejecting client");
                false
            }
        }
    }

    pub fn current_code(&self) -> Option<String> {
        if self.has_valid_code() {
            self.current_code.clone()
        } else {
            None
        }
    }

    pub fn expires_in_secs(&self) -> Option<u32> {
        if self.is_custom {
            Some(3600)
        } else if let Some(issued_at) = self.issued_at {
            let elapsed = Instant::now()
                .saturating_duration_since(issued_at)
                .as_secs();
            if elapsed < CODE_EXPIRY_SECS {
                Some((CODE_EXPIRY_SECS - elapsed) as u32)
            } else {
                Some(0)
            }
        } else {
            None
        }
    }
}
