/// Extract all HTTPS URLs from a string
pub fn extract_https_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut start_idx = 0;
    
    while let Some(https_idx) = text[start_idx..].find("https://") {
        let abs_start = start_idx + https_idx;
        let url_text = &text[abs_start..];
        
        // Find the end of the URL (first whitespace or common URL-ending chars)
        let mut end_idx = url_text.find(|c: char| {
            c.is_whitespace() || c == '"' || c == '<' || c == '>' || c == ')'
                || c == ']' || c == '}' || c == '|'
        }).unwrap_or(url_text.len());
        
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