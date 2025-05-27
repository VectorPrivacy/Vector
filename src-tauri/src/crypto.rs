use crate::rand;
use crate::rand::Rng;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use aes::Aes256;
use aes_gcm::{AesGcm, AeadInPlace, KeyInit};
use generic_array::{GenericArray, typenum::U16};
use argon2::{Argon2, Params, Version};
use chacha20poly1305::{
    aead::Aead,
    ChaCha20Poly1305, Nonce
};

/// Represents encryption parameters
#[derive(Debug)]
pub struct EncryptionParams {
    pub key: String,    // Hex string
    pub nonce: String,  // Hex string
}

/// Generates random encryption parameters (key and nonce)
pub fn generate_encryption_params() -> EncryptionParams {
    let mut rng = rand::thread_rng();
    
    // Generate 32 byte key (for AES-256)
    let key: [u8; 32] = rng.gen();
    // Generate 16 byte nonce (to match 0xChat)
    let nonce: [u8; 16] = rng.gen();
    
    EncryptionParams {
        key: hex::encode(key),
        nonce: hex::encode(nonce),
    }
}

/// Encrypts data using AES-256-GCM with a 16-byte nonce
pub fn encrypt_data(data: &[u8], params: &EncryptionParams) -> Result<Vec<u8>, String> {
    // Decode key and nonce from hex
    let key_bytes = hex::decode(&params.key).unwrap();
    let nonce_bytes = hex::decode(&params.nonce).unwrap();

    // Initialize AES-GCM cipher
    let cipher = AesGcm::<Aes256, U16>::new(
        GenericArray::from_slice(&key_bytes)
    );

    // Prepare nonce
    let nonce = GenericArray::from_slice(&nonce_bytes);

    // Create output buffer
    let mut buffer = data.to_vec();

    // Encrypt in place and get authentication tag
    let tag = cipher
        .encrypt_in_place_detached(nonce, &[], &mut buffer)
        .map_err(|_| String::from("Failed to Encrypt Data"))?;

    // Append the authentication tag to the encrypted data
    buffer.extend_from_slice(tag.as_slice());

    Ok(buffer)
}

/// Hash a password using Argon2id
pub async fn hash_pass(password: String) -> [u8; 32] {
    // 150000 KiB memory size
    let memory = 150000;
    // 10 iterations
    let iterations = 10;
    let params = Params::new(memory, iterations, 1, Some(32)).unwrap();

    // TODO: create a random on-disk salt at first init
    // However, with the nature of this being local software, it won't help a user whom has their system compromised in the first place
    let salt = "vectorvectovectvecvev".as_bytes();

    // Prepare derivation
    let argon = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);
    let mut key: [u8; 32] = [0; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .unwrap();

    key
}

/// Internal function for encryption logic using ChaCha20Poly1305
pub async fn internal_encrypt(input: String, password: Option<String>) -> String {
    // Hash our password with Argon2 and use it as the key
    let key = if password.is_none() { 
        crate::ENCRYPTION_KEY.get().unwrap() 
    } else { 
        &hash_pass(password.unwrap()).await 
    };

    // Generate a random 12-byte nonce
    let mut rng = rand::thread_rng();
    let nonce_bytes: [u8; 12] = rng.gen();
    
    // Create the cipher instance
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .expect("Key should be valid");
    
    // Create the nonce
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    // Encrypt the input
    let ciphertext = cipher
        .encrypt(nonce, input.as_bytes())
        .expect("Encryption should not fail");
    
    // Prepend the nonce to our ciphertext
    let mut buffer = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    buffer.extend_from_slice(&nonce_bytes);
    buffer.extend_from_slice(&ciphertext);

    // Save the Encryption Key locally so that we can continually encrypt data post-login
    if crate::ENCRYPTION_KEY.get().is_none() {
        crate::ENCRYPTION_KEY.set(*key).unwrap();
    }

    // Convert the encrypted bytes to a hex string for safe storage/transmission
    bytes_to_hex_string(&buffer)
}

/// Internal function for decryption logic using ChaCha20Poly1305
pub async fn internal_decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    // Check if we're using a password before we potentially move it
    let has_password = password.is_some();

    // Fast path: If we already have an encryption key and no password is provided, avoid unnecessary work
    let key = if let Some(pass) = password {
        // Only hash the password if we actually have one
        &hash_pass(pass).await
    } else if let Some(cached_key) = crate::ENCRYPTION_KEY.get() {
        // Use cached key
        cached_key
    } else {
        // No key available
        return Err(());
    };

    // Convert hex to bytes - use reference to avoid copying the string
    let encrypted_data = match hex_string_to_bytes(ciphertext.as_str()) {
        bytes if bytes.len() >= 12 => bytes,
        _ => return Err(())
    };
    
    // Extract nonce and encrypted data - use slices to avoid copying data
    let (nonce_bytes, actual_ciphertext) = encrypted_data.split_at(12);
    
    // Create the cipher instance
    let cipher = match ChaCha20Poly1305::new_from_slice(key) {
        Ok(c) => c,
        Err(_) => return Err(())
    };
    
    // Create the nonce and decrypt
    let plaintext = match cipher.decrypt(Nonce::from_slice(nonce_bytes), actual_ciphertext) {
        Ok(pt) => pt,
        Err(_) => return Err(())
    };

    // Cache the key if needed - only set if we came from password path
    if has_password && crate::ENCRYPTION_KEY.get().is_none() {
        // This only happens once after login with password
        let _ = crate::ENCRYPTION_KEY.set(*key); // Ignore result as this is non-critical
    }

    // Convert decrypted bytes to string using unsafe version, because SPEED!
    // SAFETY: The plaintext bytes are guaranteed to be valid UTF-8, making this safe, because:
    // 1. They were originally created from a valid UTF-8 string (typically JSON or plaintext)
    // 2. ChaCha20-Poly1305 authenticated decryption ensures the data is intact
    // 3. The decryption process preserves the exact byte patterns
    unsafe {
        Ok(String::from_utf8_unchecked(plaintext))
    }
}

pub fn decrypt_data(encrypted_data: &[u8], key_hex: &str, nonce_hex: &str) -> Result<Vec<u8>, String> {
    // Verify minimum size requirements
    if encrypted_data.len() < 16 {
        return Err(String::from("Invalid Input"));
    }

    // Decode key and nonce from hex
    let key_bytes = hex::decode(key_hex).unwrap();
    let nonce_bytes = hex::decode(nonce_hex).unwrap();

    // Split input into ciphertext and authentication tag
    let (ciphertext, tag_bytes) = encrypted_data.split_at(encrypted_data.len() - 16);

    // Initialize AES-GCM cipher
    let cipher = AesGcm::<Aes256, U16>::new(
        GenericArray::from_slice(&key_bytes)
    );

    // Prepare nonce and tag
    let nonce = GenericArray::from_slice(&nonce_bytes);
    let tag = aes_gcm::Tag::from_slice(tag_bytes);

    // Create output buffer
    let mut buffer = ciphertext.to_vec();

    // Perform decryption
    let decryption = cipher
        .decrypt_in_place_detached(nonce, &[], &mut buffer, tag);

    // Check that it went well
    if decryption.is_err() {
        return Err(decryption.unwrap_err().to_string());
    }

    Ok(buffer)
}