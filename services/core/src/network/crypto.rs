use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::{aead, agreement, digest, rand};

pub struct EphemeralKey {
    private_key: agreement::EphemeralPrivateKey,
    public_key_bytes: Vec<u8>,
}

impl EphemeralKey {
    pub fn new() -> Result<Self> {
        let rng = rand::SystemRandom::new();
        let private_key = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng)
            .map_err(|_| anyhow!("Failed to generate ephemeral private key"))?;
        let public_key_bytes = private_key
            .compute_public_key()
            .map_err(|_| anyhow!("Failed to compute public key"))?
            .as_ref()
            .to_vec();
        Ok(Self {
            private_key,
            public_key_bytes,
        })
    }

    pub fn public_key_b64(&self) -> String {
        B64.encode(&self.public_key_bytes)
    }

    pub fn agree_and_derive(self, peer_public_key_b64: &str) -> Result<[u8; 32]> {
        let peer_public_key_bytes = B64
            .decode(peer_public_key_b64)
            .map_err(|e| anyhow!("Invalid base64 in peer public key: {}", e))?;
        let peer_public_key = agreement::UnparsedPublicKey::new(&agreement::X25519, peer_public_key_bytes);

        let derived_key = agreement::agree_ephemeral(
            self.private_key,
            &peer_public_key,
            |key_material| {
                let hash = digest::digest(&digest::SHA256, key_material);
                let mut key = [0u8; 32];
                key.copy_from_slice(hash.as_ref());
                key
            },
        ).map_err(|_| anyhow!("Key agreement failed"))?;

        Ok(derived_key)
    }
}

pub struct SessionCipher {
    key: aead::LessSafeKey,
}

impl std::fmt::Debug for SessionCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCipher").finish_non_exhaustive()
    }
}

impl SessionCipher {
    pub fn new(key_bytes: &[u8; 32]) -> Result<Self> {
        let unbound_key = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key_bytes)
            .map_err(|_| anyhow!("Failed to create unbound key"))?;
        let key = aead::LessSafeKey::new(unbound_key);
        Ok(Self { key })
    }

    pub fn encrypt(&self, seq: u16, timestamp_us: u64, is_rtcp: bool, payload: &mut Vec<u8>) -> Result<()> {
        let nonce_bytes = make_nonce(seq, timestamp_us, is_rtcp);
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::empty(), payload)
            .map_err(|_| anyhow!("Encryption failed"))?;
        Ok(())
    }

    pub fn decrypt(&self, seq: u16, timestamp_us: u64, is_rtcp: bool, ciphertext: &mut Vec<u8>) -> Result<()> {
        let nonce_bytes = make_nonce(seq, timestamp_us, is_rtcp);
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let plaintext_len = {
            let plaintext_slice = self
                .key
                .open_in_place(nonce, aead::Aad::empty(), ciphertext)
                .map_err(|_| anyhow!("Decryption failed"))?;
            plaintext_slice.len()
        };
        ciphertext.truncate(plaintext_len);
        Ok(())
    }

    pub fn encrypt_packet(&self, nonce_bytes: [u8; 12], payload: &mut Vec<u8>) -> Result<()> {
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::empty(), payload)
            .map_err(|_| anyhow!("Encryption failed"))?;
        Ok(())
    }

    pub fn decrypt_packet(&self, nonce_bytes: [u8; 12], ciphertext: &mut Vec<u8>) -> Result<()> {
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let plaintext_len = {
            let plaintext_slice = self
                .key
                .open_in_place(nonce, aead::Aad::empty(), ciphertext)
                .map_err(|_| anyhow!("Decryption failed"))?;
            plaintext_slice.len()
        };
        ciphertext.truncate(plaintext_len);
        Ok(())
    }
}

fn make_nonce(seq: u16, timestamp_us: u64, is_rtcp: bool) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0] = if is_rtcp { 1 } else { 0 };
    nonce[1..3].copy_from_slice(&seq.to_be_bytes());
    nonce[3..11].copy_from_slice(&timestamp_us.to_be_bytes());
    nonce
}
