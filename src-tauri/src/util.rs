use once_cell::sync::Lazy;
use std::collections::HashMap;
use sha2::{Sha256, Digest};
use std::path::Path;
use blurhash::decode;
use base64::{Engine as _, engine::general_purpose};
use image::ImageEncoder;

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
        map.insert("svg", "Vector Image");
        map.insert("bmp", "Bitmap Image");
        map.insert("ico", "Icon");
        map.insert("tiff", "TIFF Image");
        map.insert("tif", "TIFF Image");
        
        // Raw Images
        map.insert("raw", "RAW Image");
        map.insert("dng", "RAW Image");
        map.insert("cr2", "Canon RAW");
        map.insert("nef", "Nikon RAW");
        map.insert("arw", "Sony RAW");
        map.insert("orf", "Olympus RAW");
        map.insert("rw2", "Panasonic RAW");

        // Audio
        map.insert("wav", "Voice Message");
        map.insert("mp3", "Audio Clip");
        map.insert("m4a", "Audio Clip");
        map.insert("aac", "Audio Clip");
        map.insert("flac", "Audio Clip");
        map.insert("ogg", "Audio Clip");
        map.insert("wma", "Audio Clip");
        map.insert("opus", "Audio Clip");
        map.insert("ape", "Audio Clip");
        map.insert("wv", "Audio Clip");
        
        // Audio Project Files
        map.insert("aup", "Audacity Project");
        map.insert("flp", "FL Studio Project");
        map.insert("als", "Ableton Project");
        map.insert("logic", "Logic Project");
        map.insert("band", "GarageBand Project");

        // Videos
        map.insert("mp4", "Video");
        map.insert("webm", "Video");
        map.insert("mov", "Video");
        map.insert("avi", "Video");
        map.insert("mkv", "Video");
        map.insert("flv", "Flash Video");
        map.insert("wmv", "Windows Video");
        map.insert("mpg", "MPEG Video");
        map.insert("mpeg", "MPEG Video");
        map.insert("m4v", "MPEG-4 Video");
        map.insert("3gp", "3GP Video");
        map.insert("3g2", "3G2 Video");
        map.insert("f4v", "Flash MP4 Video");
        map.insert("asf", "Advanced Systems Format");
        map.insert("rm", "RealMedia");
        map.insert("vob", "DVD Video");
        map.insert("ogv", "Ogg Video");
        map.insert("mxf", "Material Exchange Format");
        map.insert("ts", "MPEG Transport Stream");
        map.insert("m2ts", "Blu-ray Video");
        
        // Documents
        map.insert("pdf", "PDF Document");
        map.insert("doc", "Word Document");
        map.insert("docx", "Word Document");
        map.insert("xls", "Excel Spreadsheet");
        map.insert("xlsx", "Excel Spreadsheet");
        map.insert("ppt", "PowerPoint Presentation");
        map.insert("pptx", "PowerPoint Presentation");
        map.insert("odt", "OpenDocument Text");
        map.insert("ods", "OpenDocument Spreadsheet");
        map.insert("odp", "OpenDocument Presentation");
        map.insert("rtf", "Rich Text Document");
        map.insert("tex", "LaTeX Document");
        map.insert("pages", "Pages Document");
        map.insert("numbers", "Numbers Spreadsheet");
        map.insert("key", "Keynote Presentation");
        
        // Text Files
        map.insert("txt", "Text File");
        map.insert("md", "Markdown");
        map.insert("log", "Log File");
        map.insert("csv", "CSV File");
        map.insert("tsv", "TSV File");
        
        // Data Files
        map.insert("json", "JSON File");
        map.insert("xml", "XML File");
        map.insert("yaml", "YAML File");
        map.insert("yml", "YAML File");
        map.insert("toml", "TOML File");
        map.insert("sql", "SQL File");
        map.insert("db", "Database File");
        map.insert("sqlite", "SQLite Database");
        
        // Archives
        map.insert("zip", "ZIP Archive");
        map.insert("rar", "RAR Archive");
        map.insert("7z", "7-Zip Archive");
        map.insert("tar", "TAR Archive");
        map.insert("gz", "GZip Archive");
        map.insert("bz2", "BZip2 Archive");
        map.insert("xz", "XZ Archive");
        map.insert("tgz", "Compressed TAR");
        map.insert("tbz", "Compressed TAR");
        map.insert("txz", "Compressed TAR");
        map.insert("cab", "Cabinet Archive");
        map.insert("iso", "Disc Image");
        map.insert("dmg", "macOS Disk Image");
        map.insert("pkg", "Package File");
        map.insert("deb", "Debian Package");
        map.insert("rpm", "RPM Package");
        map.insert("apk", "Android Package");
        map.insert("ipa", "iOS App");
        map.insert("jar", "Java Archive");
        map.insert("war", "Web Archive");
        map.insert("ear", "Enterprise Archive");
        
        // 3D Files
        map.insert("obj", "3D Object");
        map.insert("fbx", "Autodesk FBX");
        map.insert("gltf", "GL Transmission Format");
        map.insert("glb", "GL Binary");
        map.insert("stl", "Stereolithography");
        map.insert("ply", "Polygon File");
        map.insert("dae", "COLLADA");
        map.insert("3ds", "3D Studio");
        map.insert("blend", "Blender File");
        map.insert("c4d", "Cinema 4D");
        map.insert("max", "3ds Max");
        map.insert("ma", "Maya ASCII");
        map.insert("mb", "Maya Binary");
        map.insert("usdz", "Universal Scene");
        
        // CAD Files
        map.insert("dwg", "AutoCAD Drawing");
        map.insert("dxf", "Drawing Exchange");
        map.insert("step", "STEP CAD");
        map.insert("stp", "STEP CAD");
        map.insert("iges", "IGES CAD");
        map.insert("igs", "IGES CAD");
        map.insert("sat", "ACIS SAT");
        map.insert("ipt", "Inventor Part");
        map.insert("iam", "Inventor Assembly");
        map.insert("prt", "Part File");
        map.insert("sldprt", "SolidWorks Part");
        map.insert("sldasm", "SolidWorks Assembly");
        map.insert("slddrw", "SolidWorks Drawing");
        map.insert("catpart", "CATIA Part");
        map.insert("catproduct", "CATIA Product");
        
        // Code Files
        map.insert("js", "JavaScript");
        map.insert("ts", "TypeScript");
        map.insert("jsx", "React JSX");
        map.insert("tsx", "React TSX");
        map.insert("py", "Python");
        map.insert("rs", "Rust");
        map.insert("go", "Go");
        map.insert("java", "Java");
        map.insert("kt", "Kotlin");
        map.insert("cpp", "C++");
        map.insert("cc", "C++");
        map.insert("cxx", "C++");
        map.insert("c", "C");
        map.insert("h", "Header File");
        map.insert("hpp", "C++ Header");
        map.insert("cs", "C#");
        map.insert("rb", "Ruby");
        map.insert("php", "PHP");
        map.insert("swift", "Swift");
        map.insert("m", "Objective-C");
        map.insert("mm", "Objective-C++");
        map.insert("lua", "Lua");
        map.insert("r", "R Script");
        map.insert("scala", "Scala");
        map.insert("clj", "Clojure");
        map.insert("dart", "Dart");
        map.insert("ex", "Elixir");
        map.insert("elm", "Elm");
        map.insert("erl", "Erlang");
        map.insert("fs", "F#");
        map.insert("hs", "Haskell");
        map.insert("jl", "Julia");
        map.insert("nim", "Nim");
        map.insert("pl", "Perl");
        map.insert("sh", "Shell Script");
        map.insert("bash", "Bash Script");
        map.insert("zsh", "Zsh Script");
        map.insert("fish", "Fish Script");
        map.insert("ps1", "PowerShell");
        map.insert("bat", "Batch File");
        map.insert("cmd", "Command File");
        map.insert("vb", "Visual Basic");
        map.insert("vbs", "VBScript");
        map.insert("asm", "Assembly");
        map.insert("s", "Assembly");
        
        // Config Files
        map.insert("ini", "INI Config");
        map.insert("cfg", "Config File");
        map.insert("conf", "Config File");
        map.insert("config", "Config File");
        map.insert("env", "Environment File");
        map.insert("properties", "Properties File");
        map.insert("plist", "Property List");
        map.insert("gitignore", "Git Ignore");
        map.insert("dockerignore", "Docker Ignore");
        map.insert("editorconfig", "Editor Config");
        map.insert("eslintrc", "ESLint Config");
        map.insert("prettierrc", "Prettier Config");
        
        // Web Files
        map.insert("html", "HTML File");
        map.insert("htm", "HTML File");
        map.insert("css", "CSS Stylesheet");
        map.insert("scss", "SCSS Stylesheet");
        map.insert("sass", "Sass Stylesheet");
        map.insert("less", "Less Stylesheet");
        map.insert("vue", "Vue Component");
        map.insert("svelte", "Svelte Component");
        
        // Vector Graphics
        map.insert("eps", "Encapsulated PostScript");
        map.insert("ai", "Adobe Illustrator");
        map.insert("sketch", "Sketch File");
        map.insert("fig", "Figma File");
        map.insert("xd", "Adobe XD");
        
        // Other
        map.insert("exe", "Executable");
        map.insert("msi", "Windows Installer");
        map.insert("app", "macOS Application");
        map.insert("ttf", "TrueType Font");
        map.insert("otf", "OpenType Font");
        map.insert("woff", "Web Font");
        map.insert("woff2", "Web Font 2");
        map.insert("eot", "Embedded OpenType");
        map.insert("ics", "Calendar File");
        map.insert("vcf", "vCard Contact");
        map.insert("torrent", "Torrent File");

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

/// Convert a byte slice to a hex string
pub fn bytes_to_hex_string(bytes: &[u8]) -> String {
    // Pre-allocate the exact size needed (2 hex chars per byte)
    let mut result = String::with_capacity(bytes.len() * 2);
    
    // Use a lookup table for hex conversion
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    
    for &b in bytes {
        // Extract high and low nibbles
        let high = b >> 4;
        let low = b & 0xF;
        result.push(HEX_CHARS[high as usize] as char);
        result.push(HEX_CHARS[low as usize] as char);
    }
    
    result
}

/// Convert hex string back to bytes for decryption
pub fn hex_string_to_bytes(s: &str) -> Vec<u8> {
    // Pre-allocate the result vector to avoid resize operations
    let mut result = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    
    // Process bytes directly to avoid UTF-8 decoding overhead
    let mut i = 0;
    while i + 1 < bytes.len() {
        // Convert two hex characters to a single byte
        let high = match bytes[i] {
            b'0'..=b'9' => bytes[i] - b'0',
            b'a'..=b'f' => bytes[i] - b'a' + 10,
            b'A'..=b'F' => bytes[i] - b'A' + 10,
            _ => 0,
        };
        
        let low = match bytes[i + 1] {
            b'0'..=b'9' => bytes[i + 1] - b'0',
            b'a'..=b'f' => bytes[i + 1] - b'a' + 10,
            b'A'..=b'F' => bytes[i + 1] - b'A' + 10,
            _ => 0,
        };
        
        result.push((high << 4) | low);
        i += 2;
    }
    
    result
}

/// Calculate SHA-256 hash of file data
pub fn calculate_file_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Check if a filename looks like a nonce (shorter than a SHA-256 hash)
/// SHA-256 hashes are 64 characters, nonces are typically 32 characters
pub fn is_nonce_filename(filename: &str) -> bool {
    // Extract the base name without extension
    let base_name = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    
    // Check if it's hex and shorter than SHA-256 hash length
    base_name.len() < 64 && base_name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Migrate a nonce-based file to hash-based naming
/// Returns the new hash-based filename if successful
pub fn migrate_nonce_file_to_hash(file_path: &Path) -> Result<String, std::io::Error> {
    // Read the file content
    let data = std::fs::read(file_path)?;
    
    // Calculate the hash
    let hash = calculate_file_hash(&data);
    
    // Get the extension
    let extension = file_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    
    // Create new hash-based filename
    let new_filename = if extension.is_empty() {
        hash.clone()
    } else {
        format!("{}.{}", hash, extension)
    };
    
    // Create new path in same directory
    let new_path = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&new_filename);
    
    // Copy to new location (don't delete original yet in case of errors)
    std::fs::copy(file_path, &new_path)?;
    
    // Remove original file only after successful copy
    std::fs::remove_file(file_path)?;
    
    Ok(new_filename)
}

/// Decode a blurhash string to a Base64-encoded PNG data URL
/// Returns a data URL string that can be used directly in an <img> src attribute
pub fn decode_blurhash_to_base64(blurhash: &str, width: u32, height: u32, punch: f32) -> String {
    const EMPTY_DATA_URL: &str = "data:image/png;base64,";
    
    let decoded_data = match decode(blurhash, width, height, punch) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Failed to decode blurhash: {}", e);
            return EMPTY_DATA_URL.to_string();
        }
    };
    
    let pixel_count = (width * height) as usize;
    let bytes_per_pixel = decoded_data.len() / pixel_count;
    
    // Fast path for RGBA data
    if bytes_per_pixel == 4 {
        encode_rgba_to_png_base64(&decoded_data, width, height)
    } 
    // Convert RGB to RGBA
    else if bytes_per_pixel == 3 {
        // Pre-allocate exact size needed
        let mut rgba_data = Vec::with_capacity(pixel_count * 4);
        
        // Use chunks_exact for safe and efficient iteration
        for rgb_chunk in decoded_data.chunks_exact(3) {
            rgba_data.extend_from_slice(&[rgb_chunk[0], rgb_chunk[1], rgb_chunk[2], 255]);
        }
        
        encode_rgba_to_png_base64(&rgba_data, width, height)
    } else {
        eprintln!("Unexpected decoded data length: {} bytes for {} pixels", 
                 decoded_data.len(), pixel_count);
        EMPTY_DATA_URL.to_string()
    }
}

/// Helper function to encode RGBA data to PNG base64
#[inline]
fn encode_rgba_to_png_base64(rgba_data: &[u8], width: u32, height: u32) -> String {
    const EMPTY_DATA_URL: &str = "data:image/png;base64,";
    
    // Create image without additional allocation
    let img = match image::RgbaImage::from_raw(width, height, rgba_data.to_vec()) {
        Some(img) => img,
        None => {
            eprintln!("Failed to create image from RGBA data");
            return EMPTY_DATA_URL.to_string();
        }
    };
    
    // Pre-allocate PNG buffer with estimated size
    // PNG is typically smaller than raw RGBA, estimate 50% of original size
    let estimated_size = (rgba_data.len() / 2).max(1024);
    let mut png_data = Vec::with_capacity(estimated_size);
    
    let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
    if let Err(e) = encoder.write_image(
        img.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgba8
    ) {
        eprintln!("Failed to encode PNG: {}", e);
        return EMPTY_DATA_URL.to_string();
    }
    
    // Encode as base64 with pre-allocated string
    // Base64 is 4/3 the size of input + padding
    let base64_capacity = ((png_data.len() * 4 / 3) + 4) + 22; // +22 for "data:image/png;base64,"
    let mut result = String::with_capacity(base64_capacity);
    result.push_str("data:image/png;base64,");
    general_purpose::STANDARD.encode_string(&png_data, &mut result);
    
    result
}
