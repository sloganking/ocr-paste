use anyhow::{anyhow, Context, Result};
use clap::Parser;
use clipboard_win::{formats, set_clipboard, set_clipboard_string};
use image::ImageFormat;
use rdev::{listen, simulate, Event, EventType, Key}; // Keep rdev::Key import
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

// --- Import the new module and enum ---
mod easy_rdev_key;
use easy_rdev_key::PTTKey;

// --- REMOVE lazy_static and KEY_MAP ---
// lazy_static::lazy_static! { ... } // DELETE THIS BLOCK
// fn string_to_rdev_key(...) { ... } // DELETE THIS FUNCTION

// --- CLI Arguments ---
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about,
    long_about = "Listens for a key press, performs OCR on clipboard image via Tesseract CLI, pastes text, restores image."
)]
struct Args {
    // --- Use PTTKey for trigger_key ---
    #[arg(short, long, value_enum, help = "Key to trigger OCR.")]
    trigger_key: PTTKey, // Changed from String to PTTKey

    #[arg(
        short,
        long,
        default_value = "eng",
        help = "Tesseract language code(s) (e.g., 'eng', 'eng+fra'). Passed via '-l'."
    )]
    lang: String,

    #[arg(
        long,
        default_value = "tesseract",
        help = "Path to the Tesseract executable or command name (if in PATH)."
    )]
    tesseract_cmd: String,

    #[arg(
        long,
        help = "Path to the Tesseract data directory (TESSDATA_PREFIX). Passed via '--tessdata-dir'."
    )]
    tessdata_path: Option<String>,

    #[arg(long, help = "Additional arguments to pass directly to the Tesseract CLI.", num_args = 0..)]
    tesseract_args: Vec<String>,
}

// --- Core OCR and Paste Logic (No changes needed inside this function) ---
fn perform_ocr_and_paste(
    lang: &str,
    tesseract_cmd: &str,
    tessdata_path: Option<&str>,
    extra_args: &[String],
) -> Result<()> {
    println!("Trigger key pressed. Processing clipboard image...");

    // 1. Backup clipboard
    let clipboard_dib_content = clipboard_win::get_clipboard(formats::Bitmap)
        .map_err(|e| anyhow!("Failed to get bitmap from clipboard: {}", e))
        .context("Is an image (Bitmap format) copied to the clipboard?")?;
    println!(
        "Got bitmap data ({} bytes) from clipboard.",
        clipboard_dib_content.len()
    );

    // 2. Prepare temp file
    let temp_dir = std::env::temp_dir();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let temp_image_filename = format!("clipboard_ocr_{}_{}.png", std::process::id(), timestamp);
    let temp_image_path = temp_dir.join(&temp_image_filename);

    struct TempFileGuard<'a>(&'a Path);
    impl Drop for TempFileGuard<'_> {
        fn drop(&mut self) {
            if self.0.exists() {
                if let Err(e) = fs::remove_file(self.0) {
                    eprintln!(
                        "Warning: Failed to delete temporary file {:?}: {}",
                        self.0, e
                    );
                } else {
                    println!("Temporary file {:?} deleted.", self.0);
                }
            }
        }
    }
    let _temp_file_guard = TempFileGuard(&temp_image_path);

    // 3. Save image to temp file
    {
        let img = image::load_from_memory_with_format(&clipboard_dib_content, ImageFormat::Bmp)
            .context("Failed to decode clipboard DIB data as BMP.")?;
        println!(
            "Decoded image. Saving temporary file to {:?}",
            temp_image_path
        );
        img.save_with_format(&temp_image_path, ImageFormat::Png)
            .with_context(|| {
                format!(
                    "Failed to save temporary PNG image to {:?}",
                    temp_image_path
                )
            })?;
        println!("Temporary image saved.");
    }

    // 4. Run Tesseract CLI
    println!("Running Tesseract CLI...");
    let mut command = Command::new(tesseract_cmd);
    // ... (rest of command setup is the same) ...
    command.arg(&temp_image_path);
    command.arg("stdout");
    command.arg("-l").arg(lang);
    if let Some(tessdata) = tessdata_path {
        command.arg("--tessdata-dir").arg(tessdata);
    }
    for arg in extra_args {
        command.arg(arg);
    }

    let output = command
        .output()
        .with_context(|| format!("Failed Tesseract command: '{}'", tesseract_cmd))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "Tesseract CLI failed ({}):\n{}",
            output.status,
            stderr
        ));
    }
    let ocr_text = String::from_utf8(output.stdout)?;

    // 5. Process OCR text
    let trimmed_text = ocr_text.trim();
    if trimmed_text.is_empty() {
        println!("OCR resulted in empty text. Skipping paste.");
        return Ok(());
    }
    println!("OCR Result (first 100 chars): {:.100}...", trimmed_text);

    // 6. Paste Text
    set_clipboard_string(trimmed_text)
        .map_err(|e| anyhow!("Failed clipboard set string: {}", e))
        .context("Failed to place OCR text onto clipboard")?;
    println!("OCR text placed on clipboard. Simulating paste (Ctrl+V)...");
    thread::sleep(Duration::from_millis(150));
    send_ctrl_v().context("Failed to simulate Ctrl+V paste")?;

    // 7. Restore Original Image
    thread::sleep(Duration::from_millis(150));
    set_clipboard(formats::Bitmap, &clipboard_dib_content)
        .map_err(|e| anyhow!("Failed clipboard set bitmap: {}", e))
        .context("Failed to restore original bitmap to clipboard")?;
    println!("Original image restored to clipboard.");

    Ok(())
}

// Helper function to simulate Ctrl+V (Keep as is)
fn send_ctrl_v() -> Result<(), rdev::SimulateError> {
    let delay = Duration::from_millis(30);
    simulate(&EventType::KeyPress(Key::ControlLeft))?;
    thread::sleep(delay);
    simulate(&EventType::KeyPress(Key::KeyV))?;
    thread::sleep(delay);
    simulate(&EventType::KeyRelease(Key::KeyV))?;
    thread::sleep(delay);
    simulate(&EventType::KeyRelease(Key::ControlLeft))?;
    println!("Paste simulated.");
    Ok(())
}

// --- Main Function ---
fn main() -> Result<()> {
    let args = Args::parse();

    // --- Convert PTTKey to rdev::Key using the From trait ---
    // No need for error handling here, clap already validated the input.
    let target_key: rdev::Key = args.trigger_key.into();

    // Clone data needed for the closure
    let lang = args.lang.clone();
    let tesseract_cmd = args.tesseract_cmd.clone();
    let tessdata_path_opt = args.tessdata_path.clone();
    let extra_args = args.tesseract_args.clone();

    println!("Clipboard OCR Listener Started (Using Tesseract CLI).");
    // --- Use {:?} for the PTTKey enum ---
    println!(
        "Trigger Key: {:?} (Converted to {:?})",
        args.trigger_key, target_key
    );
    println!("OCR Language: '{}'", lang);
    println!("Tesseract Command: '{}'", tesseract_cmd);
    if let Some(p) = &args.tessdata_path {
        println!("Tessdata Path: '{}'", p);
    }
    if !extra_args.is_empty() {
        println!("Extra Tesseract Args: {:?}", extra_args);
    }
    println!("---");
    // --- Update help message ---
    println!(
        "Press '{:?}' when an image is in the clipboard to perform OCR and paste.",
        args.trigger_key
    );
    println!(
        "Ensure Tesseract CLI ('{}') is installed and accessible.",
        tesseract_cmd
    );
    println!(
        "Ensure '{}' language data is available (use --tessdata-path if needed).",
        lang
    );
    println!("NOTE: This program likely requires administrator privileges.");
    println!("Ctrl+C in this window to exit.");
    println!("---");

    let callback = move |event: Event| {
        match event.event_type {
            // --- Comparison still works, as target_key is now rdev::Key ---
            EventType::KeyPress(key) if key == target_key => {
                if let Err(e) = perform_ocr_and_paste(
                    &lang,
                    &tesseract_cmd,
                    tessdata_path_opt.as_deref(),
                    &extra_args,
                ) {
                    eprintln!("ERROR: {:?}", e);
                }
            }
            _ => (),
        }
    };

    if let Err(error) = listen(callback) {
        eprintln!(
            "FATAL ERROR setting up global keyboard listener: {:?}",
            error
        );
        eprintln!("This might be a permissions issue. Try running as administrator.");
        return Err(anyhow!("Keyboard listener error: {:?}", error));
    }

    Ok(())
}
