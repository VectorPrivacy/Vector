//! PIVX Promos Module
//!
//! Implements PIVX promo code generation, balance checking, and sweeping
//! using Blockbook APIs for blockchain interaction.
//!
//! Each promo code is a "single-UTXO wallet" - a human-readable code that
//! derives a private key through iterated SHA256 hashing (12.5M iterations).

use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use ripemd::Ripemd160;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use secp256k1::{Secp256k1, SecretKey, PublicKey, Message};
use tauri::{AppHandle, Runtime, Emitter};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

// ============================================================================
// Constants
// ============================================================================

/// Base58 alphabet (Bitcoin-style, excludes 0/O/I/l for readability)
const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// PIVX mainnet P2PKH address prefix (0x1E = 30)
const PIVX_PUBKEY_ADDRESS_PREFIX: u8 = 30;

/// Blockbook API endpoints (with failover)
const BLOCKBOOK_APIS: &[&str] = &[
    "https://explorer.pivxla.bz",
    "https://explorer.duddino.com",
    "http://zkbitcoin.com",
];

/// Per-request timeout for fast failover (3 seconds)
const BLOCKBOOK_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Number of SHA256 iterations for promo key derivation (PoW security)
const PROMO_KEY_ITERATIONS: u32 = 12_500_000;

/// Balance cache TTL (60 seconds)
const BALANCE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// HTTP client for Blockbook API calls
static PIVX_HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .build()
        .expect("Failed to create HTTP client")
});

/// Balance cache: address -> (balance_piv, last_fetch_time)
static BALANCE_CACHE: Lazy<RwLock<HashMap<String, (f64, Instant)>>> = Lazy::new(|| {
    RwLock::new(HashMap::new())
});

// ============================================================================
// Types
// ============================================================================

/// PIVX Promo representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PivxPromo {
    pub gift_code: String,
    pub address: String,
    pub balance_piv: f64,
    pub status: String,
    pub created_at: u64,
}

/// UTXO from Blockbook API
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Utxo {
    pub txid: String,
    pub vout: u32,
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub value: u64, // satoshis
    #[serde(default)]
    pub confirmations: u32,
    #[serde(default)]
    pub height: Option<u64>,
}

/// Address balance from Blockbook API
#[derive(Debug, Clone, Deserialize)]
#[allow(non_snake_case, dead_code)]
pub struct AddressBalance {
    pub address: String,
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub balance: u64,
    #[serde(deserialize_with = "deserialize_string_to_u64", default)]
    pub unconfirmedBalance: u64,
}

/// Sweep progress for UI updates
#[derive(Debug, Clone, Serialize)]
pub struct SweepProgress {
    pub stage: String,
    pub progress_pct: u8,
    pub message: String,
}

/// Custom deserializer for string-to-u64 (Blockbook returns numbers as strings)
fn deserialize_string_to_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let s: String = String::deserialize(deserializer)?;
    s.parse().map_err(|_| D::Error::custom("invalid number"))
}

// ============================================================================
// Core Cryptographic Functions
// ============================================================================

/// Generate a random 10-character Base58 promo code
/// 10 chars = 58^10 ≈ 430 quadrillion combinations (vs 656M for 5 chars)
/// Combined with 12.5M iterations, this makes brute-forcing impractical
/// Note: Claiming/sweeping supports any code length for backwards compatibility
fn generate_promo_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..10)
        .map(|_| {
            let idx = rng.gen_range(0..BASE58_ALPHABET.len());
            BASE58_ALPHABET[idx] as char
        })
        .collect()
}

/// Derive private key from promo code via iterated SHA256
/// This provides PoW security - 12.5M iterations makes brute-forcing expensive
pub fn derive_privkey_from_code(code: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    let mut hash: [u8; 32] = hasher.finalize().into();

    for _ in 0..PROMO_KEY_ITERATIONS {
        let mut hasher = Sha256::new();
        hasher.update(&hash);
        hash = hasher.finalize().into();
    }

    hash
}

/// Double SHA256 (for checksums)
fn double_sha256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    Sha256::digest(&first).into()
}

/// RIPEMD-160 hash (used in address derivation)
fn ripemd160_hash(data: &[u8]) -> [u8; 20] {
    let mut hasher = Ripemd160::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Base58 encoding
fn base58_encode(data: &[u8]) -> String {
    // Handle leading zeros
    let mut leading_zeros = 0;
    for byte in data {
        if *byte == 0 {
            leading_zeros += 1;
        } else {
            break;
        }
    }

    // Convert to big integer (simple implementation)
    let mut num = data.iter().fold(
        num_bigint::BigUint::from(0u32),
        |acc, &byte| acc * 256u32 + byte as u32,
    );

    let mut result = Vec::new();
    let base = num_bigint::BigUint::from(58u32);

    while num > num_bigint::BigUint::from(0u32) {
        let remainder = (&num % &base).to_u32_digits();
        let idx = if remainder.is_empty() { 0 } else { remainder[0] as usize };
        result.push(BASE58_ALPHABET[idx]);
        num /= &base;
    }

    // Add leading '1's for zero bytes
    for _ in 0..leading_zeros {
        result.push(b'1');
    }

    result.reverse();
    String::from_utf8(result).unwrap_or_default()
}

/// Base58 decoding
fn base58_decode(encoded: &str) -> Result<Vec<u8>, String> {
    let mut num = num_bigint::BigUint::from(0u32);
    let base = num_bigint::BigUint::from(58u32);

    let mut leading_ones = 0;
    for c in encoded.chars() {
        if c == '1' {
            leading_ones += 1;
        } else {
            break;
        }
    }

    for c in encoded.chars() {
        let idx = BASE58_ALPHABET
            .iter()
            .position(|&b| b as char == c)
            .ok_or_else(|| format!("Invalid Base58 character: {}", c))?;
        num = num * &base + idx as u32;
    }

    let mut bytes = num.to_bytes_be();

    // Add leading zeros
    let mut result = vec![0u8; leading_ones];
    result.append(&mut bytes);

    Ok(result)
}

/// Derive PIVX address from private key
pub fn privkey_to_address(privkey: &[u8; 32]) -> Result<String, String> {
    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(privkey)
        .map_err(|e| format!("Invalid private key: {}", e))?;
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);

    // Compressed public key (33 bytes)
    let pubkey_bytes = public_key.serialize();

    // SHA256 then RIPEMD160 (Hash160)
    let sha256_hash = Sha256::digest(&pubkey_bytes);
    let hash160 = ripemd160_hash(&sha256_hash);

    // Prepend version byte
    let mut payload = vec![PIVX_PUBKEY_ADDRESS_PREFIX];
    payload.extend_from_slice(&hash160);

    // Double SHA256 for checksum
    let checksum = double_sha256(&payload);
    payload.extend_from_slice(&checksum[0..4]);

    // Base58 encode
    Ok(base58_encode(&payload))
}

/// Decode PIVX Base58Check address to pubkey hash
fn decode_pivx_address(address: &str) -> Result<[u8; 20], String> {
    let decoded = base58_decode(address)?;
    if decoded.len() != 25 {
        return Err(format!("Invalid address length: {} (expected 25)", decoded.len()));
    }
    if decoded[0] != PIVX_PUBKEY_ADDRESS_PREFIX {
        return Err(format!("Invalid PIVX address prefix: {} (expected {})", decoded[0], PIVX_PUBKEY_ADDRESS_PREFIX));
    }

    // Verify checksum
    let checksum = double_sha256(&decoded[0..21]);
    if &decoded[21..25] != &checksum[0..4] {
        return Err("Invalid address checksum".to_string());
    }

    let mut hash = [0u8; 20];
    hash.copy_from_slice(&decoded[1..21]);
    Ok(hash)
}

// ============================================================================
// Blockbook API Functions
// ============================================================================

/// Check if cached balance is still valid
fn get_cached_balance(address: &str) -> Option<f64> {
    if let Ok(cache) = BALANCE_CACHE.read() {
        if let Some((balance, timestamp)) = cache.get(address) {
            if timestamp.elapsed() < BALANCE_CACHE_TTL {
                return Some(*balance);
            }
        }
    }
    None
}

/// Store balance in cache
fn cache_balance(address: &str, balance: f64) {
    if let Ok(mut cache) = BALANCE_CACHE.write() {
        cache.insert(address.to_string(), (balance, Instant::now()));
    }
}

/// Clear the balance cache (useful after transactions)
pub fn clear_balance_cache() {
    if let Ok(mut cache) = BALANCE_CACHE.write() {
        cache.clear();
    }
}

/// Fetch balance from a single explorer (internal helper)
async fn fetch_balance_from_explorer(api_base: &str, address: &str) -> Result<f64, String> {
    let url = format!("{}/api/v2/address/{}", api_base, address);
    match PIVX_HTTP_CLIENT.get(&url).timeout(BLOCKBOOK_REQUEST_TIMEOUT).send().await {
        Ok(resp) if resp.status().is_success() => {
            let data: AddressBalance = resp.json().await
                .map_err(|e| format!("Failed to parse balance: {}", e))?;
            Ok((data.balance + data.unconfirmedBalance) as f64 / 100_000_000.0)
        }
        Ok(resp) => Err(format!("HTTP {}", resp.status())),
        Err(e) => Err(format!("Network error: {}", e)),
    }
}

/// Fetch balance for an address using parallel explorer queries (first success wins)
/// Uses caching to avoid redundant API calls
pub async fn fetch_balance(address: &str) -> Result<f64, String> {
    // Check cache first
    if let Some(cached) = get_cached_balance(address) {
        return Ok(cached);
    }

    // Query all explorers in parallel, return immediately on first success
    let futures: Vec<_> = BLOCKBOOK_APIS.iter()
        .map(|api| Box::pin(fetch_balance_from_explorer(api, address)))
        .collect();

    match futures_util::future::select_ok(futures).await {
        Ok((balance, _remaining)) => {
            cache_balance(address, balance);
            Ok(balance)
        }
        Err(_) => Err("All Blockbook APIs failed".to_string()),
    }
}

/// Fetch balances for multiple addresses in parallel
/// Distributes addresses across explorers (round-robin) for efficiency:
/// - Address 0 → Explorer 0, Address 1 → Explorer 1, Address 2 → Explorer 2, etc.
/// - If an explorer fails, falls back to trying others
/// Returns a HashMap of address -> balance (failed fetches are omitted)
pub async fn fetch_balances_batch(addresses: &[String]) -> HashMap<String, f64> {
    let mut results = HashMap::new();

    // First, collect cached results and identify addresses needing fetch
    let mut to_fetch: Vec<String> = Vec::new();
    for addr in addresses {
        if let Some(cached) = get_cached_balance(addr) {
            results.insert(addr.clone(), cached);
        } else {
            to_fetch.push(addr.clone());
        }
    }

    if to_fetch.is_empty() {
        return results;
    }

    // Distribute addresses across explorers (round-robin) and fetch in parallel
    let fetch_futures: Vec<_> = to_fetch.iter().enumerate()
        .map(|(i, addr)| {
            let addr = addr.clone();
            let primary_idx = i % BLOCKBOOK_APIS.len();
            async move {
                // Try assigned explorer first (load balanced)
                let primary_api = BLOCKBOOK_APIS[primary_idx];
                if let Ok(balance) = fetch_balance_from_explorer(primary_api, &addr).await {
                    return (addr, Ok(balance));
                }

                // Fallback: try other explorers sequentially
                for (j, api) in BLOCKBOOK_APIS.iter().enumerate() {
                    if j != primary_idx {
                        if let Ok(balance) = fetch_balance_from_explorer(api, &addr).await {
                            return (addr, Ok(balance));
                        }
                    }
                }
                (addr, Err::<f64, &str>("All explorers failed"))
            }
        })
        .collect();

    let fetched = futures_util::future::join_all(fetch_futures).await;

    for (addr, balance_result) in fetched {
        if let Ok(balance) = balance_result {
            cache_balance(&addr, balance);
            results.insert(addr, balance);
        }
    }

    results
}

/// Fetch UTXOs for an address (with failover and 3s timeout per API)
pub async fn fetch_utxos(address: &str) -> Result<Vec<Utxo>, String> {
    for api_base in BLOCKBOOK_APIS {
        let url = format!("{}/api/v2/utxo/{}", api_base, address);
        match PIVX_HTTP_CLIENT.get(&url).timeout(BLOCKBOOK_REQUEST_TIMEOUT).send().await {
            Ok(resp) if resp.status().is_success() => {
                let utxos: Vec<Utxo> = resp.json().await
                    .map_err(|e| format!("Failed to parse UTXOs: {}", e))?;
                return Ok(utxos);
            }
            Ok(resp) => {
                eprintln!("[PIVX] UTXO fetch failed from {}: {}", api_base, resp.status());
            }
            Err(e) => {
                eprintln!("[PIVX] UTXO fetch error from {} (timeout or network): {}", api_base, e);
            }
        }
    }
    Err("All Blockbook APIs failed".to_string())
}

/// Broadcast transaction (with failover and 3s timeout per API)
pub async fn broadcast_tx(tx_hex: &str) -> Result<String, String> {
    for api_base in BLOCKBOOK_APIS {
        let url = format!("{}/api/v2/sendtx/{}", api_base, tx_hex);
        match PIVX_HTTP_CLIENT.get(&url).timeout(BLOCKBOOK_REQUEST_TIMEOUT).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await
                    .map_err(|e| format!("Failed to read response: {}", e))?;
                // Blockbook returns JSON with txid
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(txid) = json.get("result").and_then(|v| v.as_str()) {
                        return Ok(txid.to_string());
                    }
                }
                // Fallback: return trimmed body
                return Ok(body.trim().trim_matches('"').to_string());
            }
            Ok(resp) => {
                let error = resp.text().await.unwrap_or_default();
                eprintln!("[PIVX] Broadcast failed from {}: {}", api_base, error);
            }
            Err(e) => {
                eprintln!("[PIVX] Broadcast error from {} (timeout or network): {}", api_base, e);
            }
        }
    }
    Err("All Blockbook APIs failed to broadcast".to_string())
}

// ============================================================================
// Transaction Building
// ============================================================================

/// Build P2PKH script for output
fn build_p2pkh_script(pubkey_hash: &[u8; 20]) -> Vec<u8> {
    let mut script = Vec::with_capacity(25);
    script.push(0x76); // OP_DUP
    script.push(0xa9); // OP_HASH160
    script.push(0x14); // Push 20 bytes
    script.extend_from_slice(pubkey_hash);
    script.push(0x88); // OP_EQUALVERIFY
    script.push(0xac); // OP_CHECKSIG
    script
}

/// Build and sign a PIVX transaction to sweep UTXOs to destination
pub async fn build_sweep_transaction(
    privkey: &[u8; 32],
    source_address: &str,
    utxos: &[Utxo],
    dest_address: &str,
) -> Result<String, String> {
    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(privkey)
        .map_err(|e| format!("Invalid private key: {}", e))?;
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);

    // Calculate total input value
    let total_input: u64 = utxos.iter().map(|u| u.value).sum();

    // Estimate fee (10 satoshis per byte)
    // Size: ~10 (overhead) + 148 per input + 34 per output
    let estimated_size = 10 + (utxos.len() * 148) + 34;
    let fee = (estimated_size as u64) * 10;

    if total_input <= fee {
        return Err(format!(
            "Balance too low: {} sats, need at least {} sats for fee",
            total_input, fee
        ));
    }

    let output_value = total_input - fee;

    // Decode destination address
    let dest_pubkeyhash = decode_pivx_address(dest_address)?;

    // Get source pubkey hash for scriptPubKey
    let source_pubkeyhash = decode_pivx_address(source_address)?;
    let source_script = build_p2pkh_script(&source_pubkeyhash);

    // Build unsigned transaction
    let mut tx_bytes = Vec::new();

    // Version (4 bytes, little-endian) - PIVX uses version 1
    tx_bytes.extend_from_slice(&1u32.to_le_bytes());

    // Input count (varint)
    tx_bytes.push(utxos.len() as u8);

    // Build inputs (initially unsigned)
    for utxo in utxos {
        // Previous txid (32 bytes, reversed)
        let txid_bytes = hex_string_to_bytes(&utxo.txid);
        tx_bytes.extend(txid_bytes.iter().rev());

        // Previous output index (4 bytes, little-endian)
        tx_bytes.extend_from_slice(&utxo.vout.to_le_bytes());

        // ScriptSig length (0 for now, will be filled during signing)
        tx_bytes.push(0);

        // Sequence (4 bytes)
        tx_bytes.extend_from_slice(&0xFFFFFFFEu32.to_le_bytes());
    }

    // Output count
    tx_bytes.push(1);

    // Output value (8 bytes, little-endian)
    tx_bytes.extend_from_slice(&output_value.to_le_bytes());

    // Output script
    let output_script = build_p2pkh_script(&dest_pubkeyhash);
    tx_bytes.push(output_script.len() as u8);
    tx_bytes.extend_from_slice(&output_script);

    // Locktime (4 bytes)
    tx_bytes.extend_from_slice(&0u32.to_le_bytes());

    // Now sign each input
    let mut signed_tx = Vec::new();

    // Version
    signed_tx.extend_from_slice(&1u32.to_le_bytes());

    // Input count
    signed_tx.push(utxos.len() as u8);

    for (i, utxo) in utxos.iter().enumerate() {
        // Build the signing preimage for this input
        let mut preimage = Vec::new();
        preimage.extend_from_slice(&1u32.to_le_bytes()); // Version

        preimage.push(utxos.len() as u8); // Input count

        for (j, other_utxo) in utxos.iter().enumerate() {
            let other_txid = hex_string_to_bytes(&other_utxo.txid);
            preimage.extend(other_txid.iter().rev());
            preimage.extend_from_slice(&other_utxo.vout.to_le_bytes());

            if i == j {
                // Include scriptPubKey for the input being signed
                preimage.push(source_script.len() as u8);
                preimage.extend_from_slice(&source_script);
            } else {
                // Empty script for other inputs
                preimage.push(0);
            }

            preimage.extend_from_slice(&0xFFFFFFFEu32.to_le_bytes());
        }

        // Output count and output
        preimage.push(1);
        preimage.extend_from_slice(&output_value.to_le_bytes());
        preimage.push(output_script.len() as u8);
        preimage.extend_from_slice(&output_script);

        // Locktime
        preimage.extend_from_slice(&0u32.to_le_bytes());

        // SIGHASH_ALL (4 bytes)
        preimage.extend_from_slice(&1u32.to_le_bytes());

        // Double SHA256 the preimage
        let sighash = double_sha256(&preimage);

        // Sign
        let message = Message::from_digest_slice(&sighash)
            .map_err(|e| format!("Failed to create message: {}", e))?;
        let signature = secp.sign_ecdsa(&message, &secret_key);
        let mut sig_bytes = signature.serialize_der().to_vec();
        sig_bytes.push(0x01); // SIGHASH_ALL

        // Build scriptSig: <sig> <pubkey>
        let pubkey_bytes = public_key.serialize();
        let script_sig_len = 1 + sig_bytes.len() + 1 + pubkey_bytes.len();

        // Write input to signed tx
        let txid_bytes = hex_string_to_bytes(&utxo.txid);
        signed_tx.extend(txid_bytes.iter().rev());
        signed_tx.extend_from_slice(&utxo.vout.to_le_bytes());

        // ScriptSig
        signed_tx.push(script_sig_len as u8);
        signed_tx.push(sig_bytes.len() as u8);
        signed_tx.extend_from_slice(&sig_bytes);
        signed_tx.push(pubkey_bytes.len() as u8);
        signed_tx.extend_from_slice(&pubkey_bytes);

        signed_tx.extend_from_slice(&0xFFFFFFFEu32.to_le_bytes());
    }

    // Output
    signed_tx.push(1);
    signed_tx.extend_from_slice(&output_value.to_le_bytes());
    signed_tx.push(output_script.len() as u8);
    signed_tx.extend_from_slice(&output_script);

    // Locktime
    signed_tx.extend_from_slice(&0u32.to_le_bytes());

    Ok(bytes_to_hex_string(&signed_tx))
}

// ============================================================================
// Database Functions
// ============================================================================

/// Check if a promo code already exists in the database
fn promo_code_exists<R: Runtime>(handle: &AppHandle<R>, code: &str) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pivx_promos WHERE gift_code = ?1",
        rusqlite::params![code],
        |row| row.get(0),
    ).unwrap_or(false);
    Ok(exists)
}

/// Save a promo to the database
async fn save_promo<R: Runtime>(
    handle: &AppHandle<R>,
    gift_code: &str,
    address: &str,
    privkey: &[u8; 32],
) -> Result<(), String> {
    // Encrypt private key
    let privkey_hex = bytes_to_hex_string(privkey);
    let privkey_encrypted = crate::crypto::internal_encrypt(privkey_hex, None).await;

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let conn = crate::account_manager::get_db_connection_guard(handle)?;
    conn.execute(
        "INSERT INTO pivx_promos (gift_code, address, privkey_encrypted, created_at, status)
         VALUES (?1, ?2, ?3, ?4, 'active')",
        rusqlite::params![gift_code, address, privkey_encrypted, created_at],
    ).map_err(|e| format!("Failed to store promo: {}", e))?;

    Ok(())
}

/// Decrypt an encrypted private key string and convert to bytes
/// Used for retrieving stored promo privkeys without re-deriving (avoids PoW cost)
async fn decrypt_privkey_bytes(privkey_encrypted: String) -> Result<[u8; 32], String> {
    let privkey_hex = crate::crypto::internal_decrypt(privkey_encrypted, None)
        .await
        .map_err(|_| "Failed to decrypt private key".to_string())?;

    hex_string_to_bytes(&privkey_hex)
        .try_into()
        .map_err(|_| "Invalid private key format/length".to_string())
}

/// Promo with decrypted private key ready for signing
pub struct DecryptedPromo {
    pub gift_code: String,
    pub address: String,
    pub privkey: [u8; 32],
    pub amount_piv: f64,
}

/// Get all active promos with positive balances, with decrypted private keys
/// This queries the DB once and decrypts all keys, avoiding repeated PoW derivation
async fn get_active_promos_with_keys<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Vec<DecryptedPromo>, String> {
    // Query all active promos with balances
    let encrypted_promos: Vec<(String, String, String, f64)> = {
        let conn = crate::account_manager::get_db_connection_guard(handle)?;
        let result = {
            let mut stmt = conn.prepare(
                "SELECT gift_code, address, privkey_encrypted, COALESCE(amount_piv, 0)
                 FROM pivx_promos WHERE status = 'active' AND COALESCE(amount_piv, 0) > 0
                 ORDER BY amount_piv DESC"
            ).map_err(|e| format!("DB error: {}", e))?;

            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            }).map_err(|e| format!("Query error: {}", e))?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("Row error: {}", e))?
        };        result
    };

    // Decrypt all private keys
    let mut promos = Vec::with_capacity(encrypted_promos.len());
    for (gift_code, address, privkey_encrypted, amount_piv) in encrypted_promos {
        let privkey = decrypt_privkey_bytes(privkey_encrypted).await?;
        promos.push(DecryptedPromo { gift_code, address, privkey, amount_piv });
    }

    Ok(promos)
}

/// Update promo status in database
/// For 'claimed' or 'sent' status, the promo is deleted (funds are gone)
fn update_promo_status<R: Runtime>(
    handle: &AppHandle<R>,
    gift_code: &str,
    status: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    // For claimed/sent promos, delete them entirely (no need to track spent promos)
    if status == "claimed" || status == "sent" {
        conn.execute(
            "DELETE FROM pivx_promos WHERE gift_code = ?1",
            rusqlite::params![gift_code],
        ).map_err(|e| format!("Failed to delete promo: {}", e))?;
    } else {
        conn.execute(
            "UPDATE pivx_promos SET status = ?1 WHERE gift_code = ?2",
            rusqlite::params![status, gift_code],
        ).map_err(|e| format!("Failed to update promo status: {}", e))?;
    }
    Ok(())
}

/// Find a reusable zero-balance promo for deposits
/// Returns (gift_code, address, created_at) if found, None otherwise
/// This helps avoid accumulating unused addresses
fn find_reusable_promo<R: Runtime>(handle: &AppHandle<R>) -> Result<Option<(String, String, u64)>, String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    // Find promos that:
    // 1. Are active status (not 'sent' or 'claimed')
    // 2. Have zero or null balance (never received funds)
    // Order by oldest first to reuse legacy unused addresses
    let result: Option<(String, String, u64)> = conn.query_row(
        "SELECT gift_code, address, created_at
         FROM pivx_promos
         WHERE status = 'active'
           AND COALESCE(amount_piv, 0) = 0
         ORDER BY created_at ASC
         LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).ok();
    Ok(result)
}

/// Find or create a promo for deposits (reuses zero-balance unused promos)
async fn get_or_create_deposit_promo<R: Runtime>(handle: &AppHandle<R>) -> Result<PivxPromo, String> {
    // First try to find a reusable promo
    if let Some((gift_code, address, created_at)) = find_reusable_promo(handle)? {
        // Verify it's actually zero balance on-chain (paranoia check)
        let balance = fetch_balance(&address).await.unwrap_or(0.0);
        if balance == 0.0 {
            return Ok(PivxPromo {
                gift_code,
                address,
                balance_piv: 0.0,
                status: "active".to_string(),
                created_at,
            });
        }
        // If it has balance, update the DB and continue to create new promo
        let conn = crate::account_manager::get_db_connection_guard(handle)?;
        let _ = conn.execute(
            "UPDATE pivx_promos SET amount_piv = ?1 WHERE gift_code = ?2",
            rusqlite::params![balance, gift_code],
        );    }

    // No reusable promo found, create a new one
    let gift_code = loop {
        let code = generate_promo_code();
        if !promo_code_exists(handle, &code)? {
            break code;
        }
    };

    let privkey = derive_privkey_from_code(&gift_code);
    let address = privkey_to_address(&privkey)?;

    save_promo(handle, &gift_code, &address, &privkey).await?;

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    Ok(PivxPromo {
        gift_code,
        address,
        balance_piv: 0.0,
        status: "active".to_string(),
        created_at,
    })
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Create a new promo code and store it encrypted
/// For deposits, this now reuses zero-balance unused promos to prevent address buildup
#[tauri::command]
pub async fn pivx_create_promo<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<PivxPromo, String> {
    // Use the optimized get_or_create function that reuses unused promos
    get_or_create_deposit_promo(&handle).await
}

/// Get balance for a promo code (derives address from code)
#[tauri::command]
pub async fn pivx_get_promo_balance(
    gift_code: String,
) -> Result<f64, String> {
    // Derive address from code
    let privkey = derive_privkey_from_code(&gift_code);
    let address = privkey_to_address(&privkey)?;

    // Fetch balance from API
    fetch_balance(&address).await
}

/// Get wallet total balance (sum of all active promos)
#[tauri::command]
pub async fn pivx_get_wallet_balance<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<f64, String> {
    // Fetch addresses in a separate scope to release DB connection before await
    let addresses: Vec<String> = {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let mut stmt = conn.prepare(
            "SELECT address FROM pivx_promos WHERE status = 'active'"
        ).map_err(|e| format!("Query failed: {}", e))?;

        let result: Vec<String> = stmt.query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to fetch addresses: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        drop(stmt);        result
    };

    // Fetch balances (sequentially to avoid rate limiting)
    let mut total = 0.0;
    for address in addresses {
        if let Ok(balance) = fetch_balance(&address).await {
            total += balance;
        }
    }

    Ok(total)
}

/// List all promos for the wallet
#[tauri::command]
pub async fn pivx_list_promos<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<Vec<PivxPromo>, String> {
    let conn = crate::account_manager::get_db_connection_guard(&handle)?;

    let mut stmt = conn.prepare(
        "SELECT gift_code, address, amount_piv, status, created_at
         FROM pivx_promos ORDER BY created_at DESC"
    ).map_err(|e| format!("Query failed: {}", e))?;

    let promos: Vec<PivxPromo> = stmt.query_map([], |row| {
        Ok(PivxPromo {
            gift_code: row.get(0)?,
            address: row.get(1)?,
            balance_piv: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            status: row.get(3)?,
            created_at: row.get(4)?,
        })
    }).map_err(|e| format!("Failed to fetch promos: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    drop(stmt);
    Ok(promos)
}

/// Sweep a promo code to destination address
#[tauri::command]
pub async fn pivx_sweep_promo<R: Runtime>(
    handle: AppHandle<R>,
    gift_code: String,
    dest_address: String,
) -> Result<String, String> {
    // Validate destination address
    decode_pivx_address(&dest_address)?;

    // Derive private key and address
    let privkey = derive_privkey_from_code(&gift_code);
    let address = privkey_to_address(&privkey)?;

    // Emit progress: fetching UTXOs
    let _ = handle.emit("pivx_sweep_progress", SweepProgress {
        stage: "fetching_utxos".to_string(),
        progress_pct: 10,
        message: "Fetching UTXOs...".to_string(),
    });

    // Fetch UTXOs
    let utxos = fetch_utxos(&address).await?;
    if utxos.is_empty() {
        return Err("No UTXOs to sweep".to_string());
    }

    // Emit progress: building transaction
    let _ = handle.emit("pivx_sweep_progress", SweepProgress {
        stage: "building_tx".to_string(),
        progress_pct: 40,
        message: "Building transaction...".to_string(),
    });

    // Build and sign transaction
    let tx_hex = build_sweep_transaction(&privkey, &address, &utxos, &dest_address).await?;

    // Emit progress: broadcasting
    let _ = handle.emit("pivx_sweep_progress", SweepProgress {
        stage: "broadcasting".to_string(),
        progress_pct: 70,
        message: "Broadcasting transaction...".to_string(),
    });

    // Broadcast
    let txid = broadcast_tx(&tx_hex).await?;

    // Update promo status if it's in our database
    let _ = update_promo_status(&handle, &gift_code, "claimed");

    // Emit progress: complete
    let _ = handle.emit("pivx_sweep_progress", SweepProgress {
        stage: "complete".to_string(),
        progress_pct: 100,
        message: format!("Swept! TXID: {}...", &txid[0..16.min(txid.len())]),
    });

    Ok(txid)
}

/// Set user's personal PIVX receiving address (empty string clears it)
#[tauri::command]
pub fn pivx_set_wallet_address<R: Runtime>(
    handle: AppHandle<R>,
    address: String,
) -> Result<(), String> {
    // Validate address format only if not empty
    if !address.is_empty() {
        decode_pivx_address(&address)?;
    }

    crate::db::set_sql_setting(handle, "pivx_wallet_address".to_string(), address)
}

/// Get user's personal PIVX receiving address
#[tauri::command]
pub fn pivx_get_wallet_address<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<Option<String>, String> {
    crate::db::get_sql_setting(handle, "pivx_wallet_address".to_string())
}

/// Claim a promo code from a message
/// If auto-withdraw address is set, sweeps directly to that address
/// Otherwise, creates internal promo and sweeps to it
#[tauri::command]
pub async fn pivx_claim_from_message<R: Runtime>(
    handle: AppHandle<R>,
    gift_code: String,
) -> Result<serde_json::Value, String> {
    // Check balance first
    let balance = pivx_get_promo_balance(gift_code.clone()).await?;
    if balance <= 0.0 {
        return Err("This promo code has already been claimed or has no balance".to_string());
    }

    // Check if auto-withdraw address is set (filter out empty strings)
    let auto_withdraw_address = crate::db::get_sql_setting(handle.clone(), "pivx_wallet_address".to_string())
        .ok()
        .flatten()
        .filter(|s| !s.is_empty());

    if let Some(dest_address) = auto_withdraw_address {
        // Auto-withdraw mode: sweep directly to external address
        let txid = pivx_sweep_promo(handle.clone(), gift_code, dest_address).await?;

        Ok(serde_json::json!({
            "txid": txid,
            "amount_piv": balance,
            "auto_withdrawn": true,
        }))
    } else {
        // In-wallet mode: create internal promo and sweep to it
        let new_code = loop {
            let code = generate_promo_code();
            if !promo_code_exists(&handle, &code)? {
                break code;
            }
        };

        // Derive keys for the new promo
        let new_privkey = derive_privkey_from_code(&new_code);
        let new_address = privkey_to_address(&new_privkey)?;

        // Save the new promo to our wallet (with 0 balance initially)
        save_promo(&handle, &new_code, &new_address, &new_privkey).await?;

        // Sweep from the claimed promo to our new promo's address
        let txid = pivx_sweep_promo(handle.clone(), gift_code, new_address).await?;

        // Update the new promo's amount in the database
        {
            let conn = crate::account_manager::get_db_connection_guard(&handle)?;
            let _ = conn.execute(
                "UPDATE pivx_promos SET amount_piv = ?1 WHERE gift_code = ?2",
                rusqlite::params![balance, new_code],
            );
        }

        Ok(serde_json::json!({
            "txid": txid,
            "amount_piv": balance,
            "new_promo_code": new_code,
            "auto_withdrawn": false,
        }))
    }
}

/// Import an external promo code (add to our wallet)
#[tauri::command]
pub async fn pivx_import_promo<R: Runtime>(
    handle: AppHandle<R>,
    gift_code: String,
) -> Result<PivxPromo, String> {
    // Check if already exists
    if promo_code_exists(&handle, &gift_code)? {
        return Err("This promo code is already in your wallet".to_string());
    }

    // Derive keys
    let privkey = derive_privkey_from_code(&gift_code);
    let address = privkey_to_address(&privkey)?;

    // Check balance
    let balance = fetch_balance(&address).await.unwrap_or(0.0);

    // Store in database
    save_promo(&handle, &gift_code, &address, &privkey).await?;

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    Ok(PivxPromo {
        gift_code,
        address,
        balance_piv: balance,
        status: "active".to_string(),
        created_at,
    })
}

/// Refresh balances for all active promos
/// Uses parallel batch fetching and caching for performance
/// Pass force_refresh=true to bypass cache (e.g., after a transaction)
#[tauri::command]
pub async fn pivx_refresh_balances<R: Runtime>(
    handle: AppHandle<R>,
    force_refresh: Option<bool>,
) -> Result<Vec<PivxPromo>, String> {
    // Clear cache if forced refresh
    if force_refresh.unwrap_or(false) {
        clear_balance_cache();
    }

    // Fetch promos in a separate scope to release DB connection before await
    let promos: Vec<(String, String, String, u64)> = {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let mut stmt = conn.prepare(
            "SELECT gift_code, address, status, created_at FROM pivx_promos WHERE status = 'active'"
        ).map_err(|e| format!("Query failed: {}", e))?;

        let result: Vec<(String, String, String, u64)> = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u64>(3)?,
            ))
        }).map_err(|e| format!("Failed to fetch promos: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        drop(stmt);        result
    };

    if promos.is_empty() {
        return Ok(Vec::new());
    }

    // Collect all addresses for batch fetching
    let addresses: Vec<String> = promos.iter().map(|(_, addr, _, _)| addr.clone()).collect();

    // Fetch all balances in parallel batches (much faster than sequential)
    let balances = fetch_balances_batch(&addresses).await;

    // Build results and update database
    let mut results = Vec::new();

    // Batch update all balances in database
    {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        for (gift_code, address, status, created_at) in &promos {
            let balance = balances.get(address).copied().unwrap_or(0.0);

            let _ = conn.execute(
                "UPDATE pivx_promos SET amount_piv = ?1 WHERE gift_code = ?2",
                rusqlite::params![balance, gift_code],
            );

            results.push(PivxPromo {
                gift_code: gift_code.clone(),
                address: address.clone(),
                balance_piv: balance,
                status: status.clone(),
                created_at: *created_at,
            });
        }    }

    Ok(results)
}

// We need num-bigint for Base58 encoding
use num_bigint;

// ============================================================================
// Send PIVX Payment via Chat
// ============================================================================

/// Send a PIVX payment to a chat using coin selection
/// Merges existing promos as needed and creates a promo with the exact amount
#[tauri::command]
pub async fn pivx_send_payment<R: Runtime>(
    handle: AppHandle<R>,
    receiver: String,
    amount_piv: f64,
) -> Result<String, String> {
    use nostr_sdk::prelude::*;
    use std::borrow::Cow;

    // Validate amount
    if amount_piv <= 0.0 {
        return Err("Amount must be greater than 0".to_string());
    }

    let amount_sats = (amount_piv * 100_000_000.0) as u64;

    // Get all active promos with decrypted keys
    let promos = get_active_promos_with_keys(&handle).await?;
    if promos.is_empty() {
        return Err("No funds available to send".to_string());
    }

    // Calculate total available
    let total_available: f64 = promos.iter().map(|p| p.amount_piv).sum();
    let total_available_sats = (total_available * 100_000_000.0) as u64;

    if total_available_sats < amount_sats {
        return Err(format!(
            "Insufficient funds: have {:.8} PIV, need {:.8} PIV",
            total_available, amount_piv
        ));
    }

    // Coin selection: select promos until we have enough (largest first)
    let mut selected_promos: Vec<(String, String, [u8; 32])> = Vec::new();
    let mut selected_total_sats: u64 = 0;

    for promo in &promos {
        if selected_total_sats >= amount_sats {
            break;
        }
        selected_promos.push((promo.gift_code.clone(), promo.address.clone(), promo.privkey));
        selected_total_sats += (promo.amount_piv * 100_000_000.0) as u64;
    }

    // Fetch UTXOs for all selected promos
    let mut inputs: Vec<WithdrawInput> = Vec::new();
    for (gift_code, address, privkey) in &selected_promos {
        let utxos = fetch_utxos(address).await?;
        if !utxos.is_empty() {
            inputs.push(WithdrawInput {
                utxos,
                privkey: *privkey,
                address: address.clone(),
                gift_code: gift_code.clone(),
            });
        }
    }

    if inputs.is_empty() {
        return Err("No UTXOs found in selected promos".to_string());
    }

    // Generate the send promo (will hold exact amount for recipient)
    let send_promo = {
        let code = loop {
            let c = generate_promo_code();
            if !promo_code_exists(&handle, &c)? {
                break c;
            }
        };
        let privkey = derive_privkey_from_code(&code);
        let address = privkey_to_address(&privkey)?;
        (code, address, privkey)
    };

    // Generate change promo
    let change_promo = {
        let code = loop {
            let c = generate_promo_code();
            if !promo_code_exists(&handle, &c)? && c != send_promo.0 {
                break c;
            }
        };
        let privkey = derive_privkey_from_code(&code);
        let address = privkey_to_address(&privkey)?;
        (code, address, privkey)
    };

    // Build transaction: send exact amount to send_promo, remainder to change_promo
    let (tx_hex, change_sats) = build_withdrawal_transaction(
        &inputs,
        &send_promo.1,  // destination = send promo address
        amount_sats,
        Some(&change_promo.1),  // change = change promo address
    ).await?;

    // Broadcast transaction
    let _txid = broadcast_tx(&tx_hex).await?;

    // Delete spent promos
    for input in &inputs {
        let _ = update_promo_status(&handle, &input.gift_code, "claimed");
    }

    // Save the send promo (it now has the exact amount)
    save_promo(&handle, &send_promo.0, &send_promo.1, &send_promo.2).await?;
    {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let _ = conn.execute(
            "UPDATE pivx_promos SET amount_piv = ?1 WHERE gift_code = ?2",
            rusqlite::params![amount_piv, send_promo.0],
        );    }

    // Save change promo if there's change
    let change_piv = change_sats as f64 / 100_000_000.0;
    if change_sats > 0 {
        save_promo(&handle, &change_promo.0, &change_promo.1, &change_promo.2).await?;
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let _ = conn.execute(
            "UPDATE pivx_promos SET amount_piv = ?1 WHERE gift_code = ?2",
            rusqlite::params![change_piv, change_promo.0],
        );    }

    // Now send the funded promo via Nostr
    let client = crate::NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;

    let content = serde_json::json!({
        "amount_piv": amount_piv,
    }).to_string();

    let event_id = if receiver.starts_with("npub1") {
        let receiver_pubkey = PublicKey::from_bech32(&receiver).map_err(|e| e.to_string())?;

        let rumor = EventBuilder::new(Kind::ApplicationSpecificData, &content)
            .tag(Tag::custom(TagKind::d(), vec!["pivx-payment"]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("gift-code")), vec![&send_promo.0]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("amount")), vec![&amount_sats.to_string()]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("address")), vec![&send_promo.1]))
            .tag(Tag::public_key(receiver_pubkey))
            .build(my_public_key);

        let event_id = rumor.id.ok_or("Failed to get event ID")?.to_hex();

        crate::inbox_relays::send_gift_wrap(client, &receiver_pubkey, rumor.clone(), [])
            .await
            .map_err(|e| format!("Failed to send payment: {}", e))?;

        client
            .gift_wrap(&my_public_key, rumor, [])
            .await
            .map_err(|e| format!("Failed to send self-copy: {}", e))?;

        event_id
    } else {
        let rumor = EventBuilder::new(Kind::ApplicationSpecificData, &content)
            .tag(Tag::custom(TagKind::d(), vec!["pivx-payment"]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("gift-code")), vec![&send_promo.0]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("amount")), vec![&amount_sats.to_string()]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("address")), vec![&send_promo.1]))
            .build(my_public_key);

        let event_id = rumor.id.ok_or("Failed to get event ID")?.to_hex();

        crate::mls::send_mls_message(&receiver, rumor, None).await?;

        event_id
    };

    // Delete sent promo from our DB (funds are transferred to recipient)
    {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let _ = conn.execute(
            "DELETE FROM pivx_promos WHERE gift_code = ?1",
            rusqlite::params![send_promo.0],
        );    }

    // Save payment event for persistence
    let stored_event = crate::stored_event::StoredEventBuilder::new()
        .id(&event_id)
        .kind(crate::stored_event::event_kind::APPLICATION_SPECIFIC)
        .content(&content)
        .tags(vec![
            vec!["d".to_string(), "pivx-payment".to_string()],
            vec!["gift-code".to_string(), send_promo.0.clone()],
            vec!["amount".to_string(), amount_sats.to_string()],
            vec!["address".to_string(), send_promo.1.clone()],
        ])
        .created_at(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0))
        .mine(true)
        .npub(Some(my_public_key.to_bech32().unwrap_or_default()))
        .build();
    let event_timestamp = stored_event.created_at;
    let _ = crate::db::save_pivx_payment_event(&handle, &receiver, stored_event).await;

    // Emit event to frontend
    handle.emit("pivx_payment_received", serde_json::json!({
        "conversation_id": receiver,
        "gift_code": send_promo.0,
        "amount_piv": amount_piv,
        "address": send_promo.1,
        "message_id": event_id,
        "sender": my_public_key.to_bech32().unwrap_or_default(),
        "is_mine": true,
        "at": event_timestamp * 1000,
    })).map_err(|e| format!("Failed to emit event: {}", e))?;

    Ok(event_id)
}

/// Send an existing promo code to a chat (quick send - whole promo)
#[tauri::command]
pub async fn pivx_send_existing_promo<R: Runtime>(
    handle: AppHandle<R>,
    receiver: String,
    gift_code: String,
) -> Result<String, String> {
    use nostr_sdk::prelude::*;
    use std::borrow::Cow;

    // Verify the promo exists and get its balance
    let promo_data: (String, f64) = {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let result = conn.query_row(
            "SELECT address, COALESCE(amount_piv, 0) FROM pivx_promos WHERE gift_code = ?1",
            rusqlite::params![gift_code],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
        ).map_err(|_| "Promo code not found")?;        result
    };

    let (address, amount_piv) = promo_data;

    if amount_piv <= 0.0 {
        return Err("This promo has no balance".to_string());
    }

    // Get Nostr client
    let client = crate::NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;

    // Build content JSON
    let content = serde_json::json!({
        "amount_piv": amount_piv,
    }).to_string();

    // Convert amount to satoshis for the tag
    let amount_sats = (amount_piv * 100_000_000.0) as u64;

    // Build and send the PIVX payment rumor
    let event_id = if receiver.starts_with("npub1") {
        // Direct message - send via gift wrap
        let receiver_pubkey = PublicKey::from_bech32(&receiver).map_err(|e| e.to_string())?;

        // Build the PIVX payment rumor with p tag for recipient (needed for DM routing)
        // Include address tag so recipients can check balance without deriving keys
        let rumor = EventBuilder::new(Kind::ApplicationSpecificData, &content)
            .tag(Tag::custom(TagKind::d(), vec!["pivx-payment"]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("gift-code")), vec![&gift_code]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("amount")), vec![&amount_sats.to_string()]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("address")), vec![&address]))
            .tag(Tag::public_key(receiver_pubkey))
            .build(my_public_key);

        let event_id = rumor.id.ok_or("Failed to get event ID")?.to_hex();

        // Send to receiver (routed to their inbox relays if available)
        crate::inbox_relays::send_gift_wrap(client, &receiver_pubkey, rumor.clone(), [])
            .await
            .map_err(|e| format!("Failed to send payment: {}", e))?;

        // Send to ourselves for recovery
        client
            .gift_wrap(&my_public_key, rumor, [])
            .await
            .map_err(|e| format!("Failed to send self-copy: {}", e))?;

        event_id
    } else {
        // MLS group - build rumor without p tag but include address for balance checks
        let rumor = EventBuilder::new(Kind::ApplicationSpecificData, &content)
            .tag(Tag::custom(TagKind::d(), vec!["pivx-payment"]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("gift-code")), vec![&gift_code]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("amount")), vec![&amount_sats.to_string()]))
            .tag(Tag::custom(TagKind::Custom(Cow::Borrowed("address")), vec![&address]))
            .build(my_public_key);

        let event_id = rumor.id.ok_or("Failed to get event ID")?.to_hex();

        // Send via MLS
        crate::mls::send_mls_message(&receiver, rumor, None).await?;

        event_id
    };

    // Delete sent promo from DB (funds are gone, no need to track it)
    {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let _ = conn.execute(
            "DELETE FROM pivx_promos WHERE gift_code = ?1",
            rusqlite::params![gift_code],
        );    }

    // Save PIVX payment event to database for persistence
    let stored_event = crate::stored_event::StoredEventBuilder::new()
        .id(&event_id)
        .kind(crate::stored_event::event_kind::APPLICATION_SPECIFIC)
        .content(&content)
        .tags(vec![
            vec!["d".to_string(), "pivx-payment".to_string()],
            vec!["gift-code".to_string(), gift_code.clone()],
            vec!["amount".to_string(), amount_sats.to_string()],
            vec!["address".to_string(), address.clone()],
        ])
        .created_at(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0))
        .mine(true)
        .npub(Some(my_public_key.to_bech32().unwrap_or_default()))
        .build();
    let event_timestamp = stored_event.created_at;
    let _ = crate::db::save_pivx_payment_event(&handle, &receiver, stored_event).await;

    // Emit event to frontend so our own send shows up in chat immediately
    handle.emit("pivx_payment_received", serde_json::json!({
        "conversation_id": receiver,
        "gift_code": gift_code,
        "amount_piv": amount_piv,
        "address": address,
        "message_id": event_id,
        "sender": my_public_key.to_bech32().unwrap_or_default(),
        "is_mine": true,
        "at": event_timestamp * 1000,
    })).map_err(|e| format!("Failed to emit event: {}", e))?;

    Ok(event_id)
}

/// Get all PIVX payments for a chat (for loading history)
#[tauri::command]
pub async fn pivx_get_chat_payments<R: Runtime>(
    handle: AppHandle<R>,
    conversation_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let events = crate::db::get_pivx_payments_for_chat(&handle, &conversation_id).await?;

    // Convert StoredEvents to frontend-friendly format
    let payments: Vec<serde_json::Value> = events.iter().map(|event| {
        // Extract gift code from tags
        let gift_code = event.tags.iter()
            .find(|tag| tag.len() >= 2 && tag[0] == "gift-code")
            .and_then(|tag| tag.get(1))
            .cloned()
            .unwrap_or_default();

        // Extract amount from tags (in satoshis, convert to PIV)
        let amount_str = event.tags.iter()
            .find(|tag| tag.len() >= 2 && tag[0] == "amount")
            .and_then(|tag| tag.get(1))
            .map(|s| s.as_str())
            .unwrap_or("0");
        let amount_piv = amount_str.parse::<u64>().unwrap_or(0) as f64 / 100_000_000.0;

        // Extract address from tags (for balance checking)
        let address = event.tags.iter()
            .find(|tag| tag.len() >= 2 && tag[0] == "address")
            .and_then(|tag| tag.get(1))
            .cloned();

        // Parse optional message from content JSON
        let message = serde_json::from_str::<serde_json::Value>(&event.content)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str().map(String::from)));

        serde_json::json!({
            "message_id": event.id,
            "gift_code": gift_code,
            "amount_piv": amount_piv,
            "address": address,
            "message": message,
            "sender": event.npub,
            "is_mine": event.mine,
            "at": event.created_at * 1000, // Convert to ms for JS
        })
    }).collect();

    Ok(payments)
}

/// Check balance of a PIVX address (for frontend balance checking)
/// Set force=true to bypass cache (useful after sending a tx)
#[tauri::command]
pub async fn pivx_check_address_balance(
    address: String,
    force: Option<bool>,
) -> Result<f64, String> {
    // If force=true, clear cache entry for this address first
    if force.unwrap_or(false) {
        if let Ok(mut cache) = BALANCE_CACHE.write() {
            cache.remove(&address);
        }
    }
    fetch_balance(&address).await
}

// ============================================================================
// Withdrawal Functions (Coin Control)
// ============================================================================

/// Input source for multi-input transaction
struct WithdrawInput {
    utxos: Vec<Utxo>,
    privkey: [u8; 32],
    address: String,
    gift_code: String,
}

/// Build a multi-input withdrawal transaction with optional change output
async fn build_withdrawal_transaction(
    inputs: &[WithdrawInput],
    dest_address: &str,
    amount_sats: u64,
    change_address: Option<&str>,
) -> Result<(String, u64), String> {
    let secp = Secp256k1::new();

    // Calculate total input value
    let total_input: u64 = inputs.iter()
        .flat_map(|i| i.utxos.iter())
        .map(|u| u.value)
        .sum();

    // Count total UTXOs
    let total_utxo_count: usize = inputs.iter().map(|i| i.utxos.len()).sum();

    // Estimate fee (10 satoshis per byte)
    // Size: ~10 (overhead) + 148 per input + 34 per output
    let num_outputs = if change_address.is_some() { 2 } else { 1 };
    let estimated_size = 10 + (total_utxo_count * 148) + (34 * num_outputs);
    let fee = (estimated_size as u64) * 10;

    if total_input < amount_sats + fee {
        return Err(format!(
            "Insufficient funds: have {} sats, need {} + {} fee",
            total_input, amount_sats, fee
        ));
    }

    let change_amount = total_input - amount_sats - fee;

    // Decode destination address
    let dest_pubkeyhash = decode_pivx_address(dest_address)?;
    let dest_script = build_p2pkh_script(&dest_pubkeyhash);

    // Decode change address if present
    let change_script = if let Some(change_addr) = change_address {
        if change_amount > 0 {
            let change_pubkeyhash = decode_pivx_address(change_addr)?;
            Some(build_p2pkh_script(&change_pubkeyhash))
        } else {
            None
        }
    } else {
        None
    };

    // Flatten all UTXOs with their corresponding private keys and scripts
    let mut all_inputs: Vec<(&Utxo, &[u8; 32], Vec<u8>)> = Vec::new();
    for input in inputs {
        let source_pubkeyhash = decode_pivx_address(&input.address)?;
        let source_script = build_p2pkh_script(&source_pubkeyhash);
        for utxo in &input.utxos {
            all_inputs.push((utxo, &input.privkey, source_script.clone()));
        }
    }

    // Build outputs
    let has_change = change_script.is_some() && change_amount > 0;
    let output_count = if has_change { 2u8 } else { 1u8 };

    // Now sign each input
    let mut signed_tx = Vec::new();

    // Version
    signed_tx.extend_from_slice(&1u32.to_le_bytes());

    // Input count
    signed_tx.push(all_inputs.len() as u8);

    for (i, (utxo, privkey, _)) in all_inputs.iter().enumerate() {
        let secret_key = SecretKey::from_slice(*privkey)
            .map_err(|e| format!("Invalid private key: {}", e))?;
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);

        // Build the signing preimage for this input
        let mut preimage = Vec::new();
        preimage.extend_from_slice(&1u32.to_le_bytes()); // Version

        preimage.push(all_inputs.len() as u8); // Input count

        for (j, (other_utxo, _, other_script)) in all_inputs.iter().enumerate() {
            let other_txid = hex_string_to_bytes(&other_utxo.txid);
            preimage.extend(other_txid.iter().rev());
            preimage.extend_from_slice(&other_utxo.vout.to_le_bytes());

            if i == j {
                // Include scriptPubKey for the input being signed
                preimage.push(other_script.len() as u8);
                preimage.extend_from_slice(other_script);
            } else {
                // Empty script for other inputs
                preimage.push(0);
            }

            preimage.extend_from_slice(&0xFFFFFFFEu32.to_le_bytes());
        }

        // Output count and outputs
        preimage.push(output_count);

        // Main output (destination)
        preimage.extend_from_slice(&amount_sats.to_le_bytes());
        preimage.push(dest_script.len() as u8);
        preimage.extend_from_slice(&dest_script);

        // Change output (if any)
        if let Some(ref cs) = change_script {
            if change_amount > 0 {
                preimage.extend_from_slice(&change_amount.to_le_bytes());
                preimage.push(cs.len() as u8);
                preimage.extend_from_slice(cs);
            }
        }

        // Locktime
        preimage.extend_from_slice(&0u32.to_le_bytes());

        // SIGHASH_ALL (4 bytes)
        preimage.extend_from_slice(&1u32.to_le_bytes());

        // Double SHA256 the preimage
        let sighash = double_sha256(&preimage);

        // Sign
        let message = Message::from_digest_slice(&sighash)
            .map_err(|e| format!("Failed to create message: {}", e))?;
        let signature = secp.sign_ecdsa(&message, &secret_key);
        let mut sig_bytes = signature.serialize_der().to_vec();
        sig_bytes.push(0x01); // SIGHASH_ALL

        // Build scriptSig: <sig> <pubkey>
        let pubkey_bytes = public_key.serialize();
        let script_sig_len = 1 + sig_bytes.len() + 1 + pubkey_bytes.len();

        // Write input to signed tx
        let txid_bytes = hex_string_to_bytes(&utxo.txid);
        signed_tx.extend(txid_bytes.iter().rev());
        signed_tx.extend_from_slice(&utxo.vout.to_le_bytes());

        // ScriptSig
        signed_tx.push(script_sig_len as u8);
        signed_tx.push(sig_bytes.len() as u8);
        signed_tx.extend_from_slice(&sig_bytes);
        signed_tx.push(pubkey_bytes.len() as u8);
        signed_tx.extend_from_slice(&pubkey_bytes);

        signed_tx.extend_from_slice(&0xFFFFFFFEu32.to_le_bytes());
    }

    // Output count
    signed_tx.push(output_count);

    // Main output
    signed_tx.extend_from_slice(&amount_sats.to_le_bytes());
    signed_tx.push(dest_script.len() as u8);
    signed_tx.extend_from_slice(&dest_script);

    // Change output
    if let Some(ref cs) = change_script {
        if change_amount > 0 {
            signed_tx.extend_from_slice(&change_amount.to_le_bytes());
            signed_tx.push(cs.len() as u8);
            signed_tx.extend_from_slice(cs);
        }
    }

    // Locktime
    signed_tx.extend_from_slice(&0u32.to_le_bytes());

    Ok((bytes_to_hex_string(&signed_tx), change_amount))
}

/// Withdraw PIVX to an external address using coin control
/// Selects promos to cover the requested amount, sends to destination,
/// and creates a new promo for any change
#[tauri::command]
pub async fn pivx_withdraw<R: Runtime>(
    handle: AppHandle<R>,
    dest_address: String,
    amount_piv: f64,
) -> Result<serde_json::Value, String> {
    // Validate destination address
    decode_pivx_address(&dest_address)?;

    let amount_sats = (amount_piv * 100_000_000.0) as u64;

    if amount_sats == 0 {
        return Err("Amount must be greater than 0".to_string());
    }

    // Emit progress
    let _ = handle.emit("pivx_withdraw_progress", serde_json::json!({
        "stage": "loading_promos",
        "message": "Loading wallet promos...",
    }));

    // Get all active promos with decrypted keys
    let promos = get_active_promos_with_keys(&handle).await?;
    if promos.is_empty() {
        return Err("No funds available to withdraw".to_string());
    }

    // Calculate total available
    let total_available: f64 = promos.iter().map(|p| p.amount_piv).sum();
    let total_available_sats = (total_available * 100_000_000.0) as u64;

    if total_available_sats < amount_sats {
        return Err(format!(
            "Insufficient funds: have {:.8} PIV, need {:.8} PIV",
            total_available, amount_piv
        ));
    }

    // Emit progress
    let _ = handle.emit("pivx_withdraw_progress", serde_json::json!({
        "stage": "selecting_coins",
        "message": "Selecting coins...",
    }));

    // Coin selection: select promos until we have enough (largest first)
    let mut selected_promos: Vec<(String, String, [u8; 32])> = Vec::new();
    let mut selected_total_sats: u64 = 0;

    for promo in &promos {
        if selected_total_sats >= amount_sats {
            break;
        }
        selected_promos.push((promo.gift_code.clone(), promo.address.clone(), promo.privkey));
        selected_total_sats += (promo.amount_piv * 100_000_000.0) as u64;
    }

    // Emit progress
    let _ = handle.emit("pivx_withdraw_progress", serde_json::json!({
        "stage": "fetching_utxos",
        "message": format!("Fetching UTXOs from {} promos...", selected_promos.len()),
    }));

    // Fetch UTXOs for all selected promos
    let mut inputs: Vec<WithdrawInput> = Vec::new();
    for (gift_code, address, privkey) in &selected_promos {
        let utxos = fetch_utxos(address).await?;
        if !utxos.is_empty() {
            inputs.push(WithdrawInput {
                utxos,
                privkey: *privkey,
                address: address.clone(),
                gift_code: gift_code.clone(),
            });
        }
    }

    if inputs.is_empty() {
        return Err("No UTXOs found in selected promos".to_string());
    }

    // Generate change promo if needed
    let change_promo = {
        let code = loop {
            let c = generate_promo_code();
            if !promo_code_exists(&handle, &c)? {
                break c;
            }
        };
        let privkey = derive_privkey_from_code(&code);
        let address = privkey_to_address(&privkey)?;
        (code, address, privkey)
    };

    // Emit progress
    let _ = handle.emit("pivx_withdraw_progress", serde_json::json!({
        "stage": "building_tx",
        "message": "Building transaction...",
    }));

    // Build the withdrawal transaction
    let (tx_hex, change_sats) = build_withdrawal_transaction(
        &inputs,
        &dest_address,
        amount_sats,
        Some(&change_promo.1),
    ).await?;

    // Emit progress
    let _ = handle.emit("pivx_withdraw_progress", serde_json::json!({
        "stage": "broadcasting",
        "message": "Broadcasting transaction...",
    }));

    // Broadcast
    let txid = broadcast_tx(&tx_hex).await?;

    // Update spent promos as claimed
    for input in &inputs {
        let _ = update_promo_status(&handle, &input.gift_code, "claimed");
    }

    // Save change promo if there's change
    let change_piv = change_sats as f64 / 100_000_000.0;
    if change_sats > 0 {
        save_promo(&handle, &change_promo.0, &change_promo.1, &change_promo.2).await?;
        // Update the change promo's amount
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let _ = conn.execute(
            "UPDATE pivx_promos SET amount_piv = ?1 WHERE gift_code = ?2",
            rusqlite::params![change_piv, change_promo.0],
        );    }

    // Emit progress
    let _ = handle.emit("pivx_withdraw_progress", serde_json::json!({
        "stage": "complete",
        "message": format!("Withdrawn! TXID: {}...", &txid[0..16.min(txid.len())]),
    }));

    Ok(serde_json::json!({
        "txid": txid,
        "amount_piv": amount_piv,
        "change_piv": change_piv,
        "promos_spent": inputs.len(),
    }))
}

// ============================================================================
// Currency / Price Oracle Functions
// ============================================================================

/// PIVX Labs Oracle API base URL
const PIVX_ORACLE_API: &str = "https://pivxla.bz/oracle/api/v1";

/// Currency info from the Oracle API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyInfo {
    pub currency: String,
    pub value: f64,
    pub last_updated: u64,
}

/// Fetch all available currencies from the Oracle API
#[tauri::command]
pub async fn pivx_get_currencies() -> Result<Vec<CurrencyInfo>, String> {
    let url = format!("{}/currencies", PIVX_ORACLE_API);

    let resp = PIVX_HTTP_CLIENT
        .get(&url)
        .timeout(BLOCKBOOK_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch currencies: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Oracle API returned status: {}", resp.status()));
    }

    let currencies: Vec<CurrencyInfo> = resp.json().await
        .map_err(|e| format!("Failed to parse currencies: {}", e))?;

    Ok(currencies)
}

/// Fetch the current price for a specific currency
#[tauri::command]
pub async fn pivx_get_price(currency: String) -> Result<CurrencyInfo, String> {
    let url = format!("{}/price/{}", PIVX_ORACLE_API, currency.to_lowercase());

    let resp = PIVX_HTTP_CLIENT
        .get(&url)
        .timeout(BLOCKBOOK_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch price: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Oracle API returned status: {}", resp.status()));
    }

    let price: CurrencyInfo = resp.json().await
        .map_err(|e| format!("Failed to parse price: {}", e))?;

    Ok(price)
}

/// Set user's preferred display currency
#[tauri::command]
pub fn pivx_set_preferred_currency<R: Runtime>(
    handle: AppHandle<R>,
    currency: String,
) -> Result<(), String> {
    crate::db::set_sql_setting(handle, "pivx_preferred_currency".to_string(), currency.to_uppercase())
}

/// Get user's preferred display currency
#[tauri::command]
pub fn pivx_get_preferred_currency<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<Option<String>, String> {
    crate::db::get_sql_setting(handle, "pivx_preferred_currency".to_string())
}
