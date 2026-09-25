//! Compiles the vendored XPress9 library and ripbi's shim into one static
//! archive, so the finished binaries carry no runtime native dependency.

fn main() {
    let encoder = std::env::var_os("CARGO_FEATURE_ENCODER").is_some();
    let mut build = cc::Build::new();
    build
        .include("vendor/include")
        .file("csrc/ripbi_xpress9.c")
        .file("vendor/src/Xpress9DecHuffman.c")
        .file("vendor/src/Xpress9DecLz77.c")
        .file("vendor/src/Xpress9Misc.c")
        // Status codes only: no error-text formatting and no debug traps.
        .define("XPRESS9_MAX_TRACE_LEVEL", "0")
        .warnings(false);
    if encoder {
        build
            .file("vendor/src/Xpress9EncHuffman.c")
            .file("vendor/src/Xpress9EncLz77.c")
            .define("RIPBI_XPRESS9_ENCODER", None);
    }
    build.compile("ripbi_xpress9");
    println!("cargo::rerun-if-changed=csrc");
    println!("cargo::rerun-if-changed=vendor");
}
