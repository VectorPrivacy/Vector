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

        // If it's an email autolink (mailto:email where text is the email), just return the email
        if (href === `mailto:${text}`) {
            return text;
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

        // Store the raw code in data attribute for post-processing
        const encodedRaw = encodeAttr(raw);
        const codeClasses = ['hljs'];
        if (lang) {
            codeClasses.push(`language-${encodeAttr(lang)}`);
        }
        
        return `<div class="code-block-wrapper" data-raw-code="${encodedRaw}">
            <pre><code class="${codeClasses.join(' ')}">${highlighted}</code></pre>
        </div>`;
    };

    // Spoiler extension: ||spoiler text||
    const spoilerExtension = {
        name: 'spoiler',
        level: 'inline',
        start(src) {
            return src.match(/\|\|/)?.index;
        },
        tokenizer(src) {
            const match = src.match(/^\|\|([^|]+)\|\|/);
            if (match) {
                return {
                    type: 'spoiler',
                    raw: match[0],
                    text: match[1]
                };
            }
        },
        renderer(token) {
            return `<span><span data-spoiler-text="${encodeAttr(token.text)}">${encodeAttr(token.text)}</span></span>`;
        }
    };

    marked.use({ tokenizer, renderer, extensions: [spoilerExtension] });
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
        
        // Don't add <br><br> after paragraphs - rely on CSS margins for spacing
        // Block elements (ul, ol, blockquote, pre, div, hr, table, h1-h6) have their own margins
        // Consecutive paragraphs use .markdown-paragraph margins
        p.replaceWith(span);
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

    // Wrap tables in a scrollable container for horizontal overflow on small screens
    const tables = temp.querySelectorAll('table');
    tables.forEach((table) => {
        const wrapper = document.createElement('div');
        wrapper.className = 'table-wrapper';
        table.parentNode.insertBefore(wrapper, table);
        wrapper.appendChild(table);
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
        'abbr', 'b', 'blockquote', 'br', 'code', 'del', 'details', 'div', 'em', 'h1', 'h2', 'h3', 'h4',
        'h5', 'h6', 'hr', 'i', 'li', 'ol', 'p', 'pre', 's', 'span', 'strong', 'sub',
        'summary', 'sup', 'table', 'tbody', 'td', 'th', 'thead', 'tr', 'u', 'ul'
    ];

    const SAFE_ATTRS = [
        'class', 'aria-label', 'aria-hidden', 'open', 'data-raw-code', 'data-language', 'title', 'start', 'data-spoiler-text'
    ];

    // Whitelist of allowed class prefixes (for highlight.js and our own classes)
    const ALLOWED_CLASS_PREFIXES = ['hljs', 'language-'];
    const ALLOWED_CLASSES = ['code-block-wrapper', 'markdown-paragraph', 'spoiler-wrapper', 'spoiler', 'revealed'];

    // Create a one-time hook for this sanitization call
    DOMPurify.removeAllHooks();
    DOMPurify.addHook('afterSanitizeAttributes', function(node) {
        if (node.hasAttribute('class')) {
            const classes = node.getAttribute('class').split(/\s+/).filter(Boolean);
            const validClasses = classes.filter(cls => {
                // Allow exact matches
                if (ALLOWED_CLASSES.includes(cls)) return true;
                // Allow prefix matches (hljs-*, language-*)
                return ALLOWED_CLASS_PREFIXES.some(prefix => cls.startsWith(prefix));
            });
            
            if (validClasses.length > 0) {
                node.setAttribute('class', validClasses.join(' '));
            } else {
                node.removeAttribute('class');
            }
        }
    });

    const sanitized = DOMPurify.sanitize(rendered, {
        ALLOWED_TAGS: SAFE_TAGS,
        ALLOWED_ATTR: SAFE_ATTRS,
        FORBID_TAGS: ['style', 'script', 'form', 'input', 'button'],
        FORBID_ATTR: ['onerror', 'onclick', 'onload', 'onmouseover', 'onmouseout', 'onfocus', 'onblur'],
        ALLOWED_URI_REGEXP: /^(?:(?:(?:f|ht)tps?|mailto|tel|callto|sms|cid|xmpp):|[^a-z]|[a-z+.\-]+(?:[^a-z+.\-:]|$))/i
    });

    // Clean up the hook after use
    DOMPurify.removeAllHooks();

    const withoutParagraphs = removeParagraphTags(sanitized);
    const withClasses = addClassesToMarkdownElements(withoutParagraphs);
    return addCopyButtonsToCodeBlocks(withClasses);
}

/**
 * Add copy buttons to code blocks after sanitization
 * This is done post-sanitization to prevent button injection attacks
 * @private
 */
function addCopyButtonsToCodeBlocks(html) {
    if (!html) return html;
    
    const temp = document.createElement('div');
    temp.innerHTML = html;
    
    const codeWrappers = temp.querySelectorAll('.code-block-wrapper[data-raw-code]');
    codeWrappers.forEach((wrapper) => {
        const rawCode = wrapper.getAttribute('data-raw-code');
        if (!rawCode) return;
        
        // Create the copy button (safely, outside of user-controlled content)
        const button = document.createElement('button');
        button.className = 'code-copy-btn';
        button.setAttribute('data-code', rawCode);
        button.setAttribute('title', 'Copy code');
        
        const icon = document.createElement('span');
        icon.className = 'icon icon-copy';
        button.appendChild(icon);
        
        // Insert button as first child
        wrapper.insertBefore(button, wrapper.firstChild);
        
        // Remove the data attribute as it's no longer needed
        wrapper.removeAttribute('data-raw-code');
    });
    
    return temp.innerHTML;
}

/**
 * Add classes to markdown elements after sanitization
 * This prevents class injection while allowing our own styling
 * @private
 */
function addClassesToMarkdownElements(html) {
    if (!html) return html;
    
    const temp = document.createElement('div');
    temp.innerHTML = html;
    
    // Add classes to spoiler elements (identified by data-spoiler-text attribute)
    temp.querySelectorAll('span[data-spoiler-text]').forEach(el => {
        // Add spoiler class to the element itself
        el.classList.add('spoiler');
        
        // Ensure parent span has spoiler-wrapper class
        if (el.parentElement && el.parentElement.tagName === 'SPAN') {
            el.parentElement.classList.add('spoiler-wrapper');
        }
    });
    
    return temp.innerHTML;
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

/**
 * Set up event delegation for spoiler reveals
 * Click to reveal spoiler (stays revealed, doesn't toggle back)
 */
document.addEventListener('click', (e) => {
    const spoiler = e.target.closest('.spoiler');
    if (!spoiler) return;
    
    // Only reveal if not already revealed
    if (!spoiler.classList.contains('revealed')) {
        spoiler.classList.add('revealed');
    }
});

// Initialize marked when the script loads (if it's already available)
initializeMarked();