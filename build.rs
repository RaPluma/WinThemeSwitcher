// Embeds app.manifest into the executable.
//
// The manifest is what gives the settings GUI native (Common Controls v6)
// controls and makes the process per-monitor DPI aware. It is passed straight
// to link.exe, so no extra build dependency is needed.

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest =
        std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("app.manifest");
    let manifest = std::fs::canonicalize(&manifest).expect("app.manifest is missing");

    // rustc does not pass /MANIFEST:EMBED for the msvc target, and the linker
    // rejects /MANIFESTINPUT without it. /MANIFESTUAC:NO keeps link.exe from
    // generating its own (conflicting) UAC fragment — the manifest already
    // carries requestedExecutionLevel.
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTUAC:NO");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
}
