// New Message with a Community invite link pasted into its search.
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    await sleep(200);
    openNewChat();
    await sleep(500);
    const input = document.querySelector('#create-group-filter');
    input.value = 'https://vectorapp.io/invite/naddr1qvzqqqr4gupzqmjxss3dld622uu8q25gywum9qtg4w4cv4064jmg20xsac2aam5nqqxnzd3cxqmrzv3exgmr2wfesgsmew#Zk3pQ9vT2xLm8RbN4cWd7YhJ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
})();
