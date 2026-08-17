//! Windows executable resources.
//!
//! Explorer, the taskbar, and the Programs list all read the icon and version
//! block out of the PE image itself — there is no bundle or desktop entry to
//! carry them. Every other platform builds without a script.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    export_sparkle_public_key();

    #[cfg(target_os = "windows")]
    {
        // GPUI's Taffy layout and text shaping recurse deeply enough to
        // overflow the 1 MiB the MSVC linker defaults to.
        println!("cargo:rustc-link-arg-bins=/stack:{}", 8 * 1024 * 1024);
        embed_windows_resources();
    }
}

/// Republish `SUPublicEDKey` from the macOS Info.plist as a compile-time
/// constant.
///
/// The Windows updater verifies the same EdDSA signatures `generate_appcast`
/// writes, against the same key. Reading the plist here rather than repeating
/// the key in Rust means the two cannot drift into a feed the app rejects.
fn export_sparkle_public_key() {
    const PLIST: &str = "resources/Info.plist";
    const KEY: &str = "<key>SUPublicEDKey</key>";

    println!("cargo:rerun-if-changed={PLIST}");

    let plist = std::fs::read_to_string(PLIST).expect("read the app Info.plist");
    let value = plist
        .split_once(KEY)
        .and_then(|(_, rest)| rest.split_once("<string>"))
        .and_then(|(_, rest)| rest.split_once("</string>"))
        .map(|(value, _)| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{PLIST} has no SUPublicEDKey"));

    println!("cargo:rustc-env=WAKU_SPARKLE_PUBLIC_ED_KEY={value}");
}

#[cfg(target_os = "windows")]
fn embed_windows_resources() {
    const ICON: &str = "resources/windows/AppIcon.ico";

    println!("cargo:rerun-if-changed={ICON}");

    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ICON);
    // The resource compiler reads `.rc` as C source, so a Windows path
    // separator has to survive as a literal backslash.
    let icon = icon.to_string_lossy().replace('\\', "\\\\");

    let package_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // VERSIONINFO wants four numeric fields; Waku's version has three.
    let mut fields = package_version
        .split(['.', '-', '+'])
        .map(|field| field.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    let file_version = format!(
        "{},{},{},{}",
        fields.next().unwrap_or(0),
        fields.next().unwrap_or(0),
        fields.next().unwrap_or(0),
        fields.next().unwrap_or(0),
    );
    let description = std::env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default();

    let resources = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {file_version}
PRODUCTVERSION {file_version}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Waku\0"
            VALUE "FileDescription", "{description}\0"
            VALUE "FileVersion", "{package_version}\0"
            VALUE "InternalName", "waku\0"
            VALUE "OriginalFilename", "waku.exe\0"
            VALUE "ProductName", "Waku\0"
            VALUE "ProductVersion", "{package_version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let script = out_dir.join("waku.rc");
    std::fs::write(&script, resources).expect("write the resource script");

    // GPUI embeds the application manifest through its own resource script,
    // so this one only claims the icon and version block.
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_optional()
        .expect("compile Windows resources");
}
