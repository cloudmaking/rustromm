//! Builds the one piece of C in the project.
//!
//! See `src/libretro/shim/log_shim.c` — it exists because stable Rust cannot
//! define a variadic function, and without one every core log message arrives
//! with its arguments missing.
//!
//! `cc` picks the platform toolchain automatically: gcc/clang on Linux and
//! macOS, MSVC on Windows, including both arm64 targets.
fn main() {
    println!("cargo:rerun-if-changed=src/libretro/shim/log_shim.c");
    cc::Build::new()
        .file("src/libretro/shim/log_shim.c")
        .compile("rustromm_log_shim");
}
