use once_cell::sync::Lazy;
use std::collections::HashMap;

/// Extract all HTTPS URLs from a string
pub fn extract_https_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut start_idx = 0;

    while let Some(https_idx) = text[start_idx..].find("https://") {
        let abs_start = start_idx + https_idx;
        let url_text = &text[abs_start..];

        // Find the end of the URL (first whitespace or common URL-ending chars)
        let mut end_idx = url_text
            .find(|c: char| {
                c.is_whitespace()
                    || c == '"'
                    || c == '<'
                    || c == '>'
                    || c == ')'
                    || c == ']'
                    || c == '}'
                    || c == '|'
            })
            .unwrap_or(url_text.len());

        // Trim trailing punctuation
        while end_idx > 0 {
            let last_char = url_text[..end_idx].chars().last().unwrap();
            if last_char == '.' || last_char == ',' || last_char == ':' || last_char == ';' {
                end_idx -= 1;
            } else {
                break;
            }
        }

        if end_idx > "https://".len() {
            urls.push(url_text[..end_idx].to_string());
        }

        start_idx = abs_start + 1;
    }

    urls
}

/// Creates a description of a file type based on its extension.
pub fn get_file_type_description(extension: &str) -> String {
    // Define file types with descriptions
    static FILE_TYPES: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
        let mut map = HashMap::new();

        // Images
        map.insert("png", "Picture");
        map.insert("jpg", "Picture");
        map.insert("jpeg", "Picture");
        map.insert("gif", "GIF Animation");
        map.insert("webp", "Picture");

        // Audio
        map.insert("wav", "Voice Message");
        map.insert("mp3", "Audio Clip");

        // Videos
        map.insert("mp4", "Video");
        map.insert("webm", "Video");
        map.insert("mov", "Video");
        map.insert("avi", "Video");
        map.insert("mkv", "Video");

        map
    });

    // Normalize the extension to lowercase
    let normalized_ext = extension.to_lowercase();

    // Return the file type description if found, otherwise return default value
    FILE_TYPES
        .get(normalized_ext.as_str())
        .copied()
        .unwrap_or("File")
        .to_string()
}
