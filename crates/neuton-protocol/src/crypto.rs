//! Login encryption: the RSA key exchange, the session-server handshake, and
//! the AES-128-CFB8 stream that carries everything afterwards.
//!
//! Verified against the vanilla implementation in `net.minecraft.util.Crypt`,
//! which specifies `RSA`, `AES/CFB8/NoPadding` and `SHA-1` over an ISO-8859-1
//! server ID.

use crate::error::{Error, Result};
use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use rand::RngCore;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey, pkcs8::DecodePublicKey};
use sha1::{Digest, Sha1};

/// Minecraft's shared secret is always AES-128.
pub const SECRET_LEN: usize = 16;

/// A freshly generated shared secret.
///
/// Zeroed on drop. It is only ever 16 bytes and only lives until the ciphers are
/// built, but leaving a session key lying in freed heap is avoidable.
pub struct SharedSecret([u8; SECRET_LEN]);

impl SharedSecret {
    /// Draws from the OS CSPRNG. Never use a seeded or thread RNG here: this
    /// key protects the whole session.
    pub fn generate() -> Self {
        let mut key = [0u8; SECRET_LEN];
        rand::rngs::OsRng.fill_bytes(&mut key);
        Self(key)
    }

    pub const fn as_bytes(&self) -> &[u8; SECRET_LEN] {
        &self.0
    }
}

impl Drop for SharedSecret {
    fn drop(&mut self) {
        // `write_volatile` so the compiler cannot decide this store is dead.
        for b in &mut self.0 {
            unsafe { core::ptr::write_volatile(b, 0) };
        }
    }
}

impl core::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SharedSecret(<redacted>)")
    }
}

/// Encrypts the shared secret and the server's challenge under the server's
/// public key, producing the two byte arrays of `ServerboundKeyPacket`.
///
/// The key arrives as a DER `SubjectPublicKeyInfo`, which is what Java's
/// `X509EncodedKeySpec` emits.
pub fn encrypt_key_exchange(
    public_key_der: &[u8],
    secret: &SharedSecret,
    challenge: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let key = RsaPublicKey::from_public_key_der(public_key_der)
        .map_err(|e| Error::Crypto(format!("server sent an unusable public key: {e}")))?;
    let mut rng = rand::rngs::OsRng;
    let enc_secret = key
        .encrypt(&mut rng, Pkcs1v15Encrypt, secret.as_bytes())
        .map_err(|e| Error::Crypto(format!("could not encrypt the shared secret: {e}")))?;
    let enc_challenge = key
        .encrypt(&mut rng, Pkcs1v15Encrypt, challenge)
        .map_err(|e| Error::Crypto(format!("could not encrypt the challenge: {e}")))?;
    Ok((enc_secret, enc_challenge))
}

/// Computes the server hash that both sides send to Mojang's session server.
///
/// This is SHA-1 over the server ID, the shared secret and the server's public
/// key, then rendered as a *signed* big-endian hex number: Minecraft prints the
/// digest as a Java `BigInteger`, so a digest with the high bit set becomes a
/// negative number written with a leading minus, and leading zeroes are dropped.
/// Getting this wrong is the classic cause of "invalid session" on join.
pub fn server_hash(server_id: &str, secret: &SharedSecret, public_key_der: &[u8]) -> String {
    let mut sha = Sha1::new();
    // ISO-8859-1, per Crypt.BYTE_ENCODING. Server IDs are ASCII in practice and
    // empty on modern servers, so the byte view is already correct.
    sha.update(server_id.as_bytes());
    sha.update(secret.as_bytes());
    sha.update(public_key_der);
    twos_complement_hex(&sha.finalize())
}

/// Renders a 20-byte digest the way Java's `new BigInteger(digest).toString(16)`
/// would.
fn twos_complement_hex(digest: &[u8]) -> String {
    let negative = digest[0] & 0x80 != 0;
    let mut bytes = digest.to_vec();
    if negative {
        // Negate: invert every byte, then add one with carry.
        for b in &mut bytes {
            *b = !*b;
        }
        for b in bytes.iter_mut().rev() {
            let (v, carry) = b.overflowing_add(1);
            *b = v;
            if !carry {
                break;
            }
        }
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let trimmed = hex.trim_start_matches('0');
    let body = if trimmed.is_empty() { "0" } else { trimmed };
    if negative { format!("-{body}") } else { body.to_string() }
}

/// One direction of an AES-128-CFB8 stream.
///
/// CFB8 is a mode, not a primitive: the block cipher underneath is the audited
/// `aes` crate. The mode itself is a shift register, which is why it is written
/// out here rather than pulled in.
///
/// Note that CFB8 costs one full AES block operation per *byte*. That is
/// inherent to the mode and vanilla pays exactly the same price; with hardware
/// AES it is not a bottleneck at Minecraft's packet rates.
pub struct Cfb8 {
    cipher: Aes128,
    /// The shift register. Starts as the IV, which Minecraft sets to the key.
    iv: [u8; 16],
}

impl Cfb8 {
    /// Minecraft uses the shared secret as both key and IV.
    pub fn new(secret: &SharedSecret) -> Self {
        Self {
            cipher: Aes128::new(GenericArray::from_slice(secret.as_bytes())),
            iv: *secret.as_bytes(),
        }
    }

    /// Encrypts in place, advancing the register.
    pub fn encrypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            let mut block = GenericArray::clone_from_slice(&self.iv);
            self.cipher.encrypt_block(&mut block);
            let cipher_byte = *byte ^ block[0];
            self.iv.copy_within(1.., 0);
            self.iv[15] = cipher_byte;
            *byte = cipher_byte;
        }
    }

    /// Decrypts in place, advancing the register.
    ///
    /// The register is fed the *ciphertext* in both directions, which is what
    /// makes CFB8 self-synchronising.
    pub fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            let mut block = GenericArray::clone_from_slice(&self.iv);
            self.cipher.encrypt_block(&mut block);
            let cipher_byte = *byte;
            *byte ^= block[0];
            self.iv.copy_within(1.., 0);
            self.iv[15] = cipher_byte;
        }
    }
}

impl core::fmt::Debug for Cfb8 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Cfb8(<state redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret_from(bytes: [u8; 16]) -> SharedSecret {
        SharedSecret(bytes)
    }

    #[test]
    fn cfb8_matches_the_nist_sp800_38a_vector() {
        // AES-128 CFB8, F.3.7 / F.3.8. Key and IV differ here, unlike in
        // Minecraft, which is exactly why this is a good independent check.
        let key = hex(b"2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex(b"000102030405060708090a0b0c0d0e0f");
        let plain = hex(b"6bc1bee22e409f96e93d7e117393172aae2d");
        let expected = hex(b"3b79424c9c0dd436bace9e0ed4586a4f32b9");

        let mut enc = Cfb8 {
            cipher: Aes128::new(GenericArray::from_slice(&key)),
            iv: iv.clone().try_into().unwrap(),
        };
        let mut buf = plain.clone();
        enc.encrypt(&mut buf);
        assert_eq!(buf, expected, "cfb8 encryption");

        let mut dec = Cfb8 {
            cipher: Aes128::new(GenericArray::from_slice(&key)),
            iv: iv.try_into().unwrap(),
        };
        dec.decrypt(&mut buf);
        assert_eq!(buf, plain, "cfb8 decryption");
    }

    #[test]
    fn cfb8_is_stateful_across_calls() {
        // Splitting a buffer must give the same bytes as encrypting it whole,
        // or the stream desyncs the moment a packet spans two reads.
        let secret = secret_from([7u8; 16]);
        let data: Vec<u8> = (0..64u8).collect();

        let mut whole = data.clone();
        Cfb8::new(&secret).encrypt(&mut whole);

        let mut split = data.clone();
        let mut c = Cfb8::new(&secret);
        let (a, b) = split.split_at_mut(13);
        c.encrypt(a);
        c.encrypt(b);
        assert_eq!(split, whole);
    }

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let secret = secret_from([0xA5; 16]);
        let original: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut buf = original.clone();
        Cfb8::new(&secret).encrypt(&mut buf);
        assert_ne!(buf, original);
        Cfb8::new(&secret).decrypt(&mut buf);
        assert_eq!(buf, original);
    }

    #[test]
    fn server_hash_matches_mojangs_published_examples() {
        // The three worked examples from Mojang's own authentication notes.
        assert_eq!(sha1_hex(b"Notch"), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
        assert_eq!(sha1_hex(b"jeb_"), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
        assert_eq!(sha1_hex(b"simon"), "88e16a1019277b15d58faf0541e11910eb756f6");
    }

    /// The digest-to-string half of `server_hash`, exercised on its own.
    fn sha1_hex(input: &[u8]) -> String {
        let mut sha = Sha1::new();
        sha.update(input);
        twos_complement_hex(&sha.finalize())
    }

    #[test]
    fn negative_hashes_keep_their_sign_and_drop_leading_zeroes() {
        // High bit set means a negative BigInteger.
        assert!(sha1_hex(b"jeb_").starts_with('-'));
        // "simon" hashes to a value with a leading zero nibble, which Java drops.
        assert_eq!(sha1_hex(b"simon").len(), 39);
    }

    fn hex(s: &[u8]) -> Vec<u8> {
        s.chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }
}
