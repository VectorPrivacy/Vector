/** Snarkdown v2.0 - Copyright (c) 2017 Jason Miller - https://github.com/developit/snarkdown/tree/main */

const TAGS = {
    '': ['<em>', '</em>'],
    _: ['<strong>', '</strong>'],
    '*': ['<strong>', '</strong>'],
    '~': ['<s>', '</s>'],
    '\n': ['<br />'],
    ' ': ['<br />'],
    '-': ['<hr />']
};

/** Outdent a string based on the first indented line's leading whitespace
 *	@private
 */
function outdent(str) {
    return str.replace(RegExp('^' + (str.match(/^(\t| )+/) || '')[0], 'gm'), '');
}

/** Encode special attribute characters to HTML entities in a String.
 *	@private
 */
function encodeAttr(str) {
    return (str + '').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/** Parse Markdown into an HTML String. */
function internalParseMarkdown(md, prevLinks) {
    let tokenizer = /((?:^|\n+)(?:\n---+|\* \*(?: \*)+)\n)|(?:^``` *(\w*)\n([\s\S]*?)\n```$)|((?:(?:^|\n+)(?:\t|  {2,}).+)+\n*)|((?:(?:^|\n)([>*+-]|\d+\.)\s+.*)+)|(?:!\[([^\]]*?)\]\(([^)]+?)\))|(\[)|(\](?:\(([^)]+?)\))?)|(?:(?:^|\n+)([^\s].*)\n(-{3,}|={3,})(?:\n+|$))|(?:(?:^|\n+)(#{1,6})\s*(.+)(?:\n+|$))|(?:`([^`].*?)`)|(  \n\n*|\n{2,}|__|\*\*|[_*]|~~)/gm,
        context = [],
        out = '',
        links = prevLinks || {},
        last = 0,
        chunk, prev, token, inner, t;

    function tag(token) {
        let desc = TAGS[token[1] || ''];
        let end = context[context.length - 1] == token;
        if (!desc) return token;
        if (!desc[1]) return desc[0];
        if (end) context.pop();
        else context.push(token);
        return desc[end | 0];
    }

    function flush() {
        let str = '';
        while (context.length) str += tag(context[context.length - 1]);
        return str;
    }

    md = md.replace(/^\[(.+?)\]:\s*(.+)$/gm, (s, name, url) => {
        links[name.toLowerCase()] = url;
        return '';
    }).replace(/^\n+|\n+$/g, '');

    while ((token = tokenizer.exec(md))) {
        prev = md.substring(last, token.index);
        last = tokenizer.lastIndex;
        chunk = token[0];
        if (prev.match(/[^\\](\\\\)*\\$/)) {
            // escaped
        }
        // Code/Indent blocks:
        else if (t = (token[3] || token[4])) {
            chunk = '<pre class="code ' + (token[4] ? 'poetry' : token[2].toLowerCase()) + '"><code' + (token[2] ? ` class="language-${token[2].toLowerCase()}"` : '') + '>' + outdent(encodeAttr(t).replace(/^\n+|\n+$/g, '')) + '</code></pre>';
        }
        // > Quotes, -* lists:
        else if (t = token[6]) {
            if (t.match(/\./)) {
                token[5] = token[5].replace(/^\d+/gm, '');
            }
            inner = internalParseMarkdown(outdent(token[5].replace(/^\s*[>*+.-]/gm, '')));
            if (t == '>') t = 'blockquote';
            else {
                t = t.match(/\./) ? 'ol' : 'ul';
                inner = inner.replace(/^(.*)(\n|$)/gm, '<li>$1</li>');
            }
            chunk = '<' + t + '>' + inner + '</' + t + '>';
        }
        // Images:
        else if (token[8]) {
            chunk = `<img src="${encodeAttr(token[8])}" alt="${encodeAttr(token[7])}">`;
        }
        // Links:
        else if (token[10]) {
            // Get the URL from either direct URL or reference
            const url = token[11] || links[prev.toLowerCase()];
            if (url) {
                // Only create a link if there's actually a URL
                out = out.replace('<a>', `<a href="${encodeAttr(url)}">`);
                chunk = flush() + '</a>';
            } else {
                // If no URL, replace the opening <a> tag with the original bracket text
                out = out.replace('<a>', '[');
                chunk = flush() + ']';
            }
        }
        else if (token[9]) {
            chunk = '<a>';
        }
        // Headings:
        else if (token[12] || token[14]) {
            t = 'h' + (token[14] ? token[14].length : (token[13] > '=' ? 1 : 2));
            chunk = '<' + t + '>' + internalParseMarkdown(token[12] || token[15], links) + '</' + t + '>';
        }
        // `code`:
        else if (token[16]) {
            chunk = '<code>' + encodeAttr(token[16]) + '</code>';
        }
        // Inline formatting: *em*, **strong** & friends
        else if (token[17] || token[1]) {
            chunk = tag(token[17] || '--');
        }
        out += prev;
        out += chunk;
    }

    return (out + md.substring(last) + flush()).replace(/^\n+|\n+$/g, '');
}

/**
 * A wrapper for Snarkdown that uses a little sanitisation trick to prevent users doing awful shit
 * Also protects URLs from being mangled by markdown formatting (especially underscores)
 */
function parseMarkdown(md, prevLinks) {
    const sanitized = sanitizeHTML(md);
    
    // Protect URLs from markdown processing by temporarily replacing them
    const urlPattern = /(https?:\/\/[^\s<>"{}|\\^`\[\]]+)/gi;
    const urls = [];
    let urlIndex = 0;
    
    // Extract URLs and replace with unique tokens that won't trigger markdown
    const protected = sanitized.replace(urlPattern, (url) => {
        urls.push(url);
        // Use a token format that markdown won't process: ＜URL0＞ (fullwidth brackets)
        return `＜URL${urlIndex++}＞`;
    });
    
    // Parse markdown with protected URLs
    let result = internalParseMarkdown(protected, prevLinks);
    
    // Restore URLs
    urls.forEach((url, index) => {
        result = result.replace(`＜URL${index}＞`, url);
    });
    
    return result;
}

/** Sanitise text by initalising it as Plaintext within a div, then extract that plaintext */
function sanitizeHTML(text) {
    const element = document.createElement('div');
    element.innerText = text;
    return element.innerHTML;
}