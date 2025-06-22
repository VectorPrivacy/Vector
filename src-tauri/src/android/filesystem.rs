use jni::objects::{JObject, JValue, JString};

use crate::message::AttachmentFile;
use super::utils::{with_android_context, get_content_resolver, STREAM_BUFFER_SIZE};

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

    // Parse URI
    let uri_string = env.new_string(uri)?;
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
        bytes,
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
