<script>
    // A detected crypto address in a message: the coin's brand colour, logo, name and a
    // Pay button opening its payment URI.
    let { coin, h } = $props();   // h: openUrl(url)
</script>

<div class="msg-payment" style="border-color: #{coin.color}; background-color: #{coin.color}40;">
    <img src="./icons/{coin.coin.toLowerCase()}.svg" alt="">
    <h2>{coin.coin}</h2>
    <button pay-uri={coin.uri + coin.address} onclick={() => h.openUrl(coin.uri + coin.address)}>Pay</button>
</div>
