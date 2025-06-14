// Base58 alphabet used by Bitcoin and most cryptocurrencies
const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

/**
 * Validates if a string is proper Base58 encoding
 * @param {string} str - String to validate
 * @returns {boolean} - True if valid Base58, false otherwise
 */
function isValidBase58(str) {
    // Check if all characters are in Base58 alphabet
    for (let i = 0; i < str.length; i++) {
        if (BASE58_ALPHABET.indexOf(str[i]) === -1) {
            return false;
        }
    }
    
    // Length validation for different address types
    const len = str.length;
    return (len >= 25 && len <= 35) || (len >= 50 && len <= 120);
}

/**
 * Cryptocurrency configuration with address patterns and metadata
 */
const CRYPTO_CONFIG = {
    "PIVX": {
        color: "9530f4",
        uri: "https://app.mypivxwallet.org/?pay=",
        addressTypes: {
            "shielded": {
                patterns: ["ps1[a-z0-9]{50,120}"],
                caseInsensitive: true,
                priority: 95
            },
            "transparent": {
                patterns: ["D[a-zA-Z0-9]{33,35}"],
                priority: 70
            }
        }
    }
};

/**
 * Lazy-initialized address types with compiled patterns
 */
let compiledAddressTypes = null;

/**
 * Compiles address patterns into optimized regex objects
 * @returns {Array} Sorted array of compiled address types
 */
function getCompiledAddressTypes() {
    if (compiledAddressTypes) return compiledAddressTypes;
    
    const addressTypes = [];
    
    for (const [coinName, coin] of Object.entries(CRYPTO_CONFIG)) {
        if (!coin.addressTypes) continue;
        
        for (const [typeName, typeConfig] of Object.entries(coin.addressTypes)) {
            if (typeConfig.disabled) continue;
            
            // Pre-compile regex patterns for performance
            // Add word boundaries to prevent matching within URLs or other text
            const compiledPatterns = typeConfig.patterns.map(pattern => 
                new RegExp(`\\b${pattern}\\b`, typeConfig.caseInsensitive ? 'i' : '')
            );
            
            addressTypes.push({
                coin: coinName,
                type: typeName,
                color: coin.color,
                uri: coin.uri,
                patterns: typeConfig.patterns,
                compiledPatterns,
                priority: typeConfig.priority || 0,
                caseInsensitive: typeConfig.caseInsensitive
            });
        }
    }
    
    // Sort by priority (highest first) and cache result
    compiledAddressTypes = addressTypes.sort((a, b) => b.priority - a.priority);
    return compiledAddressTypes;
}

/**
 * Detects cryptocurrency addresses in text using optimized pattern matching
 * @param {string} text - Text to search for crypto addresses
 * @returns {Object|null} - Detected address info or null if none found
 */
function detectCryptoAddress(text) {
    if (text.length < 25) return null;
    
    const addressTypes = getCompiledAddressTypes();
    
    // Check each address type in priority order
    for (const addressType of addressTypes) {
        // Test against pre-compiled regex patterns
        for (const regex of addressType.compiledPatterns) {
            const match = text.match(regex);
            
            if (match) {
                const address = match[0];
                const matchIndex = match.index;
                
                // Additional validation to prevent matching within URLs
                // Check if the match is preceded by a slash, dot, or other URL characters
                if (matchIndex > 0) {
                    const prevChar = text[matchIndex - 1];
                    if (prevChar === '/' || prevChar === '.' || prevChar === ':' || prevChar === '=' || prevChar === '?') {
                        continue;
                    }
                }
                
                // Check if the match is followed by a slash or other URL characters
                const afterIndex = matchIndex + address.length;
                if (afterIndex < text.length) {
                    const nextChar = text[afterIndex];
                    if (nextChar === '/' || nextChar === '.') {
                        continue;
                    }
                }
                
                // For transparent addresses, validate Base58
                if (addressType.type === 'transparent' && !isValidBase58(address)) {
                    continue;
                }
                
                // Return the detected and validated address
                return {
                    coin: addressType.coin,
                    type: addressType.type,
                    color: addressType.color,
                    uri: addressType.uri,
                    address: address
                };
            }
        }
    }
    
    return null;
}

/**
 * Renders the Payment UI for a detected cryptocurrency address
 * @param {Object} coin - Detected coin address with metadata
 * @returns {HTMLDivElement} Complete payment UI element
 */
function renderCryptoAddress(coin) {
    const divCoin = document.createElement('div');
    divCoin.classList.add('msg-payment');

    // Render the Coin's brand color across the UI
    divCoin.style.borderColor = `#${coin.color}`;
    divCoin.style.backgroundColor = `#${coin.color}40`;

    // Render the Coin Logo
    const imgCoin = document.createElement('img');
    imgCoin.src = `./icons/${coin.coin.toLowerCase()}.svg`;

    // Render the Coin Name
    const h2Coin = document.createElement('h2');
    h2Coin.textContent = coin.coin;

    // Render the "Pay" button
    const btnCoin = document.createElement('button');
    btnCoin.setAttribute('pay-uri', coin.uri + coin.address);
    btnCoin.textContent = `Pay`;

    // Compile and return the DOM object
    divCoin.appendChild(imgCoin);
    divCoin.appendChild(h2Coin);
    divCoin.appendChild(btnCoin);
    return divCoin;
}