fn main() {
    let statik = std::env::var("RENPAK_STATIC").is_ok();

    // Try pkg-config (Linux/macOS)
    if pkg_config::Config::new()
        .atleast_version("1.0")
        .statik(statik)
        .probe("libavif")
        .is_ok()
    {
        // libavif's .pc may not list aom in Libs.private, so link it explicitly
        if statik {
            let _ = pkg_config::Config::new().statik(true).probe("aom");
        }
        return;
    }

    // Fallback: manual linking via AVIF_PREFIX (Windows or custom builds)
    if let Ok(prefix) = std::env::var("AVIF_PREFIX") {
        println!("cargo:rustc-link-search=native={prefix}/lib");
        if statik {
            println!("cargo:rustc-link-lib=static=avif");
            if let Ok(aom) = std::env::var("AOM_PREFIX") {
                println!("cargo:rustc-link-search=native={aom}/lib");
            }
            println!("cargo:rustc-link-lib=static=aom");
        } else {
            println!("cargo:rustc-link-lib=avif");
        }
        return;
    }

    panic!("libavif not found — install libavif-dev, or set AVIF_PREFIX=/path/to/prefix");
}
