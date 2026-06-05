use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn zig_build(project_dir: &PathBuf, out_dir: &Path) {
    let status = Command::new("zig")
        .args(["build", "-Doptimize=ReleaseSafe"])
        .current_dir(project_dir)
        .status()
        .expect("Failed to run zig build");

    if !status.success() {
        panic!("zig build failed for {}", project_dir.display());
    }

    for entry in fs::read_dir(project_dir.join("zig-out").join("bin"))
        .unwrap()
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "dll" || e == "exe") {
            fs::copy(&path, out_dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        project_dir.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        project_dir.join("build.zig").display()
    );
}

fn main() {
    let out_path = PathBuf::from(&env::var("OUT_DIR").unwrap());
    let manifest_path = PathBuf::from(&env::var("CARGO_MANIFEST_DIR").unwrap());

    zig_build(&manifest_path.join("noita-trampoline"), &out_path);
    zig_build(&manifest_path.join("noita-path-hook"), &out_path);
}
