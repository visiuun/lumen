use std::path::PathBuf;

include!("src/icon.rs");

/// Builds a multi-size .ico (32-bit BMP entries) from the shared icon renderer.
fn write_ico(path: &PathBuf) {
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&n| {
            let rgba = icon_rgba(n);
            let mut dib = Vec::new();
            // BITMAPINFOHEADER: height is doubled to cover the (unused) AND mask.
            for v in [40u32, n, n * 2] {
                dib.extend_from_slice(&v.to_le_bytes());
            }
            dib.extend_from_slice(&1u16.to_le_bytes());
            dib.extend_from_slice(&32u16.to_le_bytes());
            dib.extend_from_slice(&[0u8; 24]);
            for y in (0..n).rev() {
                for x in 0..n {
                    let i = ((y * n + x) * 4) as usize;
                    dib.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
                }
            }
            let mask_row = (n.div_ceil(32) * 4) as usize;
            dib.extend(std::iter::repeat_n(0u8, mask_row * n as usize));
            dib
        })
        .collect();

    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&n, img) in sizes.iter().zip(&images) {
        let dim = if n >= 256 { 0 } else { n as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        ico.extend(img);
    }
    std::fs::write(path, ico).unwrap();
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("lumen.ico");
    write_ico(&ico);

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let numeric = parts.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Visiuun"
      VALUE "FileDescription", "lumen monitor calibration"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "lumen"
      VALUE "LegalCopyright", "(c) Visiuun. CC BY-NC 4.0"
      VALUE "OriginalFilename", "lumen.exe"
      VALUE "ProductName", "lumen"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = ico.display().to_string().replace('\\', "\\\\"),
    );
    let rc_path = out.join("lumen.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional().unwrap();
}
