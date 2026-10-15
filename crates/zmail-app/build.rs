//! Records the pinned Zed/GPUI revision and embeds the Windows version
//! resource the WiX MSI binds against.
//!
//! The Zed tag and commit come from the `gpui` entry in the workspace
//! `Cargo.lock`, so the diagnostics strip stays correct when the
//! `zed-upstream` workflow bumps the pin.
//!
//! The `!(bind.fileVersion.zmail-app.exe)` in `wix/main.wxs` requires a
//! VERSIONINFO resource in the executable; without it the MSI link fails.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    emit_zed_pin();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);

    let mut version_info = winresource::WindowsResource::new();
    version_info.set("FileDescription", "Zmail desktop email client");
    version_info.set("ProductName", "Zmail");
    version_info.set("ProductVersion", &version);
    version_info.set("FileVersion", &version);
    version_info.set_version_info(
        winresource::VersionInfo::FILEVERSION,
        (major << 48) | (minor << 32) | (patch << 16),
    );
    if let Err(error) = version_info.compile() {
        println!("cargo:warning=failed to embed the Windows version resource: {error}");
    }
}

/// Emits `ZMAIL_ZED_TAG` and `ZMAIL_ZED_COMMIT` from the locked `gpui`
/// source, e.g. `git+https://github.com/zed-industries/zed?tag=v1.24.2#<sha>`.
fn emit_zed_pin() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let lock_path = PathBuf::from(manifest_dir).join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());

    let (tag, commit) = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|lock| gpui_source(&lock))
        .unwrap_or_else(|| ("unknown".to_owned(), "unknown".to_owned()));
    println!("cargo:rustc-env=ZMAIL_ZED_TAG={tag}");
    println!("cargo:rustc-env=ZMAIL_ZED_COMMIT={commit}");
}

fn gpui_source(lock: &str) -> Option<(String, String)> {
    let package = lock
        .split("[[package]]")
        .find(|block| block.lines().any(|line| line.trim() == "name = \"gpui\""))?;
    let source = package
        .lines()
        .find_map(|line| line.trim().strip_prefix("source = \""))?
        .trim_end_matches('"');
    let (url, commit) = source.split_once('#')?;
    let tag = url
        .split_once("tag=")
        .map(|(_, tag)| tag.to_owned())
        .unwrap_or_else(|| "untagged".to_owned());
    Some((tag, commit.to_owned()))
}
