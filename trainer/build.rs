// Embeds the helper (dinput8.dll) built by ../helper, so the trainer can install and update it.
// Build the helper first (package.ps1 does); without it the trainer embeds nothing.
fn main() {
    let src = std::path::Path::new("../helper/target/i686-pc-windows-msvc/release/dinput8.dll");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("dinput8.dll");
    let bytes = std::fs::read(src).unwrap_or_default();
    if bytes.is_empty() {
        println!("cargo:warning=helper dinput8.dll not built yet: the trainer will not embed it");
    }
    std::fs::write(&out, bytes).unwrap();
    println!("cargo:rerun-if-changed=../helper/target/i686-pc-windows-msvc/release/dinput8.dll");
}
