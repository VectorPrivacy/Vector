use jni::objects::{JObject, JByteArray};
use jni::JNIEnv;
use image::{ImageBuffer, Rgba};

use super::utils::{with_android_context, get_system_service, STREAM_BUFFER_SIZE};

pub fn read_image_from_clipboard() -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>, String> {
    with_android_context(|env, activity| {
        // Get ClipboardManager using the helper
        let clipboard_manager = get_system_service(env, activity, "clipboard")?;
        get_clipboard_image(env, &clipboard_manager, activity)
    })
}

fn get_clipboard_image(
    env: &mut JNIEnv, 
    clipboard_manager: &JObject,
    context: &JObject
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>, String> {
    
    // Check if clipboard has content
    let has_primary_clip = env.call_method(
        &clipboard_manager,
        "hasPrimaryClip",
        "()Z",
        &[],
    ).map_err(|e| e.to_string())?
    .z().map_err(|e| e.to_string())?;
    
    if !has_primary_clip {
        return Err("No content in clipboard".to_string());
    }
    
    // Get primary clip
    let clip = env.call_method(
        &clipboard_manager,
        "getPrimaryClip",
        "()Landroid/content/ClipData;",
        &[],
    ).map_err(|e| e.to_string())?
    .l().map_err(|e| e.to_string())?;
    
    // Get item at index 0
    let item = env.call_method(
        &clip,
        "getItemAt",
        "(I)Landroid/content/ClipData$Item;",
        &[0i32.into()],
    ).map_err(|e| e.to_string())?
    .l().map_err(|e| e.to_string())?;
    
    // Get URI from item
    let uri = env.call_method(
        &item,
        "getUri",
        "()Landroid/net/Uri;",
        &[],
    ).map_err(|e| e.to_string())?
    .l().map_err(|e| e.to_string())?;
    
    if uri.is_null() {
        return Err("No image URI in clipboard".to_string());
    }
    
    // Read image data from URI
    read_image_from_uri(env, context, &uri)
}

fn read_image_from_uri(env: &mut JNIEnv, context: &JObject, uri: &JObject) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>, String> {
    // Get ContentResolver
    let content_resolver = env.call_method(
        context,
        "getContentResolver",
        "()Landroid/content/ContentResolver;",
        &[],
    ).map_err(|e| e.to_string())?
    .l().map_err(|e| e.to_string())?;
    
    // Open InputStream
    let input_stream = env.call_method(
        &content_resolver,
        "openInputStream",
        "(Landroid/net/Uri;)Ljava/io/InputStream;",
        &[uri.into()],
    ).map_err(|e| format!("Failed to open input stream: {:?}", e))?
    .l().map_err(|e| e.to_string())?;
    
    if input_stream.is_null() {
        return Err("Failed to open input stream".to_string());
    }
    
    // Read all bytes from InputStream
    let byte_array_output_stream_class = env.find_class("java/io/ByteArrayOutputStream")
        .map_err(|e| e.to_string())?;
    let baos = env.new_object(byte_array_output_stream_class, "()V", &[])
        .map_err(|e| e.to_string())?;
    
    // Create buffer for reading
    let buffer = env.new_byte_array(STREAM_BUFFER_SIZE).map_err(|e| e.to_string())?;
    
    // Read loop
    loop {
        let bytes_read = env.call_method(
            &input_stream,
            "read",
            "([B)I",
            &[(&buffer).into()],
        ).map_err(|e| e.to_string())?
        .i().map_err(|e| e.to_string())?;
        
        if bytes_read <= 0 {
            break;
        }
        
        env.call_method(
            &baos,
            "write",
            "([BII)V",
            &[(&buffer).into(), 0i32.into(), bytes_read.into()],
        ).map_err(|e| e.to_string())?;
    }
    
    // Close input stream
    let _ = env.call_method(&input_stream, "close", "()V", &[]);
    
    // Get byte array
    let byte_array = env.call_method(
        &baos,
        "toByteArray",
        "()[B",
        &[],
    ).map_err(|e| e.to_string())?
    .l().map_err(|e| e.to_string())?;
    
    // Convert JObject to JByteArray
    let byte_array = unsafe { JByteArray::from_raw(byte_array.into_raw()) };
    
    // Convert to Rust Vec<u8>
    let bytes = env.convert_byte_array(&byte_array)
        .map_err(|e| format!("Failed to convert byte array: {:?}", e))?;
    
    // Decode image
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to decode image: {:?}", e))?;
    
    Ok(img.to_rgba8())
}