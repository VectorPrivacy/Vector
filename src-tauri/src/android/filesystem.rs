use std::sync::Arc;
use jni::objects::{JObject, JValue, JString};

use crate::message::{AttachmentFile, FileInfo};
use super::utils::{with_android_context, get_content_resolver, STREAM_BUFFER_SIZE};

/// Simple percent-decoding for URIs (e.g., %3A -> :)
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '%' {
            // Try to read two hex digits
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            // If decoding failed, keep the original
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(c);
        }
    }
    
    result
}

/// Get file info from an Android content URI
pub fn get_android_uri_info(uri: String) -> Result<FileInfo, String> {
    with_android_context(|env, activity| {
        let content_resolver = get_content_resolver(env, activity)?;
        get_android_uri_info_internal(env, &content_resolver, &uri)
            .map_err(|e| format!("Failed to get URI info: {:?}", e))
    })
}

fn get_android_uri_info_internal(
    env: &mut jni::JNIEnv,
    content_resolver: &JObject,
    uri: &str,
) -> Result<FileInfo, Box<dyn std::error::Error>> {
    // URL decode the URI in case it's encoded (e.g., %3A -> :)
    let decoded_uri = percent_decode(uri);
    
    // Parse URI
    let uri_string = env.new_string(&decoded_uri)?;
    let uri_class = env.find_class("android/net/Uri")?;
    let uri_object = env.call_static_method(
        &uri_class,
        "parse",
        "(Ljava/lang/String;)Landroid/net/Uri;",
        &[JValue::Object(&uri_string)],
    )?.l()?;

    // Query for file info using ContentResolver.query()
    // Projection: OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE
    let display_name_field = env.new_string("_display_name")?;
    let size_field = env.new_string("_size")?;
    
    // Create String array for projection
    let string_class = env.find_class("java/lang/String")?;
    let projection = env.new_object_array(2, &string_class, JObject::null())?;
    env.set_object_array_element(&projection, 0, &display_name_field)?;
    env.set_object_array_element(&projection, 1, &size_field)?;

    // Query the content resolver
    let cursor = env.call_method(
        content_resolver,
        "query",
        "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
        &[
            JValue::Object(&uri_object),
            JValue::Object(&projection),
            JValue::Object(&JObject::null()),
            JValue::Object(&JObject::null()),
            JValue::Object(&JObject::null()),
        ],
    )?.l()?;

    if cursor.is_null() {
        return Err("Failed to query content resolver".into());
    }

    // Move to first row
    let has_data = env.call_method(&cursor, "moveToFirst", "()Z", &[])?.z()?;
    
    let (name, size) = if has_data {
        // Get column indices
        let name_col_str = env.new_string("_display_name")?;
        let name_col_idx = env.call_method(
            &cursor,
            "getColumnIndex",
            "(Ljava/lang/String;)I",
            &[JValue::Object(&name_col_str)],
        )?.i()?;

        let size_col_str = env.new_string("_size")?;
        let size_col_idx = env.call_method(
            &cursor,
            "getColumnIndex",
            "(Ljava/lang/String;)I",
            &[JValue::Object(&size_col_str)],
        )?.i()?;

        // Get display name
        let name = if name_col_idx >= 0 {
            let name_obj = env.call_method(
                &cursor,
                "getString",
                "(I)Ljava/lang/String;",
                &[JValue::Int(name_col_idx)],
            )?.l()?;
            
            if !name_obj.is_null() {
                let jstring = JString::from(name_obj);
                let name_str: String = env.get_string(&jstring)?.into();
                name_str
            } else {
                "unknown".to_string()
            }
        } else {
            "unknown".to_string()
        };

        // Get size
        let size = if size_col_idx >= 0 {
            env.call_method(
                &cursor,
                "getLong",
                "(I)J",
                &[JValue::Int(size_col_idx)],
            )?.j()? as u64
        } else {
            0
        };

        (name, size)
    } else {
        ("unknown".to_string(), 0)
    };

    // Close cursor
    let _ = env.call_method(&cursor, "close", "()V", &[]);

    // Extract extension from name
    let mut extension = name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();
    
    // If no extension found in filename, try to get it from MIME type
    if extension.is_empty() || extension == name.to_lowercase() {
        // Get MIME type from ContentResolver
        let mime_type_obj = env.call_method(
            content_resolver,
            "getType",
            "(Landroid/net/Uri;)Ljava/lang/String;",
            &[JValue::Object(&uri_object)],
        )?.l()?;

        if !mime_type_obj.is_null() {
            let jstring = JString::from(mime_type_obj);
            let mime_type: String = env.get_string(&jstring)?.into();
            
            // Use MimeTypeMap to get extension from MIME type
            if let Some(ext) = get_extension_from_mime_type(env, &mime_type)? {
                extension = ext;
            }
        }
    }

    Ok(FileInfo {
        size,
        name,
        extension,
    })
}

/// Try to take persistable URI permission (best effort, may fail silently)
fn try_take_persistable_permission(
    env: &mut jni::JNIEnv,
    content_resolver: &JObject,
    uri_object: &JObject,
) {
    // Try to take persistable read permission
    // This may fail if the URI doesn't support it, which is fine
    let flag_read = 1i32; // Intent.FLAG_GRANT_READ_URI_PERMISSION
    let _ = env.call_method(
        content_resolver,
        "takePersistableUriPermission",
        "(Landroid/net/Uri;I)V",
        &[JValue::Object(uri_object), JValue::Int(flag_read)],
    );
}

/// Read raw bytes from an Android content URI (for compression)
pub fn read_android_uri_bytes(uri: String) -> Result<(Vec<u8>, String), String> {
    with_android_context(|env, activity| {
        let content_resolver = get_content_resolver(env, activity)?;
        read_android_uri_bytes_internal(env, &content_resolver, &uri)
            .map_err(|e| format!("Failed to read URI bytes: {:?}", e))
    })
}

fn read_android_uri_bytes_internal(
    env: &mut jni::JNIEnv,
    content_resolver: &JObject,
    uri: &str,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    // URL decode the URI in case it's encoded
    let decoded_uri = percent_decode(uri);
    
    // Parse URI
    let uri_string = env.new_string(&decoded_uri)?;
    let uri_class = env.find_class("android/net/Uri")?;
    let uri_object = env.call_static_method(
        &uri_class,
        "parse",
        "(Ljava/lang/String;)Landroid/net/Uri;",
        &[JValue::Object(&uri_string)],
    )?.l()?;

    // Try to take persistable permission (best effort)
    try_take_persistable_permission(env, content_resolver, &uri_object);

    // Get file name to extract extension
    let display_name_field = env.new_string("_display_name")?;
    let string_class = env.find_class("java/lang/String")?;
    let projection = env.new_object_array(1, &string_class, JObject::null())?;
    env.set_object_array_element(&projection, 0, &display_name_field)?;

    let cursor = env.call_method(
        content_resolver,
        "query",
        "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
        &[
            JValue::Object(&uri_object),
            JValue::Object(&projection),
            JValue::Object(&JObject::null()),
            JValue::Object(&JObject::null()),
            JValue::Object(&JObject::null()),
        ],
    )?.l()?;

    let extension = if !cursor.is_null() {
        let has_data = env.call_method(&cursor, "moveToFirst", "()Z", &[])?.z()?;
        let ext = if has_data {
            let name_col_str = env.new_string("_display_name")?;
            let name_col_idx = env.call_method(
                &cursor,
                "getColumnIndex",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&name_col_str)],
            )?.i()?;

            if name_col_idx >= 0 {
                let name_obj = env.call_method(
                    &cursor,
                    "getString",
                    "(I)Ljava/lang/String;",
                    &[JValue::Int(name_col_idx)],
                )?.l()?;
                
                if !name_obj.is_null() {
                    let jstring = JString::from(name_obj);
                    let name: String = env.get_string(&jstring)?.into();
                    name.rsplit('.').next().unwrap_or("bin").to_lowercase()
                } else {
                    "bin".to_string()
                }
            } else {
                "bin".to_string()
            }
        } else {
            "bin".to_string()
        };
        let _ = env.call_method(&cursor, "close", "()V", &[]);
        ext
    } else {
        "bin".to_string()
    };

    // Open InputStream and read bytes
    let input_stream = env.call_method(
        content_resolver,
        "openInputStream",
        "(Landroid/net/Uri;)Ljava/io/InputStream;",
        &[JValue::Object(&uri_object)],
    )?.l()?;

    if input_stream.is_null() {
        return Err("Failed to open input stream".into());
    }

    let mut bytes = Vec::new();
    let buffer = env.new_byte_array(STREAM_BUFFER_SIZE)?;

    loop {
        let bytes_read = env.call_method(
            &input_stream,
            "read",
            "([B)I",
            &[JValue::Object(&buffer)],
        )?.i()?;

        if bytes_read == -1 {
            break;
        }

        if bytes_read > 0 {
            let mut temp_buffer = vec![0i8; bytes_read as usize];
            env.get_byte_array_region(&buffer, 0, &mut temp_buffer)?;
            let u8_buffer: Vec<u8> = temp_buffer.into_iter().map(|b| b as u8).collect();
            bytes.extend_from_slice(&u8_buffer);
        }
    }

    let _ = env.call_method(&input_stream, "close", "()V", &[]);

    Ok((bytes, extension))
}

pub fn read_android_uri(uri: String) -> Result<AttachmentFile, String> {
    with_android_context(|env, activity| {
        let content_resolver = get_content_resolver(env, activity)?;
        read_from_android_uri_internal(env, &content_resolver, &uri)
            .map_err(|e| format!("Failed to read URI: {:?}", e))
    })
}

fn read_from_android_uri_internal(
    env: &mut jni::JNIEnv,
    content_resolver: &JObject,
    uri: &str,
) -> Result<AttachmentFile, Box<dyn std::error::Error>> {
    // URL decode the URI in case it's encoded
    let decoded_uri = percent_decode(uri);

    // Parse URI
    let uri_string = env.new_string(&decoded_uri)?;
    let uri_class = env.find_class("android/net/Uri")?;
    let uri_object = env.call_static_method(
        &uri_class,
        "parse",
        "(Ljava/lang/String;)Landroid/net/Uri;",
        &[JValue::Object(&uri_string)],
    )?.l()?;

    // Get MIME type
    let mime_type_obj = env.call_method(
        &content_resolver,
        "getType",
        "(Landroid/net/Uri;)Ljava/lang/String;",
        &[JValue::Object(&uri_object)],
    )?.l()?;

    let mime_type: Option<String> = if !mime_type_obj.is_null() {
        let jstring = JString::from(mime_type_obj);
        let string_value: String = env.get_string(&jstring)?.into();
        Some(string_value)
    } else {
        None
    };

    // Get extension from MIME type
    let extension = if let Some(ref mime) = mime_type {
        get_extension_from_mime_type(env, mime)?
            .unwrap_or_else(|| "bin".to_string())
    } else {
        "bin".to_string()
    };

    // Open InputStream
    let input_stream = env.call_method(
        &content_resolver,
        "openInputStream",
        "(Landroid/net/Uri;)Ljava/io/InputStream;",
        &[JValue::Object(&uri_object)],
    )?.l()?;

    if input_stream.is_null() {
        return Err("Failed to open input stream".into());
    }

    // Read data
    let mut bytes = Vec::new();
    let buffer = env.new_byte_array(STREAM_BUFFER_SIZE)?;

    loop {
        let bytes_read = env.call_method(
            &input_stream,
            "read",
            "([B)I",
            &[JValue::Object(&buffer)],
        )?.i()?;

        if bytes_read == -1 {
            break;
        }

        if bytes_read > 0 {
            let mut temp_buffer = vec![0i8; bytes_read as usize];
            env.get_byte_array_region(&buffer, 0, &mut temp_buffer)?;
            // Convert i8 to u8
            let u8_buffer: Vec<u8> = temp_buffer.into_iter()
                .map(|b| b as u8)
                .collect();
            bytes.extend_from_slice(&u8_buffer);
        }
    }

    // Close the stream
    let _ = env.call_method(&input_stream, "close", "()V", &[]);

    Ok(AttachmentFile {
        bytes: Arc::new(bytes),
        img_meta: None,
        extension,
    })
}

fn get_extension_from_mime_type(
    env: &mut jni::JNIEnv,
    mime_type: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    // Use MimeTypeMap to get extension
    let mime_type_map_class = env.find_class("android/webkit/MimeTypeMap")?;
    
    // Get singleton instance
    let mime_type_map = env.call_static_method(
        &mime_type_map_class,
        "getSingleton",
        "()Landroid/webkit/MimeTypeMap;",
        &[],
    )?.l()?;

    // Get extension from MIME type
    let mime_string = env.new_string(mime_type)?;
    let extension_obj = env.call_method(
        &mime_type_map,
        "getExtensionFromMimeType",
        "(Ljava/lang/String;)Ljava/lang/String;",
        &[JValue::Object(&mime_string)],
    )?.l()?;

    if !extension_obj.is_null() {
        let jstring = JString::from(extension_obj);
        let string_value: String = env.get_string(&jstring)?.into();
        Ok(Some(string_value))
    } else {
        Ok(None)
    }
}
