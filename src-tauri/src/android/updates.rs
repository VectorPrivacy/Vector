//! Install-source detection + store hand-off for the in-app update flow.

use jni::objects::{JClass, JObject, JValue};
use jni::JNIEnv;
use super::with_android_context;

fn load_class<'a>(
    env: &mut JNIEnv<'a>,
    activity: &JObject<'a>,
    name: &str,
) -> Result<JClass<'a>, String> {
    let class_loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .map_err(|e| format!("classloader: {:?}", e))?
        .l()
        .map_err(|e| format!("classloader obj: {:?}", e))?;
    let j_name = env
        .new_string(name.replace('/', "."))
        .map_err(|e| format!("class name str: {:?}", e))?;
    let cls = env
        .call_method(
            &class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&j_name)],
        )
        .map_err(|e| format!("loadClass {}: {:?}", name, e))?
        .l()
        .map_err(|e| format!("loaded class obj: {:?}", e))?;
    Ok(JClass::from(cls))
}

/// Hand a downloaded update APK to the system installer — but only after the
/// Kotlin side confirms it carries the SAME signing certificate as the running
/// app. Android would reject a mismatch anyway; checking here means the user
/// hears why instead of watching an install fail.
///
/// Returns the raw status: `ok`, `needs-permission` (the settings screen was
/// opened), `signature-mismatch`, `unverifiable`, `missing`.
pub fn install_update(path: &str) -> Result<String, String> {
    let path = path.to_string();
    with_android_context(|env, activity| {
        let cls = load_class(env, activity, "io/vectorapp/VectorUpdates")?;
        let j_path = env.new_string(&path).map_err(|e| format!("path str: {:?}", e))?;
        let res = env
            .call_static_method(
                &cls,
                "installUpdate",
                "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(activity), JValue::Object(&j_path)],
            )
            .map_err(|e| format!("installUpdate: {:?}", e))?
            .l()
            .map_err(|e| format!("installUpdate obj: {:?}", e))?;
        let s: String = env
            .get_string((&res).into())
            .map_err(|e| format!("installUpdate string: {:?}", e))?
            .into();
        Ok(s)
    })
}

/// Package name of whatever installed us (e.g. `dev.zapstore.app`), or
/// `None` for sideloads (browser download, file manager, adb).
pub fn get_installer_package() -> Result<Option<String>, String> {
    with_android_context(|env, ctx| {
        let pm = env
            .call_method(ctx, "getPackageManager", "()Landroid/content/pm/PackageManager;", &[])
            .map_err(|e| format!("getPackageManager: {:?}", e))?
            .l()
            .map_err(|e| format!("PackageManager obj: {:?}", e))?;
        let own = env
            .call_method(ctx, "getPackageName", "()Ljava/lang/String;", &[])
            .map_err(|e| format!("getPackageName: {:?}", e))?
            .l()
            .map_err(|e| format!("package name obj: {:?}", e))?;
        // Deprecated in API 30 but still answers on every supported level;
        // the InstallSourceInfo replacement would exclude API 26-29 devices.
        let installer = env
            .call_method(
                &pm,
                "getInstallerPackageName",
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&own)],
            )
            .map_err(|e| format!("getInstallerPackageName: {:?}", e))?
            .l()
            .map_err(|e| format!("installer obj: {:?}", e))?;
        if installer.is_null() {
            return Ok(None);
        }
        let s: String = env
            .get_string((&installer).into())
            .map_err(|e| format!("installer string: {:?}", e))?
            .into();
        Ok(Some(s))
    })
}

/// Human-readable app name for a package (e.g. `dev.zapstore.app` ->
/// "Zapstore"). `None` when the package isn't installed. Lets the update
/// button name whatever store shipped the build without a hardcoded table.
pub fn get_app_label(package: &str) -> Result<Option<String>, String> {
    with_android_context(|env, ctx| {
        let pm = env
            .call_method(ctx, "getPackageManager", "()Landroid/content/pm/PackageManager;", &[])
            .map_err(|e| format!("getPackageManager: {:?}", e))?
            .l()
            .map_err(|e| format!("PackageManager obj: {:?}", e))?;
        let pkg = env
            .new_string(package)
            .map_err(|e| format!("package str: {:?}", e))?;
        // getApplicationInfo throws NameNotFoundException for a stale
        // installer whose app was since removed -> treat as unknown name.
        let app_info = match env.call_method(
            &pm,
            "getApplicationInfo",
            "(Ljava/lang/String;I)Landroid/content/pm/ApplicationInfo;",
            &[(&pkg).into(), JValue::Int(0)],
        ) {
            Ok(v) => v.l().map_err(|e| format!("ApplicationInfo obj: {:?}", e))?,
            Err(_) => {
                let _ = env.exception_clear();
                return Ok(None);
            }
        };
        let label = env
            .call_method(
                &pm,
                "getApplicationLabel",
                "(Landroid/content/pm/ApplicationInfo;)Ljava/lang/CharSequence;",
                &[JValue::Object(&app_info)],
            )
            .map_err(|e| format!("getApplicationLabel: {:?}", e))?
            .l()
            .map_err(|e| format!("label obj: {:?}", e))?;
        let label_str = env
            .call_method(&label, "toString", "()Ljava/lang/String;", &[])
            .map_err(|e| format!("label toString: {:?}", e))?
            .l()
            .map_err(|e| format!("label string obj: {:?}", e))?;
        let s: String = env
            .get_string((&label_str).into())
            .map_err(|e| format!("label get_string: {:?}", e))?
            .into();
        Ok(Some(s))
    })
}

/// Open a URI inside a specific app (ACTION_VIEW pinned to `package`).
/// Returns `false` when the app can't handle it (not installed, or no
/// matching intent filter). CLEAR_TASK is deliberate: Zapstore's engine
/// only reads deep links at activity launch, so a warm instance must be
/// relaunched or the link lands on its home screen.
pub fn open_url_in_app(package: &str, url: &str) -> Result<bool, String> {
    with_android_context(|env, ctx| {
        let action = env
            .new_string("android.intent.action.VIEW")
            .map_err(|e| format!("action str: {:?}", e))?;
        let url_j = env.new_string(url).map_err(|e| format!("url str: {:?}", e))?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[(&url_j).into()],
            )
            .map_err(|e| format!("Uri.parse: {:?}", e))?
            .l()
            .map_err(|e| format!("uri obj: {:?}", e))?;
        let intent = env
            .new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[(&action).into(), JValue::Object(&uri)],
            )
            .map_err(|e| format!("new Intent: {:?}", e))?;
        let pkg_j = env
            .new_string(package)
            .map_err(|e| format!("package str: {:?}", e))?;
        env.call_method(
            &intent,
            "setPackage",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[(&pkg_j).into()],
        )
        .map_err(|e| format!("setPackage: {:?}", e))?;
        const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;
        const FLAG_ACTIVITY_CLEAR_TASK: i32 = 0x0000_8000;
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(FLAG_ACTIVITY_NEW_TASK | FLAG_ACTIVITY_CLEAR_TASK)],
        )
        .map_err(|e| format!("addFlags: {:?}", e))?;
        // ActivityNotFoundException is the expected miss (app absent):
        // clear it so the pending exception can't poison later JNI calls.
        if env
            .call_method(ctx, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])
            .is_err()
        {
            let _ = env.exception_clear();
            return Ok(false);
        }
        Ok(true)
    })
}

/// Whether `package` can open Vector's store page — i.e. it registers a
/// `market://details` handler. This is what separates a real store from a
/// browser, file manager, or the system installer UI, any of which can be
/// recorded as the installer for a sideload yet resolve no store scheme.
/// The installer is always visible to us (Android auto-grants visibility of
/// whoever installed the app), so this resolves even under the API 30+
/// package-visibility rules without a `<queries>` declaration.
pub fn resolves_market_link(package: &str) -> Result<bool, String> {
    with_android_context(|env, ctx| {
        let pm = env
            .call_method(ctx, "getPackageManager", "()Landroid/content/pm/PackageManager;", &[])
            .map_err(|e| format!("getPackageManager: {:?}", e))?
            .l()
            .map_err(|e| format!("PackageManager obj: {:?}", e))?;
        let action = env
            .new_string("android.intent.action.VIEW")
            .map_err(|e| format!("action str: {:?}", e))?;
        let url_j = env
            .new_string("market://details?id=io.vectorapp")
            .map_err(|e| format!("url str: {:?}", e))?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[(&url_j).into()],
            )
            .map_err(|e| format!("Uri.parse: {:?}", e))?
            .l()
            .map_err(|e| format!("uri obj: {:?}", e))?;
        let intent = env
            .new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[(&action).into(), JValue::Object(&uri)],
            )
            .map_err(|e| format!("new Intent: {:?}", e))?;
        let pkg_j = env
            .new_string(package)
            .map_err(|e| format!("package str: {:?}", e))?;
        env.call_method(
            &intent,
            "setPackage",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[(&pkg_j).into()],
        )
        .map_err(|e| format!("setPackage: {:?}", e))?;
        let resolve = env
            .call_method(
                &pm,
                "resolveActivity",
                "(Landroid/content/Intent;I)Landroid/content/pm/ResolveInfo;",
                &[JValue::Object(&intent), JValue::Int(0)],
            )
            .map_err(|e| format!("resolveActivity: {:?}", e))?
            .l()
            .map_err(|e| format!("ResolveInfo obj: {:?}", e))?;
        Ok(!resolve.is_null())
    })
}
