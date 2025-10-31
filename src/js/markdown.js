/**
 * Markdown Parser using Marked.js with DOMPurify hardening
 * Configured for Discord-like behavior, code highlighting, and safe HTML output
 *
 * Marked.js, Highlight.js, and DOMPurify are all shipped locally.
 */

// Configure Marked.js once it's loaded
function initializeMarked() {
    // Configure marked for Discord-like behavior
    marked.setOptions({
        // Enable GitHub Flavored Markdown but with customizations
        gfm: true,
        // Enable single line breaks similar to Discord/GFM
        breaks: true,
        // Don't use pedantic mode (too strict)
        pedantic: false,
        // Disable headerIds to avoid potential XSS
        headerIds: false,
        // Disable mangle to keep email addresses readable
        mangle: false,
        // Prefix language classes so Highlight.js CSS picks them up
        langPrefix: 'hljs language-',
        // Syntax highlighting handled in custom renderer
        highlight: null
    });
    
    // Override the tokenizer to:
    // 1. Disable + as a list marker (only - and * should work)
    // 2. Make blockquotes stricter (require > on each line)
    // 3. Disable autolinks (bare URLs) - Vector handles link detection separately
    const tokenizer = {
        list(src) {
            const cap = /^( {0,3})([-*]|\d{1,9}\.) /.exec(src);
            if (!cap) return;
            return false;
        },
        blockquote(src) {
            // Stricter blockquote: each line must start with >
            // This prevents "loose" blockquote behavior where lines without > are included
            const cap = /^( {0,3}> ?(paragraph|[^\n]*)(?:\n|$))+/.exec(src);
            if (!cap) return;
            
            const text = cap[0].replace(/^ *>[ \t]?/gm, '');
            
            return {
                type: 'blockquote',
                raw: cap[0],
                tokens: this.lexer.blockTokens(text, []),
                text: text
            };
        },
        // Disable autolinks - return false to prevent bare URLs from being auto-linked
        url(src) {
            return false;
        }
    };

    // Custom renderer for better control
    const renderer = new marked.Renderer();

    // Disable markdown link rendering - Vector handles links separately with linkifyUrls()
    // This prevents conflicts and allows Vector's link handling (OpenGraph, etc.) to work
    renderer.link = function(token) {
        const text = token.text || '';
        const href = token.href || '';
        
        // If text equals href, it's an autolink (bare URL) - just return the URL
        // This allows Vector's linkifyUrls to handle it
        if (text === href) {
            return href;
        }
        
        // Otherwise it's a markdown link [text](url) - return the markdown syntax
        // so Vector's linkifyUrls can process it
        return `[${text}](${href})`;
    };

    // Disable markdown image rendering - Vector prefers images as file attachments
    // This prevents arbitrary image loading and maintains consistent UX
    renderer.image = function(token) {
        const text = token.text || '';
        const href = token.href || '';
        // Return the markdown syntax as plain text
        return `![${text}](${href})`;
    };

    // Override inline code (no highlighting for inline code, only blocks)
    renderer.codespan = function(token) {
        const code = token.text || '';
        return `<code>${encodeAttr(code)}</code>`;
    };

    renderer.code = function(code, infostring, escaped) {
        const raw = toPlainString(code);
        const info = toPlainString(infostring).trim();
        const lang = info.split(/\s+/)[0]?.toLowerCase() || '';

        let highlighted = escaped ? raw : encodeAttr(raw);
        if (raw && typeof hljs !== 'undefined') {
            try {
                if (lang && hljs.getLanguage(lang)) {
                    highlighted = hljs.highlight(raw, {
                        language: lang,
                        ignoreIllegals: true
                    }).value;
                } else {
                    highlighted = hljs.highlightAuto(raw).value;
                }
            } catch (err) {
                console.error('Highlight.js error:', err);
                highlighted = encodeAttr(raw);
            }
        }

        const classes = ['hljs'];
        if (lang) {
            classes.push(`language-${encodeAttr(lang)}`);
        }

        // Store the raw code in a data attribute for copying
        const encodedRaw = encodeAttr(raw);
        
        return `<div class="code-block-wrapper">
            <button class="code-copy-btn" data-code="${encodedRaw}" title="Copy code">
                <span class="icon icon-copy"></span>
            </button>
            <pre><code class="${classes.join(' ')}">${highlighted}</code></pre>
        </div>`;
    };

    marked.use({ tokenizer, renderer });
}

/**
 * Encode special attribute characters to HTML entities
 * @private
 */
function encodeAttr(str) {
    return (str + '')
        .replace(/&/g, '&amp;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');
}

/**
 * Normalize Marked renderer inputs (strings, tokens, arrays) into a string.
 * @private
 */
function toPlainString(value) {
    if (value == null) return '';
    if (typeof value === 'string') return value;
    if (Array.isArray(value)) return value.map(toPlainString).join('');
    if (typeof value === 'object') {
        if (typeof value.text === 'string') return value.text;
        if (typeof value.raw === 'string') return value.raw;
    }
    return String(value);
}

/**
 * Sanitize URLs to prevent javascript: and data: URIs
 * @private
 */
function sanitizeUrl(url) {
    if (!url) return '';

    const urlString = typeof url === 'string' ? url : String(url);
    const normalized = urlString.replace(/^[\s\u0000-\u001F]+/, '').trim();
    const lower = normalized.toLowerCase();

    const allowedProtocols = ['http:', 'https:', 'mailto:'];
    if (
        allowedProtocols.some((protocol) => lower.startsWith(protocol)) ||
        lower.startsWith('/') ||
        lower.startsWith('./') ||
        lower.startsWith('../')
    ) {
        return encodeAttr(normalized);
    }

    return '';
}

/**
 * Sanitize HTML by escaping it as plain text
 * This prevents any HTML injection
 * IMPORTANT: Preserves newlines for proper markdown parsing
 * @private
 */
function sanitizeHTML(text) {
    const element = document.createElement('div');
    element.textContent = text; // Use textContent instead of innerText to preserve newlines
    return element.innerHTML;
}

/**
 * Replace all <p> elements with spans and add line breaks between them.
 * Preserves inner markup and avoids nested paragraph structures.
 * Special handling for blockquotes to avoid trailing breaks inside them.
 * Also adds spacing after blockquotes.
 * @private
 */
function removeParagraphTags(html) {
    if (!html) return html;

    const temp = document.createElement('div');
    temp.innerHTML = html.trim();

    const paragraphs = temp.querySelectorAll('p');
    paragraphs.forEach((p) => {
        const span = document.createElement('span');
        span.className = 'markdown-paragraph';
        span.innerHTML = p.innerHTML;
        
        // Check if we're inside a blockquote
        const parentBlockquote = p.closest('blockquote');
        const isInBlockquote = parentBlockquote !== null;
        
        // Check if there's a next sibling element
        const nextSibling = p.nextElementSibling;
        
        // Determine if we should add line breaks after this paragraph
        let shouldAddBreaks = false;
        
        if (isInBlockquote) {
            // Inside blockquote: only add breaks if followed by another paragraph in the same blockquote
            shouldAddBreaks = nextSibling && nextSibling.tagName === 'P';
        } else {
            // Outside blockquote: add breaks if there's any next sibling (paragraph or other block element)
            shouldAddBreaks = nextSibling !== null;
        }
        
        if (shouldAddBreaks) {
            const br1 = document.createElement('br');
            const br2 = document.createElement('br');
            p.replaceWith(span, br1, br2);
        } else {
            p.replaceWith(span);
        }
    });

    // Add spacing after blockquotes if they're followed by other content
    const blockquotes = temp.querySelectorAll('blockquote');
    blockquotes.forEach((bq) => {
        const nextSibling = bq.nextElementSibling;
        if (nextSibling) {
            const br = document.createElement('br');
            bq.after(br);
        }
    });

    // Fix numbered list continuity across interrupting elements
    // When lists are separated by code blocks, set the 'start' attribute to continue numbering
    const orderedLists = Array.from(temp.querySelectorAll('ol'));
    let cumulativeCount = 0;
    
    orderedLists.forEach((ol, index) => {
        const itemCount = ol.querySelectorAll('li').length;
        
        if (index === 0) {
            // First list starts at 1 (default)
            cumulativeCount = itemCount;
        } else {
            // Subsequent lists continue from where the previous one left off
            ol.setAttribute('start', cumulativeCount + 1);
            cumulativeCount += itemCount;
        }
    });

    return temp.innerHTML || html;
}

/**
 * Parse Markdown into HTML
 * This is the main function used throughout the app
 * 
 * @param {string} md - The markdown text to parse
 * @returns {string} - The parsed HTML
 */
function parseMarkdown(md) {
    const rawInput = typeof md === 'string' ? md : String(md);

    let rendered;
    try {
        rendered = marked.parse(rawInput);
    } catch (error) {
        console.error('Markdown parsing error:', error);
        return sanitizeHTML(rawInput);
    }

    if (typeof DOMPurify === 'undefined') {
        console.warn('DOMPurify not loaded, returning unsanitized HTML');
        return rendered;
    }


    const SAFE_TAGS = [
        'abbr', 'b', 'blockquote', 'br', 'button', 'code', 'del', 'details', 'div', 'em', 'h1', 'h2', 'h3', 'h4',
        'h5', 'h6', 'hr', 'i', 'li', 'ol', 'p', 'pre', 's', 'span', 'strong', 'sub',
        'summary', 'sup', 'table', 'tbody', 'td', 'th', 'thead', 'tr', 'u', 'ul'
    ];

    const SAFE_ATTRS = [
        'class', 'aria-label', 'aria-hidden', 'open', 'data-code', 'title', 'start'
    ];

    const sanitized = DOMPurify.sanitize(rendered, {
        ALLOWED_TAGS: SAFE_TAGS,
        ALLOWED_ATTR: SAFE_ATTRS,
        FORBID_TAGS: ['style', 'script'],
        FORBID_ATTR: ['onerror', 'onclick', 'onload', 'onmouseover', 'onmouseout', 'onfocus', 'onblur']
    });

    return removeParagraphTags(sanitized);
}

/**
 * Set up event delegation for code block copy buttons
 * Uses event delegation on document to handle dynamically added code blocks
 */
document.addEventListener('click', (e) => {
    const copyBtn = e.target.closest('.code-copy-btn');
    if (!copyBtn) return;

    const code = copyBtn.getAttribute('data-code');
    if (!code) return;

    // Decode HTML entities back to original text
    const textarea = document.createElement('textarea');
    textarea.innerHTML = code;
    const decodedCode = textarea.value;

    navigator.clipboard.writeText(decodedCode).then(() => {
        // Show checkmark feedback
        const originalHTML = copyBtn.innerHTML;
        copyBtn.innerHTML = '<span class="icon icon-check"></span>';
        copyBtn.classList.add('copied');
        
        setTimeout(() => {
            copyBtn.innerHTML = originalHTML;
            copyBtn.classList.remove('copied');
        }, 2000);
    }).catch(err => {
        console.error('Failed to copy code:', err);
    });
});

// Initialize marked when the script loads (if it's already available)
initializeMarked();