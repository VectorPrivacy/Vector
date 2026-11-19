//! The desktop client's JNI glue for calls and the microphone, compiled from its sources, over
//! the activity and services in `crates/vector-gpui/android`.

#[path = "../../../../../src-tauri/src/android/utils.rs"]
#[allow(dead_code)]
pub mod utils;
#[path = "../../../../../src-tauri/src/android/permissions.rs"]
pub mod permissions;
#[path = "../../../../../src-tauri/src/android/call_audio.rs"]
#[allow(dead_code)]
pub mod call_audio;
#[path = "../../../../../src-tauri/src/android/call_service.rs"]
pub mod call_service;
#[path = "../../../../../src-tauri/src/android/incoming_call.rs"]
#[allow(dead_code)]
pub mod incoming_call;
#[path = "../../../../../src-tauri/src/android/storage.rs"]
#[allow(dead_code)]
pub mod storage;
#[path = "../../../../../src-tauri/src/android/biometric.rs"]
pub mod biometric;
#[path = "../../../../../src-tauri/src/android/external_signer.rs"]
pub mod external_signer;
#[path = "../../../../../src-tauri/src/android/updates.rs"]
pub mod updates;
pub mod background_sync;

use utils::with_android_context;
