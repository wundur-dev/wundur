//! On macOS the window's binary carries an Info.plist of its own (in a `__TEXT,__info_plist`
//! section), so that run without an app bundle (in development) it can ask for the microphone
//! and speech recognition: macOS stops a program that asks without saying why. The bundle's
//! Info.plist (scripts/bundle-macos.sh) says the same.

fn main() {
    // The folder of the checkout being built, read when this script runs, not when it was
    // compiled (`env!`): a compiled build script is reused across checkouts sharing a build
    // cache (worktrees), and `env!` kept handing every one of them the path of the checkout it
    // was first compiled in. Once that worktree was removed, linking failed on its missing plist.
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for build scripts");
    let plist = std::path::Path::new(&dir).join("assets/macos/Info.plist");
    println!("cargo:rerun-if-changed={}", plist.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bin=wundur=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    }
}
