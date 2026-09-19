fn main() {
    println!("cargo:rerun-if-changed=vendor/httpyac/src");
    if std::env::var_os("CARGO_FEATURE_LOAD_GRAMMARS").is_some() {
        cc::Build::new()
            .include("vendor/httpyac/src")
            .file("vendor/httpyac/src/parser.c")
            .warnings(false)
            .compile("tree-sitter-httpyac");
    }
}
