fn main() {
    // When statically linking OpenSSL on Windows (required by bundled-sqlcipher
    // in mdk-sqlite-storage), the linker needs additional Windows system
    // libraries that OpenSSL's libcrypto depends on. The libsqlite3-sys build
    // script links libcrypto but doesn't emit these system library dependencies.
    //
    // - crypt32: Certificate store functions (CertOpenStore, CertCloseStore, etc.)
    // - user32: Window station and message box functions (GetProcessWindowStation,
    //   GetUserObjectInformationW, MessageBoxW)
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        println!("cargo:rustc-link-lib=crypt32");
        println!("cargo:rustc-link-lib=user32");
    }
}
