// Static configuration
const cryptoAddressConfig = {
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

// Pre-process the config into a sorted array (done once)
const addressTypes = [];
for (const coinName in cryptoAddressConfig) {
    const coin = cryptoAddressConfig[coinName];
    const color = cryptoAddressConfig[coinName].color;
    const uri = cryptoAddressConfig[coinName].uri;

    if (coin.addressTypes) {
        for (const typeName in coin.addressTypes) {
            const typeConfig = coin.addressTypes[typeName];

            // Skip if disabled
            if (typeConfig.disabled) continue;

            addressTypes.push({
                coin: coinName,
                type: typeName,
                color: color,
                uri: uri,
                patterns: typeConfig.patterns,
                priority: typeConfig.priority || 0,
                caseInsensitive: typeConfig.caseInsensitive
            });
        }
    }
}

// Sort address types by priority (highest first)
addressTypes.sort((a, b) => b.priority - a.priority);

/**
 * Detects cryptocurrency addresses in text
 * @param {string} text - Text to search for crypto addresses
 * @returns {Object|null} - Detected address info or null if none found
 */
function detectCryptoAddress(text) {
    // Check each address type in priority order
    for (const addrType of addressTypes) {
        // Run each pattern for this address type
        for (const pattern of addrType.patterns) {
            const regex = new RegExp(pattern, addrType.caseInsensitive ? 'i' : '');
            const match = text.match(regex);

            if (match) {
                return {
                    coin: addrType.coin,
                    type: addrType.type,
                    color: addrType.color,
                    uri: addrType.uri,
                    address: match[0]
                };
            }
        }
    }

    // No match found
    return null;
}

/**
 * Renders the Payment UI for a given coin address
 * @param {Object} coin - A detected coin address
 * @returns {HTMLDivElement}
 */
function renderCryptoAddress(coin) {
    const divCoin = document.createElement('div');
    divCoin.classList.add('msg-payment');

    // Render the Coin's brand color across the UI
    // RGB HEX
    divCoin.style.borderColor = `#${coin.color}`;
    // RGB HEX + 25% Alpha in HEX for complete RGBA
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