fn main() {
    let statik = std::env::var("RENPAK_STATIC").is_ok();

    // Try pkg-config (Linux/macOS)
    if pkg_config::Config::new()
        .atleast_version("1.0")
        .statik(statik)
        .probe("libavif")
        .is_ok()
    {
        // libavif's .pc may not list rav1e in Libs.private, so link it explicitly
        if statik {
            let _ = pkg_config::Config::new().statik(true).probe("rav1e");
        }
        return;
    }

    // Fallback: manual linking via AVIF_PREFIX (Windows or custom builds)
    if let Ok(prefix) = std::env::var("AVIF_PREFIX") {
        println!("cargo:rustc-link-search=native={prefix}/lib");
        if statik {
            println!("cargo:rustc-link-lib=static=avif");
            // rav1e static lib
            if let Ok(rav1e) = std::env::var("RAV1E_PREFIX") {
                println!("cargo:rustc-link-search=native={rav1e}/lib");
            }
            println!("cargo:rustc-link-lib=static=rav1e");
            // Windows system libs needed for static linking
            if cfg!(target_os = "windows") {
                println!("cargo:rustc-link-lib=ws2_32");
                println!("cargo:rustc-link-lib=userenv");
                println!("cargo:rustc-link-lib=bcrypt");
                println!("cargo:rustc-link-lib=ntdll");
            }
        } else {
            println!("cargo:rustc-link-lib=avif");
        }
        return;
    }

    panic!("libavif not found — install libavif-dev, or set AVIF_PREFIX=/path/to/prefix");
}
