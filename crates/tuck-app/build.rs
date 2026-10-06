use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let res = manifest_dir.join("res");
    for file in ["tuck.rc", "tuck.manifest", "tuck.ico"] {
        println!("cargo:rerun-if-changed={}", res.join(file).display());
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    let mut macros = vec![
        format!("TUCK_VERSION_MAJOR={}", part("CARGO_PKG_VERSION_MAJOR")),
        format!("TUCK_VERSION_MINOR={}", part("CARGO_PKG_VERSION_MINOR")),
        format!("TUCK_VERSION_PATCH={}", part("CARGO_PKG_VERSION_PATCH")),
        format!("TUCK_VERSION_STRING=\"{version}\""),
    ];
    if res.join("tuck.ico").exists() {
        macros.push("TUCK_HAS_ICON".into());
    } else {
        println!("cargo:warning=res/tuck.ico is missing; run `cargo run -p tuck-app --example make_icon`");
    }
    let include_dirs = [res.clone()];
    embed_resource::compile(res.join("tuck.rc"), embed_resource::ParamsMacrosAndIncludeDirs(&macros, &include_dirs))
        .manifest_required()
        .expect("compiling the Windows resources (manifest, icon, version info)");
}
