use once_cell::sync::Lazy;
use std::collections::HashMap;
use sha2::{Sha256, Digest};
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

/// Format bytes into human-readable format (KB, MB, GB)
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    
    if bytes < KB as u64 {
        format!("{} B", bytes)
    } else if bytes < MB as u64 {
        format!("{:.1} KB", bytes as f64 / KB)
    } else if bytes < GB as u64 {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{:.1} GB", bytes as f64 / GB)
    }
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

/// Generate a blurhash from RGBA8 image data with adaptive downscaling for optimal performance
///
/// This function uses adaptive downscaling based on image size:
/// - 1% for 4K+ images (≥3840px)
/// - 2.5% for large images (≥1920px)
/// - 5% for medium images (≥960px)
/// - 10% for smaller images (<960px)
///
/// Returns the blurhash string, or None if encoding fails
pub fn generate_blurhash_from_rgba(pixels: &[u8], width: u32, height: u32) -> Option<String> {
    // Adaptive downscaling based on image size for optimal blurhash performance
    let max_dimension = width.max(height);
    let scale_factor = if max_dimension >= 3840 {
        0.01  // 1% for 4K+ images
    } else if max_dimension >= 1920 {
        0.025 // 2.5% for large images
    } else if max_dimension >= 960 {
        0.05  // 5% for medium images
    } else {
        0.10  // 10% for smaller images
    };
    
    let thumbnail_width = (width as f32 * scale_factor).max(1.0) as u32;
    let thumbnail_height = (height as f32 * scale_factor).max(1.0) as u32;
    
    // Fast manual nearest-neighbor downsampling
    let mut thumbnail_pixels = Vec::with_capacity((thumbnail_width * thumbnail_height * 4) as usize);
    
    let x_ratio = width as f32 / thumbnail_width as f32;
    let y_ratio = height as f32 / thumbnail_height as f32;
    
    for ty in 0..thumbnail_height {
        let sy = (ty as f32 * y_ratio) as u32;
        for tx in 0..thumbnail_width {
            let sx = (tx as f32 * x_ratio) as u32;
            let src_idx = ((sy * width + sx) * 4) as usize;
            thumbnail_pixels.extend_from_slice(&pixels[src_idx..src_idx + 4]);
        }
    }
    
    blurhash::encode(4, 3, thumbnail_width, thumbnail_height, &thumbnail_pixels).ok()
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


// ===== MIME & Extension Conversion Utilities =====
static EXT_TO_MIME: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // Images
    m.insert("png", "image/png");
    m.insert("jpg", "image/jpeg");
    m.insert("jpeg", "image/jpeg");
    m.insert("gif", "image/gif");
    m.insert("webp", "image/webp");
    m.insert("svg", "image/svg+xml");
    m.insert("bmp", "image/bmp");
    m.insert("ico", "image/x-icon");
    m.insert("tif", "image/tiff");
    m.insert("tiff", "image/tiff");

    // Raw images
    m.insert("dng", "image/x-adobe-dng");
    m.insert("cr2", "image/x-canon-cr2");
    m.insert("nef", "image/x-nikon-nef");
    m.insert("arw", "image/x-sony-arw");

    // Audio
    m.insert("wav", "audio/wav");
    m.insert("mp3", "audio/mp3");
    m.insert("flac", "audio/flac");
    m.insert("ogg", "audio/ogg");
    m.insert("m4a", "audio/mp4");
    m.insert("aac", "audio/aac");
    m.insert("wma", "audio/x-ms-wma");
    m.insert("opus", "audio/opus");

    // Video
    m.insert("mp4", "video/mp4");
    m.insert("webm", "video/webm");
    m.insert("mov", "video/quicktime");
    m.insert("avi", "video/x-msvideo");
    m.insert("mkv", "video/x-matroska");
    m.insert("flv", "video/x-flv");
    m.insert("wmv", "video/x-ms-wmv");
    m.insert("mpg", "video/mpeg");
    m.insert("mpeg", "video/mpeg");
    m.insert("3gp", "video/3gpp");
    m.insert("ogv", "video/ogg");
    m.insert("ts", "video/mp2t");

    // Documents
    m.insert("pdf", "application/pdf");
    m.insert("doc", "application/msword");
    m.insert("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document");
    m.insert("xls", "application/vnd.ms-excel");
    m.insert("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
    m.insert("ppt", "application/vnd.ms-powerpoint");
    m.insert("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation");
    m.insert("odt", "application/vnd.oasis.opendocument.text");
    m.insert("ods", "application/vnd.oasis.opendocument.spreadsheet");
    m.insert("odp", "application/vnd.oasis.opendocument.presentation");
    m.insert("rtf", "application/rtf");

    // Text/Data
    m.insert("txt", "text/plain");
    m.insert("md", "text/markdown");
    m.insert("csv", "text/csv");
    m.insert("json", "application/json");
    m.insert("xml", "application/xml");
    m.insert("yaml", "application/x-yaml");
    m.insert("yml", "application/x-yaml");
    m.insert("toml", "application/toml");
    m.insert("sql", "application/sql");

    // Archives
    m.insert("zip", "application/zip");
    m.insert("rar", "application/vnd.rar");
    m.insert("7z", "application/x-7z-compressed");
    m.insert("tar", "application/x-tar");
    m.insert("gz", "application/gzip");
    m.insert("bz2", "application/x-bzip2");
    m.insert("xz", "application/x-xz");
    m.insert("iso", "application/x-iso9660-image");
    m.insert("dmg", "application/x-apple-diskimage");
    m.insert("apk", "application/vnd.android.package-archive");
    m.insert("jar", "application/java-archive");

    // 3D
    m.insert("obj", "model/obj");
    m.insert("gltf", "model/gltf+json");
    m.insert("glb", "model/gltf-binary");
    m.insert("stl", "model/stl");
    m.insert("dae", "model/vnd.collada+xml");

    // Code
    m.insert("js", "text/javascript");
    m.insert("ts", "text/typescript");
    m.insert("py", "text/x-python");
    m.insert("rs", "text/x-rust");
    m.insert("go", "text/x-go");
    m.insert("java", "text/x-java");
    m.insert("c", "text/x-c");
    m.insert("cpp", "text/x-c++");
    m.insert("cs", "text/x-csharp");
    m.insert("rb", "text/x-ruby");
    m.insert("php", "text/x-php");
    m.insert("swift", "text/x-swift");

    // Web
    m.insert("html", "text/html");
    m.insert("css", "text/css");

    // Other
    m.insert("exe", "application/x-msdownload");
    m.insert("msi", "application/x-msi");
    m.insert("ttf", "font/ttf");
    m.insert("otf", "font/otf");
    m.insert("woff", "font/woff");
    m.insert("woff2", "font/woff2");

    m
});

static MIME_TO_EXT: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // Images
    m.insert("image/png", "png");
    m.insert("image/jpeg", "jpg");
    m.insert("image/jpg", "jpg");
    m.insert("image/gif", "gif");
    m.insert("image/webp", "webp");
    m.insert("image/svg+xml", "svg");
    m.insert("image/bmp", "bmp");
    m.insert("image/x-ms-bmp", "bmp");
    m.insert("image/x-icon", "ico");
    m.insert("image/vnd.microsoft.icon", "ico");
    m.insert("image/tiff", "tiff");

    // Raw Images
    m.insert("image/x-adobe-dng", "dng");
    m.insert("image/x-canon-cr2", "cr2");
    m.insert("image/x-nikon-nef", "nef");
    m.insert("image/x-sony-arw", "arw");

    // Audio
    m.insert("audio/wav", "wav");
    m.insert("audio/x-wav", "wav");
    m.insert("audio/wave", "wav");
    m.insert("audio/mp3", "mp3");
    m.insert("audio/mpeg", "mp3");
    m.insert("audio/flac", "flac");
    m.insert("audio/ogg", "ogg");
    m.insert("audio/mp4", "m4a");
    m.insert("audio/aac", "aac");
    m.insert("audio/x-aac", "aac");
    m.insert("audio/x-ms-wma", "wma");
    m.insert("audio/opus", "opus");

    // Video
    m.insert("video/mp4", "mp4");
    m.insert("video/webm", "webm");
    m.insert("video/quicktime", "mov");
    m.insert("video/x-msvideo", "avi");
    m.insert("video/x-matroska", "mkv");
    m.insert("video/x-flv", "flv");
    m.insert("video/x-ms-wmv", "wmv");
    m.insert("video/mpeg", "mpg");
    m.insert("video/3gpp", "3gp");
    m.insert("video/ogg", "ogv");
    m.insert("video/mp2t", "ts");

    // Documents
    m.insert("application/pdf", "pdf");
    m.insert("application/msword", "doc");
    m.insert("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "docx");
    m.insert("application/vnd.ms-excel", "xls");
    m.insert("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "xlsx");
    m.insert("application/vnd.ms-powerpoint", "ppt");
    m.insert("application/vnd.openxmlformats-officedocument.presentationml.presentation", "pptx");
    m.insert("application/vnd.oasis.opendocument.text", "odt");
    m.insert("application/vnd.oasis.opendocument.spreadsheet", "ods");
    m.insert("application/vnd.oasis.opendocument.presentation", "odp");
    m.insert("application/rtf", "rtf");

    // Text/Data
    m.insert("text/plain", "txt");
    m.insert("text/markdown", "md");
    m.insert("text/csv", "csv");
    m.insert("application/json", "json");
    m.insert("application/xml", "xml");
    m.insert("text/xml", "xml");
    m.insert("application/x-yaml", "yaml");
    m.insert("text/yaml", "yaml");
    m.insert("application/toml", "toml");
    m.insert("application/sql", "sql");

    // Archives
    m.insert("application/zip", "zip");
    m.insert("application/x-rar-compressed", "rar");
    m.insert("application/vnd.rar", "rar");
    m.insert("application/x-7z-compressed", "7z");
    m.insert("application/x-tar", "tar");
    m.insert("application/gzip", "gz");
    m.insert("application/x-bzip2", "bz2");
    m.insert("application/x-xz", "xz");
    m.insert("application/x-iso9660-image", "iso");
    m.insert("application/x-apple-diskimage", "dmg");
    m.insert("application/vnd.android.package-archive", "apk");
    m.insert("application/java-archive", "jar");

    // 3D
    m.insert("model/obj", "obj");
    m.insert("model/gltf+json", "gltf");
    m.insert("model/gltf-binary", "glb");
    m.insert("model/stl", "stl");
    m.insert("application/sla", "stl");
    m.insert("model/vnd.collada+xml", "dae");

    // Code
    m.insert("text/javascript", "js");
    m.insert("application/javascript", "js");
    m.insert("text/typescript", "ts");
    m.insert("application/typescript", "ts");
    m.insert("text/x-python", "py");
    m.insert("application/x-python", "py");
    m.insert("text/x-rust", "rs");
    m.insert("text/x-go", "go");
    m.insert("text/x-java", "java");
    m.insert("text/x-c", "c");
    m.insert("text/x-c++", "cpp");
    m.insert("text/x-csharp", "cs");
    m.insert("text/x-ruby", "rb");
    m.insert("text/x-php", "php");
    m.insert("text/x-swift", "swift");

    // Web
    m.insert("text/html", "html");
    m.insert("text/css", "css");

    // Other
    m.insert("application/x-msdownload", "exe");
    m.insert("application/x-dosexec", "exe");
    m.insert("application/x-msi", "msi");
    m.insert("application/x-font-ttf", "ttf");
    m.insert("font/ttf", "ttf");
    m.insert("application/x-font-otf", "otf");
    m.insert("font/otf", "otf");
    m.insert("font/woff", "woff");
    m.insert("font/woff2", "woff2");

    m
});

/// Convert a file extension (with or without leading dot) to a MIME type.
/// Returns "application/octet-stream" when unknown.
pub fn mime_from_extension(extension: &str) -> String {
    let ext = extension.trim().trim_start_matches('.').to_lowercase();
    EXT_TO_MIME
        .get(ext.as_str())
        .copied()
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// Convert a MIME type to a file extension.
/// Falls back to using the MIME subtype when unknown (e.g. "application/x-foo" -> "x-foo"),
/// and "bin" if MIME is malformed.
pub fn extension_from_mime(mime: &str) -> String {
    let lower = mime.trim().to_lowercase();

    if let Some(ext) = MIME_TO_EXT.get(lower.as_str()) {
        return (*ext).to_string();
    }

    // Fallback: best-effort subtype extraction, mirroring previous behavior
    lower
        .split('/')
        .nth(1)
        .unwrap_or("bin")
        .to_string()
}

/// Convert a file extension (with or without leading dot) to a MIME type,
/// with an optional restriction to image/* MIME types.
///
/// If `image_only` is true and the extension maps to a non-image MIME type,
/// returns `Err` with the invalid MIME type.
pub fn mime_from_extension_safe(extension: &str, image_only: bool) -> Result<String, String> {
    let mime = mime_from_extension(extension);
    
    if image_only && !is_image_mime(&mime) {
        return Err(mime);
    }
    
    Ok(mime)
}

/// Returns true if the provided MIME type is an image/*
pub fn is_image_mime(mime: &str) -> bool {
    mime.trim().starts_with("image/")
}
