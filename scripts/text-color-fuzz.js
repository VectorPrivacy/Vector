// Colour span edges must never change how a message's markdown renders. Paste into the
// console of a running Vector Web page (`npm run web:serve`, signed in or not): every span
// position over a fixed corpus and seeded random strings is marked, rendered and compared
// with the plain render, sentinels stripped. Re-run after touching tcMarkSpans' edge rules.
//
// Known leftovers, all exotic: a tab at a line start inside a quote or list, a table
// without a leading pipe, and `#` runs mixed with indented text.
(() => {
    const strip = (s) => s.replace(/[\u{F0000}-\u{F01FF}]/gu, '');
    const fixed = [
        'YOOO!', '!!!', '**YOOO!**', '_wow!_', '"_a_"', 'love you :)', '(wow)', 'C# and F#!', '<3 you!', 'go -> there',
        'a < b > c!', '¡hola!', '“quoted!”', 'hey **you!** there', '- **a!**\n- _b?_', '> **q!**', '```\ncode!\n```',
        '    code!\n\nafter!', 'see ![img](https://a.b/c.png)!', '[a](https://x.com/f_(b))', '# Hi #', '#tag', '||secret!||',
        '| a | b |\n|---|---|\n| c | d |', 'x._y_', '_y_.x', '**wow!**x', '<!b', 'hey \nyou ', 'a  \nb',
    ];
    const alphabets = [
        ['a', 'b', ' ', ' ', '\n', '-', '*', '_', '!', '#', '>', '1.', '\\', '(', ')', '.', '"', '    '],
        ['*', '_', '~', '|', '\\', '`', '[', ']', '(', ')', '<', '>', '!', '#', '.', ',', '?', '"', 'a', 'b', ' ', '\n', '-', ':', '¡', '“'],
        ['<', '>', '/', '!', '?', 'a', 'b', 'p', ' ', '\n', '"', '=', '3', '-', '*', '_', '[', ']'],
    ];
    const random = [];
    alphabets.forEach((set, k) => {
        let seed = 1000 + k;
        const rnd = (n) => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed % n; };
        for (let i = 0; i < 2000; i++) {
            let t = '';
            const len = 2 + rnd(11);
            for (let j = 0; j < len; j++) t += set[rnd(set.length)];
            random.push(t);
        }
    });
    let tried = 0;
    const broken = [];
    for (const text of [...fixed, ...random]) {
        const base = renderMarkdown(text);
        const n = [...text].length;
        for (let a = 0; a < n; a++) for (let b = a + 1; b <= n; b++) {
            const marked = tcMarkSpans(text, [{ kind: 'color', from: a, to: b, effect: 'solid', colors: ['#ff0000'] }]);
            if (!marked) continue;
            tried++;
            if (strip(renderMarkdown(marked)) !== base) broken.push({ text, span: [a, b], marked });
        }
    }
    return { strings: fixed.length + random.length, tried, broken: broken.length, examples: broken.slice(0, 20) };
})();
