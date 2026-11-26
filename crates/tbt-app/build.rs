use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icons = manifest_dir.join("../../packaging/icons");
    let icon = "io.VagueMotion.TokenBurnTrack.ico";
    println!("cargo:rerun-if-changed={}", icons.join(icon).display());

    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numeric = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap())
        .join(",");

    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {numeric},0
PRODUCTVERSION {numeric},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "TokenBurnTrack"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "tbt-app"
      VALUE "OriginalFilename", "tbt-app.exe"
      VALUE "ProductName", "TokenBurnTrack"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("tbt-app.rc");
    fs::write(&rc_path, rc).unwrap();

    embed_resource::compile(&rc_path, embed_resource::ParamsIncludeDirs([icons]))
        .manifest_required()
        .unwrap();
}
