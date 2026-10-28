fn main() {
    // unrar's C++ sources call registry and CryptoAPI functions from advapi32 but the unrar_sys
    // crate does not ask for it, so binaries using the `rar` feature fail to link on Windows.
    if std::env::var_os("CARGO_FEATURE_RAR").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
