// Links against lib/pdfium.dll, copies it next to the built executable so it runs straight
// from target/, and embeds the app icon and manifest into the executable.
use std::path::PathBuf;

#[path = "build/logo.rs"]
mod logo;
#[path = "build/resources.rs"]
mod resources;

fn main() {
    println!("cargo:rerun-if-changed=lib/pdfium.dll");
    println!("cargo:rerun-if-changed=build");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rustc-link-search=native={}", manifest.join("lib").display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out
    let profile_dir = out.ancestors().nth(3).unwrap();
    std::fs::copy(manifest.join("lib/pdfium.dll"), profile_dir.join("pdfium.dll"))
        .expect("lib/pdfium.dll is missing");

    // The bundled GNU toolchain has no resource compiler, so write the resource object ourselves.
    let icons: Vec<(u32, Vec<u8>)> = logo::ICON_SIZES.iter().map(|&s| (s, logo::render(s))).collect();
    let object = resources::object(&icons, resources::MANIFEST.as_bytes());
    let path = out.join("resources.o");
    std::fs::write(&path, object).unwrap();
    println!("cargo:rustc-link-arg-bins={}", path.display());
}
