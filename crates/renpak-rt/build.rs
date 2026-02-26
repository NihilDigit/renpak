fn main() {
    pkg_config::Config::new()
        .atleast_version("1.0")
        .probe("libavif")
        .expect("system libavif not found — install libavif via your package manager");
}
