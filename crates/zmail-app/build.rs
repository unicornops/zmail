//! Embeds the Windows version resource the WiX MSI binds against.
//!
//! The `!(bind.fileVersion.zmail-app.exe)` in `wix/main.wxs` requires a
//! VERSIONINFO resource in the executable; without it the MSI link fails.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
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
