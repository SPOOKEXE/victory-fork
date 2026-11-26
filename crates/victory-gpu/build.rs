//! The layer's own shaders as `src/hlsl/build.sh` compiled them, for
//! `hlsl.rs` to include: copied to the build's folder, empty where that
//! script has not run (the system's compiler makes those shaders then).

use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hlsl");
    let out = std::env::var_os("OUT_DIR").expect("cargo sets it");
    println!("cargo:rerun-if-changed=src/hlsl");
    for file in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = file.path();
        let Some(stem) = name.file_stem().filter(|_| name.extension().is_some_and(|e| e == "hlsl")) else {
            continue;
        };
        for kind in ["dxil", "sum"] {
            let made = dir.join("dxil").join(stem).with_extension(kind);
            let bytes = std::fs::read(made).unwrap_or_default();
            std::fs::write(Path::new(&out).join(stem).with_extension(kind), bytes).expect("the build's folder");
        }
    }
}
