#!/usr/bin/env node
/**
 * build-emoji.mjs — Generate src/js/emoji.js from Unicode CLDR data + twemoji SVGs
 *
 * Downloads emoji-test.txt and CLDR annotation data, cross-references with
 * available twemoji SVGs, merges CLDR keywords with custom keyword additions,
 * and outputs a complete emoji.js in the same format as the hand-written one.
 *
 * Usage: node scripts/build-emoji.mjs
 *
 * Zero runtime dependencies. Uses Node 18+ built-in fetch.
 */

import { readFileSync, writeFileSync, existsSync, mkdirSync, readdirSync, unlinkSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const CACHE_DIR = join(__dirname, '.emoji-cache');
const SVG_DIR = join(ROOT, 'src', 'twemoji', 'svg');
const EMOJI_JS = join(ROOT, 'src', 'js', 'emoji.js');
const CUSTOM_KEYWORDS_FILE = join(__dirname, 'emoji-custom-keywords.json');

// Unicode data URLs
const EMOJI_TEST_URL = 'https://unicode.org/Public/emoji/latest/emoji-test.txt';
const CLDR_ANNOTATIONS_URL = 'https://cdn.jsdelivr.net/npm/cldr-annotations-full@48.1.0/annotations/en/annotations.json';
const CLDR_DERIVED_URL = 'https://cdn.jsdelivr.net/npm/cldr-annotations-derived-full@48.1.0/annotationsDerived/en/annotations.json';

// Skin tone modifier codepoints (1F3FB–1F3FF)
const SKIN_TONES = new Set(['1F3FB', '1F3FC', '1F3FD', '1F3FE', '1F3FF']);

// Base codepoints to exclude entirely (non-female pregnancy emojis)
// 1FAC3 = pregnant man, 1FAC4 = pregnant person (keep 1F930 = pregnant woman)
const EXCLUDED_BASES = new Set(['1FAC3', '1FAC4']);

// Stopwords to remove from keyword lists
const STOPWORDS = new Set(['with', 'and', 'the', 'of', 'a', 'an', 'in', 'on', 'for', 'to', 'is']);

// ── Phase 0: Helpers ────────────────────────────────────────────────────

async function cachedFetch(url, filename) {
    mkdirSync(CACHE_DIR, { recursive: true });
    const cachePath = join(CACHE_DIR, filename);
    if (existsSync(cachePath)) {
        console.log(`  [cache] ${filename}`);
        return readFileSync(cachePath, 'utf-8');
    }
    console.log(`  [fetch] ${url}`);
    const res = await fetch(url);
    if (!res.ok) throw new Error(`Failed to fetch ${url}: ${res.status}`);
    const text = await res.text();
    writeFileSync(cachePath, text);
    return text;
}

/**
 * Compute the twemoji SVG filename for an emoji's codepoints.
 * Mirrors twemoji's grabTheRightIcon logic:
 * - If contains ZWJ (200D): keep FE0F, join with '-'
 * - Otherwise: strip FE0F, join with '-'
 * All hex is lowercase.
 */
function toTwemojiFilename(codepoints) {
    const hasZWJ = codepoints.includes('200D');
    const filtered = hasZWJ
        ? codepoints
        : codepoints.filter(cp => cp !== 'FE0F');
    // Twemoji uses minimal hex (no leading zeros): 0023 → 23, 1F600 → 1f600
    return filtered.map(cp => cp.replace(/^0+/, '').toLowerCase() || '0').join('-');
}

// ── Phase 1: Download ───────────────────────────────────────────────────

async function downloadAll() {
    console.log('Phase 1: Downloading Unicode data...');
    const [emojiTestRaw, cldrRaw, cldrDerivedRaw] = await Promise.all([
        cachedFetch(EMOJI_TEST_URL, 'emoji-test.txt'),
        cachedFetch(CLDR_ANNOTATIONS_URL, 'cldr-annotations.json'),
        cachedFetch(CLDR_DERIVED_URL, 'cldr-annotations-derived.json'),
    ]);
    return { emojiTestRaw, cldrRaw, cldrDerivedRaw };
}

// ── Phase 2: Parse emoji-test.txt ───────────────────────────────────────

function parseEmojiTest(raw) {
    console.log('Phase 2: Parsing emoji-test.txt...');
    const emojis = [];
    let currentGroup = '';
    let currentSubgroup = '';

    for (const line of raw.split('\n')) {
        // Group headers: "# group: Smileys & Emotion"
        const groupMatch = line.match(/^# group: (.+)$/);
        if (groupMatch) {
            currentGroup = groupMatch[1].trim();
            continue;
        }
        // Subgroup headers
        const subgroupMatch = line.match(/^# subgroup: (.+)$/);
        if (subgroupMatch) {
            currentSubgroup = subgroupMatch[1].trim();
            continue;
        }
        // Skip comments and blank lines
        if (line.startsWith('#') || !line.trim()) continue;

        // Data line: "1F600 ; fully-qualified # 😀 E1.0 grinning face"
        const match = line.match(/^([0-9A-F ]+)\s*;\s*([\w-]+)\s*#\s*(\S+)\s+E[\d.]+\s+(.+)$/i);
        if (!match) continue;

        const codepoints = match[1].trim().split(/\s+/);
        const status = match[2];
        const char = match[3];
        const name = match[4].trim();

        // Only include fully-qualified emojis
        if (status !== 'fully-qualified') continue;

        // Filter out skin tone variants
        if (codepoints.some(cp => SKIN_TONES.has(cp.toUpperCase()))) continue;

        // Filter out excluded base emojis (e.g. pregnant man/person)
        if (codepoints.some(cp => EXCLUDED_BASES.has(cp.toUpperCase()))) continue;

        emojis.push({ codepoints, char, name, group: currentGroup, subgroup: currentSubgroup });
    }

    console.log(`  Parsed ${emojis.length} fully-qualified emojis (skin tones excluded)`);
    return emojis;
}

// ── Phase 3: Filter by twemoji SVG availability ─────────────────────────

function filterBySvg(emojis) {
    console.log('Phase 3: Filtering by twemoji SVG availability...');
    const svgFiles = new Set(readdirSync(SVG_DIR).map(f => f.replace('.svg', '')));

    const available = [];
    const missing = [];

    for (const emoji of emojis) {
        const primary = toTwemojiFilename(emoji.codepoints);

        // Try primary filename
        if (svgFiles.has(primary)) {
            emoji.svgName = primary;
            available.push(emoji);
            continue;
        }

        // Fallback: strip ALL FE0F (some ZWJ sequences might work without)
        const stripped = emoji.codepoints.filter(cp => cp !== 'FE0F').map(cp => cp.replace(/^0+/, '').toLowerCase() || '0').join('-');
        if (stripped !== primary && svgFiles.has(stripped)) {
            emoji.svgName = stripped;
            available.push(emoji);
            continue;
        }

        missing.push(emoji);
    }

    console.log(`  ${available.length} emojis have matching SVGs, ${missing.length} missing`);
    if (missing.length > 0 && missing.length <= 20) {
        for (const m of missing) {
            console.log(`    Missing: ${m.char} ${m.name} (${toTwemojiFilename(m.codepoints)})`);
        }
    }
    return available;
}

/**
 * Delete SVG files that match excluded patterns (skin tones, excluded bases).
 * Runs after filterBySvg so we can clean up the SVG directory automatically
 * whenever twemoji assets are updated.
 */
function pruneExcludedSvgs() {
    console.log('Phase 3b: Pruning excluded SVGs from twemoji directory...');
    const skinToneLower = [...SKIN_TONES].map(s => s.toLowerCase());
    const excludedBasesLower = [...EXCLUDED_BASES].map(s => s.toLowerCase());

    let deleted = 0;
    for (const file of readdirSync(SVG_DIR)) {
        const name = file.replace('.svg', '');
        const parts = name.split('-');

        // Delete if any part is a skin tone modifier
        if (parts.some(p => skinToneLower.includes(p))) {
            unlinkSync(join(SVG_DIR, file));
            deleted++;
            continue;
        }

        // Delete if any part is an excluded base codepoint
        if (parts.some(p => excludedBasesLower.includes(p))) {
            unlinkSync(join(SVG_DIR, file));
            deleted++;
            continue;
        }
    }

    console.log(`  Deleted ${deleted} excluded SVGs`);
}

// ── Phase 4: Merge keywords ─────────────────────────────────────────────

function parseCLDR(cldrRaw, cldrDerivedRaw) {
    console.log('Phase 4: Loading CLDR annotations...');
    const cldr = JSON.parse(cldrRaw);
    const cldrDerived = JSON.parse(cldrDerivedRaw);

    // Structure: { annotations: { identity: {...}, annotations: { "emoji": { default: [...], tts: [...] } } } }
    const base = cldr?.annotations?.annotations || {};
    // Structure: { annotationsDerived: { identity: {...}, annotations: { "emoji": { default: [...], tts: [...] } } } }
    const derived = cldrDerived?.annotationsDerived?.annotations || {};

    // Merge into a single map: emoji char → { keywords: string[], tts: string }
    const map = new Map();

    function addEntry(char, data) {
        if (!data || typeof data !== 'object') return;
        map.set(char, {
            keywords: data.default || [],
            tts: Array.isArray(data.tts) ? data.tts[0] : (data.tts || ''),
        });
    }

    for (const [char, data] of Object.entries(base)) {
        addEntry(char, data);
    }
    for (const [char, data] of Object.entries(derived)) {
        if (!map.has(char)) {
            addEntry(char, data);
        }
    }

    console.log(`  ${map.size} CLDR annotations loaded`);
    return map;
}

function loadCustomKeywords() {
    if (!existsSync(CUSTOM_KEYWORDS_FILE)) return new Map();
    const raw = JSON.parse(readFileSync(CUSTOM_KEYWORDS_FILE, 'utf-8'));
    const map = new Map();
    for (const [emoji, keywords] of Object.entries(raw)) {
        map.set(emoji, keywords);
    }
    console.log(`  ${map.size} custom keyword entries loaded`);
    return map;
}

function migrateCustomKeywords(emojis, cldrMap) {
    console.log('  Auto-migrating custom keywords from current emoji.js...');

    // Parse existing emoji.js
    const raw = readFileSync(EMOJI_JS, 'utf-8');
    const existing = new Map();
    const re = /\{\s*emoji:\s*'([^']+)',\s*name:\s*'([^']+)'\s*\}/g;
    let m;
    while ((m = re.exec(raw)) !== null) {
        const char = m[1];
        const words = m[2].toLowerCase().split(/\s+/).filter(Boolean);
        // If same emoji appears multiple times, merge words
        if (existing.has(char)) {
            const prev = existing.get(char);
            for (const w of words) {
                if (!prev.includes(w)) prev.push(w);
            }
        } else {
            existing.set(char, words);
        }
    }

    console.log(`  Found ${existing.size} emojis in current emoji.js`);

    // For each existing entry, compute the diff vs CLDR
    const custom = {};
    let customCount = 0;

    for (const [char, ourWords] of existing) {
        const cldrEntry = cldrLookup(cldrMap, char);
        const cldrWords = new Set();
        if (cldrEntry) {
            // Add all words from CLDR short name and keywords
            for (const kw of cldrEntry.keywords) {
                for (const w of kw.toLowerCase().split(/[\s|]+/)) {
                    if (w) cldrWords.add(w);
                }
            }
            if (cldrEntry.tts) {
                for (const w of cldrEntry.tts.toLowerCase().split(/\s+/)) {
                    if (w) cldrWords.add(w);
                }
            }
        }

        // Also add words from emoji-test.txt name
        const emojiEntry = emojis.find(e => e.char === char);
        if (emojiEntry) {
            for (const w of emojiEntry.name.toLowerCase().split(/\s+/)) {
                if (w) cldrWords.add(w);
            }
        }

        // Diff: words in ours but not in CLDR
        const diff = ourWords.filter(w => !cldrWords.has(w) && !STOPWORDS.has(w));
        if (diff.length > 0) {
            custom[char] = diff;
            customCount += diff.length;
        }
    }

    writeFileSync(CUSTOM_KEYWORDS_FILE, JSON.stringify(custom, null, 2) + '\n');
    console.log(`  Migrated ${customCount} custom keywords for ${Object.keys(custom).length} emojis`);
    console.log(`  Written to ${CUSTOM_KEYWORDS_FILE}`);

    const map = new Map();
    for (const [emoji, keywords] of Object.entries(custom)) {
        map.set(emoji, keywords);
    }
    return map;
}

/**
 * Look up CLDR data for an emoji char. CLDR may store entries without FE0F
 * variation selectors, so try the bare character if the full one doesn't match.
 */
function cldrLookup(cldrMap, char) {
    if (cldrMap.has(char)) return cldrMap.get(char);
    // Strip FE0F and try again
    const bare = char.replaceAll('\uFE0F', '');
    if (bare !== char && cldrMap.has(bare)) return cldrMap.get(bare);
    return null;
}

function buildKeywords(emojis, cldrMap, customMap) {
    console.log('  Merging keywords (CLDR + emoji-test.txt + custom)...');

    let cldrHits = 0;
    for (const emoji of emojis) {
        const words = new Set();

        // Layer 1: emoji-test.txt name (e.g. "grinning face")
        for (const w of emoji.name.toLowerCase().split(/\s+/)) {
            if (w && !STOPWORDS.has(w)) words.add(w);
        }

        // Layer 2: CLDR annotations
        const cldr = cldrLookup(cldrMap, emoji.char);
        if (cldr) {
            cldrHits++;
            for (const kw of cldr.keywords) {
                for (const w of kw.toLowerCase().split(/[\s|]+/)) {
                    if (w && !STOPWORDS.has(w)) words.add(w);
                }
            }
            if (cldr.tts) {
                for (const w of cldr.tts.toLowerCase().split(/\s+/)) {
                    if (w && !STOPWORDS.has(w)) words.add(w);
                }
            }
        }

        // Layer 3: Custom keywords
        const custom = customMap.get(emoji.char);
        if (custom) {
            for (const w of custom) {
                if (w) words.add(w.toLowerCase());
            }
        }

        emoji.keywords = [...words].join(' ');
    }

    console.log(`  CLDR matched ${cldrHits}/${emojis.length} emojis`);
}

// ── Phase 5: Output ─────────────────────────────────────────────────────

function extractFooter() {
    const raw = readFileSync(EMOJI_JS, 'utf-8');
    // Everything after the closing "];" of arrEmojis
    const idx = raw.indexOf('];\n');
    if (idx === -1) throw new Error('Could not find end of arrEmojis in emoji.js');
    return raw.slice(idx + 2); // includes the \n after ];
}

function writeEmojiJs(emojis) {
    console.log('Phase 5: Writing emoji.js...');

    const footer = extractFooter();

    const lines = [];
    lines.push('/** Auto-generated emoji dataset — do not edit by hand */');
    lines.push('/** Run: node scripts/build-emoji.mjs to regenerate */');
    lines.push('');
    lines.push('const arrEmojis = [');

    let currentGroup = '';
    for (const emoji of emojis) {
        if (emoji.group !== currentGroup) {
            currentGroup = emoji.group;
            if (lines[lines.length - 1] !== '') lines.push('');
            lines.push(`    // ${currentGroup}`);
        }
        // Escape single quotes in keywords (shouldn't happen but be safe)
        const safeKeywords = emoji.keywords.replace(/'/g, "\\'");
        lines.push(`    { emoji: '${emoji.char}', name: '${safeKeywords}' },`);
    }

    // Remove trailing comma from last entry
    const lastLine = lines[lines.length - 1];
    if (lastLine.endsWith(',')) {
        lines[lines.length - 1] = lastLine.slice(0, -1);
    }

    lines.push('];');

    const output = lines.join('\n') + footer;
    writeFileSync(EMOJI_JS, output);
    console.log(`  Written ${emojis.length} emojis to ${EMOJI_JS}`);
}

// ── Main ────────────────────────────────────────────────────────────────

async function main() {
    console.log('=== build-emoji.mjs ===\n');

    // Phase 1: Download
    const { emojiTestRaw, cldrRaw, cldrDerivedRaw } = await downloadAll();

    // Phase 2: Parse
    const emojis = parseEmojiTest(emojiTestRaw);

    // Phase 3: Filter by SVG + prune excluded SVGs from disk
    const available = filterBySvg(emojis);
    pruneExcludedSvgs();

    // Phase 4: Keywords
    const cldrMap = parseCLDR(cldrRaw, cldrDerivedRaw);

    let customMap;
    if (!existsSync(CUSTOM_KEYWORDS_FILE)) {
        customMap = migrateCustomKeywords(available, cldrMap);
    } else {
        customMap = loadCustomKeywords();
    }

    buildKeywords(available, cldrMap, customMap);

    // Phase 5: Output
    writeEmojiJs(available);

    // Stats
    console.log('\n=== Stats ===');
    console.log(`  Total emojis: ${available.length}`);
    console.log(`  Groups: ${[...new Set(available.map(e => e.group))].join(', ')}`);
    console.log(`  Custom keywords: ${customMap.size} emojis with additions`);
    console.log('\nDone!');
}

main().catch(err => {
    console.error('Error:', err);
    process.exit(1);
});
