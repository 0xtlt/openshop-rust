fn main() {
    // The admin UI bundle is embedded at compile time: rebuild when it changes.
    println!("cargo:rerun-if-changed=ui-dist");
    println!("cargo:rerun-if-changed=migrations");
}
