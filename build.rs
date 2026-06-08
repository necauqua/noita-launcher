use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(&env::var("OUT_DIR").unwrap());
    let manifest_path = PathBuf::from(&env::var("CARGO_MANIFEST_DIR").unwrap());

    let zig_dir = manifest_path.join("noita-patcher");

    let status = Command::new("zig")
        .args([
            "build",
            match env::var("PROFILE").unwrap().as_ref() {
                "debug" => "-Doptimize=Debug",
                _ => "-Doptimize=ReleaseSafe",
            },
        ])
        .current_dir(&zig_dir)
        .status()
        .expect("Failed to run zig build");

    if !status.success() {
        panic!("zig build failed for {}", zig_dir.display());
    }

    for entry in fs::read_dir(zig_dir.join("zig-out").join("bin"))
        .unwrap()
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "dll" || e == "exe") {
            fs::copy(&path, out_dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    println!("cargo:rerun-if-changed={}", zig_dir.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        zig_dir.join("build.zig").display()
    );
}
