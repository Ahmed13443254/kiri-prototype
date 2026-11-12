fn main() {
    tauri_build::build();

    // tauri-build links the manifest resource into bins only; the lib test
    // harness needs it too, or comctl32 resolves to v5.82 without
    // TaskDialogIndirect and the test binary fails to load.
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") {
        let out = std::env::var("OUT_DIR").expect("OUT_DIR");
        let res = if target.contains("msvc") {
            "resource.lib"
        } else {
            "libresource.a"
        };
        println!("cargo:rustc-link-arg={out}/{res}");
    }
}