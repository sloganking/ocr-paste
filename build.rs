use std::env;
use std::fs::{self, File};
use std::path::PathBuf;

fn main() {
    // Only do resource embedding on Windows
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // Generate a very simple 16x16 ICO at build time
    // (blue square with white inner square) and embed it as the app icon
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let ico_path = out_dir.join("ocr_paste_icon.ico");

    generate_simple_ico(&ico_path);

    // Create an .rc file that references the generated icon
    let rc_path = out_dir.join("ocr_paste_icon.rc");
    // Use forward slashes so RC.EXE parses the path correctly
    let mut ico_path_str = ico_path.to_string_lossy().to_string();
    ico_path_str = ico_path_str.replace('\\', "/");
    let rc_contents = format!("IDI_ICON1 ICON \"{}\"\n", ico_path_str);
    fs::write(&rc_path, rc_contents).expect("failed to write rc file");

    // Compile the resource (no macros)
    use std::ffi::OsStr;
    let empty_macros: [&OsStr; 0] = [];
    embed_resource::compile(&rc_path, &empty_macros);
}

fn generate_simple_ico(path: &PathBuf) {
    use ico::{IconDir, IconImage};

    // Create a simple 16x16 RGBA image
    let width = 16u32;
    let height = 16u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            // Blue background
            rgba[idx + 0] = 0; // R
            rgba[idx + 1] = 122; // G
            rgba[idx + 2] = 204; // B
            rgba[idx + 3] = 255; // A

            // Draw a white 10x10 inner square centered
            if x >= 3 && x <= 12 && y >= 3 && y <= 12 {
                rgba[idx + 0] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = 255;
            }
        }
    }

    let image = IconImage::from_rgba_data(width as u32, height as u32, rgba);
    let entry = ico::IconDirEntry::encode(&image).expect("failed to encode icon entry");
    let mut icon_dir = IconDir::new(ico::ResourceType::Icon);
    icon_dir.add_entry(entry);

    let mut file = File::create(path).expect("failed to create ico file");
    icon_dir.write(&mut file).expect("failed to write ico file");
}
