//! The PIVX wallet over the desktop's promo-code wallet (`pivx.rs`): balances, deposits,
//! sends into a chat, claims and withdrawals.

use vector_media::pivx as px;

pub use px::PivxPromo;

fn handle() -> Result<tauri::AppHandle, String> {
    vector_media::TAURI_APP.get().copied().ok_or_else(|| "The wallet isn't ready yet".to_string())
}

pub async fn balance() -> Result<f64, String> {
    px::pivx_get_wallet_balance(handle()?).await
}

pub async fn promos(force: bool) -> Result<Vec<PivxPromo>, String> {
    px::pivx_refresh_balances(handle()?, Some(force)).await
}

pub async fn create_promo() -> Result<PivxPromo, String> {
    px::pivx_create_promo(handle()?).await
}

pub async fn send_amount(receiver: String, amount: f64) -> Result<String, String> {
    px::pivx_send_payment(handle()?, receiver, amount).await
}

pub async fn send_promo(receiver: String, code: String) -> Result<String, String> {
    px::pivx_send_existing_promo(handle()?, receiver, code).await
}

pub async fn withdraw(address: String, amount: f64) -> Result<serde_json::Value, String> {
    px::pivx_withdraw(handle()?, address, amount).await
}

pub async fn claim(code: String) -> Result<serde_json::Value, String> {
    px::pivx_claim_from_message(handle()?, code).await
}

pub async fn address_balance(address: String, force: bool) -> Result<f64, String> {
    px::pivx_check_address_balance(address, Some(force)).await
}

pub async fn chat_payments(chat: String) -> Result<Vec<serde_json::Value>, String> {
    px::pivx_get_chat_payments(handle()?, chat).await
}

pub fn wallet_address() -> Option<String> {
    px::pivx_get_wallet_address(handle().ok()?).ok().flatten()
}

pub fn set_wallet_address(address: String) -> Result<(), String> {
    px::pivx_set_wallet_address(handle()?, address)
}

pub fn currency() -> Option<String> {
    px::pivx_get_preferred_currency(handle().ok()?).ok().flatten()
}

pub fn set_currency(currency: String) -> Result<(), String> {
    px::pivx_set_preferred_currency(handle()?, currency)
}

/// `fetchPivxCurrencies`: the common fiat codes the oracle prices, sorted.
pub async fn currencies() -> Vec<String> {
    const FIAT: &[&str] = &[
        "USD", "EUR", "GBP", "CAD", "AUD", "JPY", "CHF", "CNY", "INR", "RUB", "BRL", "MXN", "KRW", "SGD", "HKD",
        "SEK", "NOK", "DKK", "PLN", "CZK", "HUF", "TRY", "ZAR", "AED", "SAR", "THB", "MYR", "IDR", "PHP", "VND",
        "NZD", "ILS", "ARS", "CLP", "COP", "PEN", "NGN", "KES", "EGP", "PKR", "BDT", "TWD", "RON", "BGN", "HRK",
        "ISK", "UAH",
    ];
    let mut list: Vec<String> = px::pivx_get_currencies()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.currency.to_uppercase())
        .filter(|c| FIAT.contains(&c.as_str()))
        .collect();
    list.sort();
    list.dedup();
    list
}

pub async fn price(currency: String) -> Option<f64> {
    px::pivx_get_price(currency).await.ok().map(|c| c.value).filter(|v| *v > 0.)
}

/// `detectDefaultCurrency`: the locale's region, else USD.
pub fn default_currency() -> String {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MONETARY"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let region = locale.split(['.', '@']).next().unwrap_or("").split(['_', '-']).nth(1).unwrap_or("").to_uppercase();
    let euro = [
        "IE", "DE", "FR", "ES", "IT", "NL", "BE", "AT", "PT", "FI", "GR", "SK", "SI", "EE", "LV", "LT", "MT", "CY",
        "LU", "MC", "SM", "VA", "AD", "ME", "XK",
    ];
    if euro.contains(&region.as_str()) {
        return "EUR".into();
    }
    let map: &[(&str, &str)] = &[
        ("US", "USD"), ("CA", "CAD"), ("AU", "AUD"), ("NZ", "NZD"), ("GB", "GBP"), ("UK", "GBP"), ("JP", "JPY"),
        ("CN", "CNY"), ("HK", "HKD"), ("TW", "TWD"), ("KR", "KRW"), ("IN", "INR"), ("SG", "SGD"), ("MY", "MYR"),
        ("TH", "THB"), ("ID", "IDR"), ("PH", "PHP"), ("VN", "VND"), ("PK", "PKR"), ("BD", "BDT"), ("RU", "RUB"),
        ("UA", "UAH"), ("PL", "PLN"), ("CZ", "CZK"), ("HU", "HUF"), ("RO", "RON"), ("BG", "BGN"), ("HR", "HRK"),
        ("RS", "RSD"), ("CH", "CHF"), ("SE", "SEK"), ("NO", "NOK"), ("DK", "DKK"), ("IS", "ISK"), ("TR", "TRY"),
        ("IL", "ILS"), ("AE", "AED"), ("SA", "SAR"), ("QA", "QAR"), ("KW", "KWD"), ("BH", "BHD"), ("OM", "OMR"),
        ("EG", "EGP"), ("ZA", "ZAR"), ("NG", "NGN"), ("KE", "KES"), ("GH", "GHS"), ("MX", "MXN"), ("BR", "BRL"),
        ("AR", "ARS"), ("CL", "CLP"), ("CO", "COP"), ("PE", "PEN"), ("VE", "VES"), ("NI", "NIO"), ("CR", "CRC"),
        ("PA", "PAB"), ("DO", "DOP"), ("GT", "GTQ"),
    ];
    map.iter().find(|(r, _)| *r == region).map_or("USD", |(_, c)| c).to_string()
}
