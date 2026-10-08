// Desktop's reading of a Whisper pass (src-tauri/src/whisper.rs): which segments count, how
// confident the pass was, and when to try again. Pure, so the worker and the tests share it.

export const MIN_CONFIDENCE = 0.40;
/** Beam search, then beam search warmed up: [beam size, temperature]. */
export const RETRIES = [[5, 0.0], [5, 0.6]];

// A country for the flag, by Whisper language id: several languages share one.
export const COUNTRY = [
    'GB', 'CN', 'DE', 'ES', 'RU', 'KR', 'FR', 'JP', 'PT', 'TR', 'PL', 'ES', 'NL', 'SA', 'SE', 'IT', 'ID', 'IN', 'FI', 'VN',
    'IL', 'UA', 'GR', 'MY', 'CZ', 'RO', 'DK', 'HU', 'IN', 'NO', 'TH', 'PK', 'HR', 'BG', 'LT', 'VA', 'NZ', 'IN', 'GB', 'SK',
    'IN', 'IR', 'LV', 'BD', 'RS', 'AZ', 'SI', 'IN', 'EE', 'MK', 'FR', 'ES', 'IS', 'AM', 'NP', 'MN', 'BA', 'KZ', 'AL', 'TZ',
    'ES', 'IN', 'IN', 'LK', 'KH', 'ZW', 'NG', 'SO', 'ZA', 'FR', 'GE', 'BY', 'TJ', 'PK', 'IN', 'ET', 'IL', 'LA', 'UZ', 'FO',
    'HT', 'AF', 'TM', 'NO', 'MT', 'IN', 'LU', 'MM', 'CN', 'PH', 'MG', 'IN', 'RU', 'US', 'CD', 'NG', 'RU', 'ID', 'ID', 'HK',
];

/** A loop: one 3-word phrase covering most of the text, or a 4-char run spelled over and over. */
export function repeats(sections) {
    const text = sections.map((s) => s.text).join(' ');
    const words = text.split(/\s+/).filter(Boolean);
    if (words.length >= 12) {
        const counts = new Map();
        let max = 0;
        for (let i = 0; i + 2 < words.length; i++) {
            const key = `${words[i]}\0${words[i + 1]}\0${words[i + 2]}`;
            const c = (counts.get(key) || 0) + 1;
            counts.set(key, c);
            max = Math.max(max, c);
        }
        if (max >= 4 && (max * 3) / words.length > 0.4) return true;
    }
    const chars = [...text];
    if (chars.length >= 32) {
        const counts = new Map();
        let max = 0;
        for (let i = 0; i + 3 < chars.length; i++) {
            const key = chars[i] + chars[i + 1] + chars[i + 2] + chars[i + 3];
            const c = (counts.get(key) || 0) + 1;
            counts.set(key, c);
            max = Math.max(max, c);
        }
        if (max >= 8 && max / (chars.length - 3) > 0.3) return true;
    }
    return false;
}

/**
 * One pass's result. `segments`: [{ text, t0 (centiseconds), p (mean token probability) }].
 */
export function summarize(segments, langId, language) {
    const sections = [];
    for (const s of segments) {
        const trimmed = s.text.trim();
        if (!trimmed || trimmed === ',' || trimmed === '.' || trimmed === '[BLANK_AUDIO]') continue;
        sections.push({ text: s.text, at: s.t0 * 10, confidence: s.p });
    }
    let confidence = sections.length ? sections.reduce((sum, s) => sum + s.confidence, 0) / sections.length : 0;
    // A loop can be confidently wrong: zero it so the retries run.
    if (repeats(sections)) confidence = 0;
    return { sections, lang: COUNTRY[langId] ?? 'auto', language: langId >= 0 ? language : '', confidence };
}

/** Greedy first; below MIN_CONFIDENCE, the retries, keeping whichever pass was surest. */
export async function bestOf(pass) {
    let best = await pass(0, 0);
    if (best.confidence < MIN_CONFIDENCE && best.sections.length) {
        for (const [beam, temperature] of RETRIES) {
            const next = await pass(beam, temperature);
            if (next.confidence > best.confidence) best = next;
            if (best.confidence >= MIN_CONFIDENCE) break;
        }
    }
    return best;
}
