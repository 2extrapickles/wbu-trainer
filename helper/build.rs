fn main() {
    // Export the undecorated name the game imports (stdcall would otherwise be _DirectInput8Create@20).
    let def = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("exports.def");
    println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
    println!("cargo:rerun-if-changed=exports.def");
}
