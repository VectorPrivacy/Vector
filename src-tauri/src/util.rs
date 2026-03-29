use std::sync::LazyLock;
use std::collections::HashMap;
use fast_thumbhash::{rgba_to_thumb_hash, thumb_hash_to_rgba, base91_encode, base91_decode};

// Re-export from vector-core (supersedes local MIME HashMaps)
pub use vector_core::crypto::{
    extension_from_mime, mime_from_extension_safe, is_image_mime,
    mime_from_magic_bytes, format_bytes,
};

/// Convert a file extension to a MIME type. Returns owned String for backward compatibility.
pub fn mime_from_extension(extension: &str) -> String {
    vector_core::crypto::mime_from_extension(extension).to_string()
}

/// Build a `data:{mime};base64,{...}` URI with a single pre-allocated buffer.
/// Uses SIMD-accelerated base64 encoding (NEON/AVX2).
#[inline]
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    // "data:" + mime + ";base64," = 5 + mime.len() + 8
    let prefix_len = 13 + mime.len();
    let base64_len = (bytes.len() + 2) / 3 * 4;
    let mut result = String::with_capacity(prefix_len + base64_len);
    result.push_str("data:");
    result.push_str(mime);
    result.push_str(";base64,");
    base64_simd::STANDARD.encode_append(bytes, &mut result);
    result
}

pub use vector_core::hex::{
    bytes_to_hex_16, bytes_to_hex_32, bytes_to_hex_string,
    hex_string_to_bytes, hex_to_bytes_16, hex_to_bytes_32,
};

pub use crate::simd::{
    has_alpha_transparency, set_all_alpha_opaque,
};

#[cfg(target_os = "windows")]
pub use crate::simd::has_all_alpha_near_zero;

/// Extract all HTTPS URLs from a string
pub fn extract_https_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut start_idx = 0;

    while let Some(https_idx) = text[start_idx..].find("https://") {
        let abs_start = start_idx + https_idx;
        let url_text = &text[abs_start..];

        // Find the end of the URL (SIMD-accelerated: 4.7-5.2x faster than scalar)
        let mut end_idx = crate::simd::url::find_url_delimiter(url_text.as_bytes());

        // Trim trailing punctuation (ASCII-only, so byte access is safe)
        while end_idx > 0 {
            let last_byte = url_text.as_bytes()[end_idx - 1];
            if last_byte == b'.' || last_byte == b',' || last_byte == b':' || last_byte == b';' {
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
    static FILE_TYPES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
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
        map.insert("md", "Markdown File");
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

/// Calculate SHA-256 hash of file data
pub fn calculate_file_hash(data: &[u8]) -> String {
    vector_core::crypto::sha256_hex(data)
}

/// Ultra-fast nearest-neighbor downsampling for RGBA8 pixel data
///
/// This is significantly faster than image crate's resize functions because:
/// - No interpolation calculations (just picks nearest pixel)
/// - No filter kernel convolutions
/// - Simple memory access pattern
///
/// # Arguments
/// * `pixels` - Source RGBA8 pixel data (4 bytes per pixel)
/// * `src_width` - Source image width
/// * `src_height` - Source image height
/// * `dst_width` - Target width
/// * `dst_height` - Target height
///
/// # Returns
/// Downsampled RGBA8 pixel data
/// Fast nearest-neighbor downsampling for RGBA images.
///
/// Delegates to SIMD-optimized implementation in `crate::simd::image`.
#[inline]
pub fn nearest_neighbor_downsample(
    pixels: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Vec<u8> {
    crate::simd::image::nearest_neighbor_downsample(pixels, src_width, src_height, dst_width, dst_height)
}

/// Generate a thumbhash from RGBA8 image data.
///
/// Downscales to fit within 100x100 (ThumbHash's max) while preserving aspect ratio.
/// Returns the base91-encoded thumbhash string, or None if encoding fails.
pub fn generate_thumbhash_from_rgba(pixels: &[u8], width: u32, height: u32) -> Option<String> {
    const MAX_DIM: u32 = 100;

    let (thumb_w, thumb_h) = if width <= MAX_DIM && height <= MAX_DIM {
        (width, height)
    } else if width > height {
        (MAX_DIM, (MAX_DIM * height / width).max(1))
    } else {
        ((MAX_DIM * width / height).max(1), MAX_DIM)
    };

    // Use fast nearest-neighbor downsampling
    let thumbnail_pixels = nearest_neighbor_downsample(pixels, width, height, thumb_w, thumb_h);

    let hash = rgba_to_thumb_hash(thumb_w as usize, thumb_h as usize, &thumbnail_pixels);
    Some(base91_encode(&hash))
}

/// Generate a thumbhash from a DynamicImage with minimal memory allocation.
///
/// Resizes to a small thumbnail before converting to RGBA,
/// avoiding large temporary allocations for high-resolution images.
#[inline]
pub fn generate_thumbhash_from_image(img: &image::DynamicImage) -> Option<String> {
    const THUMBHASH_SIZE: u32 = 100;

    let (width, height) = (img.width(), img.height());

    // Calculate thumbnail dimensions maintaining aspect ratio
    let (thumb_w, thumb_h) = if width > height {
        (THUMBHASH_SIZE, (THUMBHASH_SIZE * height / width).max(1))
    } else {
        ((THUMBHASH_SIZE * width / height).max(1), THUMBHASH_SIZE)
    };

    let thumbnail = img.thumbnail(thumb_w, thumb_h);
    let rgba = thumbnail.to_rgba8();
    let hash = rgba_to_thumb_hash(rgba.width() as usize, rgba.height() as usize, rgba.as_raw());
    Some(base91_encode(&hash))
}

/// Decode a thumbhash string to a Base64-encoded PNG data URL
/// Returns a data URL string that can be used directly in an <img> src attribute
pub fn decode_thumbhash_to_base64(thumbhash: &str) -> String {
    const EMPTY_DATA_URL: &str = "data:image/png;base64,";

    if thumbhash.is_empty() {
        return EMPTY_DATA_URL.to_string();
    }

    let hash_bytes = match base91_decode(thumbhash) {
        Ok(bytes) => bytes,
        Err(_) => {
            eprintln!("Failed to decode thumbhash base91");
            return EMPTY_DATA_URL.to_string();
        }
    };

    let (w, h, rgba_data) = match thumb_hash_to_rgba(&hash_bytes) {
        Ok(result) => result,
        Err(_) => {
            eprintln!("Failed to decode thumbhash");
            return EMPTY_DATA_URL.to_string();
        }
    };
    encode_rgba_to_png_base64(&rgba_data, w as u32, h as u32)
}

/// Helper function to encode RGBA data to PNG base64
#[inline]
fn encode_rgba_to_png_base64(rgba_data: &[u8], width: u32, height: u32) -> String {
    const EMPTY_DATA_URL: &str = "data:image/png;base64,";

    // Use shared encode_png - no need to clone data into RgbaImage
    let png_data = match crate::shared::image::encode_png(rgba_data, width, height) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Failed to encode PNG: {}", e);
            return EMPTY_DATA_URL.to_string();
        }
    };

    data_uri("image/png", &png_data)
}

/// Zero-allocation MIME lookup — wraps vector-core's match-based implementation.
/// Handles leading dots and whitespace for backward compatibility.
pub fn mime_from_extension_static(extension: &str) -> &'static str {
    let ext = extension.trim().trim_start_matches('.');
    vector_core::crypto::mime_from_extension(ext)
}
