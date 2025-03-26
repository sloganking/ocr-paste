use anyhow::{anyhow, Context, Result}; // Make sure anyhow itself is imported for anyhow! macro
use clap::Parser;
// Updated clipboard imports: Use format types directly
use clipboard_win::{formats, set_clipboard, set_clipboard_string};
// Removed get_clipboard since we use the specific format getter now
// Removed ImageOutputFormat, use ImageFormat for saving. Removed GenericImageView (unused).
use image::ImageFormat;
use rdev::{listen, simulate, Event, EventType, Key};
use std::collections::HashMap;
use std::fs;
// Removed std::io::Write (unused)
use std::path::Path; // Removed PathBuf (unused)
use std::process::Command;
use std::thread;
use std::time::Duration;

// --- Key Mapping (lazy_static) ---
// Keep the full map here...
lazy_static::lazy_static! {
    static ref KEY_MAP: HashMap<&'static str, Key> = {
        let mut m = HashMap::new();
        // --- SNIPPED for brevity - Keep the full map from the original code ---
        m.insert("alt", Key::Alt);
        m.insert("altgr", Key::AltGr);
        m.insert("backspace", Key::Backspace);
        m.insert("capslock", Key::CapsLock);
        m.insert("controlleft", Key::ControlLeft);
        m.insert("controlright", Key::ControlRight);
        m.insert("delete", Key::Delete);
        m.insert("downarrow", Key::DownArrow);
        m.insert("end", Key::End);
        m.insert("escape", Key::Escape);
        m.insert("f1", Key::F1);
        m.insert("f2", Key::F2);
        m.insert("f3", Key::F3);
        m.insert("f4", Key::F4);
        m.insert("f5", Key::F5);
        m.insert("f6", Key::F6);
        m.insert("f7", Key::F7);
        m.insert("f8", Key::F8);
        m.insert("f9", Key::F9);
        m.insert("f10", Key::F10);
        m.insert("f11", Key::F11);
        m.insert("f12", Key::F12);
        m.insert("home", Key::Home);
        m.insert("leftarrow", Key::LeftArrow);
        m.insert("metaleft", Key::MetaLeft); // Windows Key
        m.insert("metaright", Key::MetaRight); // Windows Key
        m.insert("pagedown", Key::PageDown);
        m.insert("pageup", Key::PageUp);
        m.insert("return", Key::Return); // Enter
        m.insert("rightarrow", Key::RightArrow);
        m.insert("shiftleft", Key::ShiftLeft);
        m.insert("shiftright", Key::ShiftRight);
        m.insert("space", Key::Space);
        m.insert("tab", Key::Tab);
        m.insert("uparrow", Key::UpArrow);
        m.insert("printscreen", Key::PrintScreen);
        m.insert("scrolllock", Key::ScrollLock);
        m.insert("pause", Key::Pause);
        m.insert("numlock", Key::NumLock);
        m.insert("backquote", Key::BackQuote);
        m.insert("num1", Key::Num1);
        m.insert("num2", Key::Num2);
        m.insert("num3", Key::Num3);
        m.insert("num4", Key::Num4);
        m.insert("num5", Key::Num5);
        m.insert("num6", Key::Num6);
        m.insert("num7", Key::Num7);
        m.insert("num8", Key::Num8);
        m.insert("num9", Key::Num9);
        m.insert("num0", Key::Num0);
        m.insert("minus", Key::Minus);
        m.insert("equal", Key::Equal);
        m.insert("keyq", Key::KeyQ);
        m.insert("keyw", Key::KeyW);
        m.insert("keye", Key::KeyE);
        m.insert("keyr", Key::KeyR);
        m.insert("keyt", Key::KeyT);
        m.insert("keyy", Key::KeyY);
        m.insert("keyu", Key::KeyU);
        m.insert("keyi", Key::KeyI);
        m.insert("keyo", Key::KeyO);
        m.insert("keyp", Key::KeyP);
        m.insert("leftbracket", Key::LeftBracket);
        m.insert("rightbracket", Key::RightBracket);
        m.insert("keya", Key::KeyA);
        m.insert("keys", Key::KeyS);
        m.insert("keyd", Key::KeyD);
        m.insert("keyf", Key::KeyF);
        m.insert("keyg", Key::KeyG);
        m.insert("keyh", Key::KeyH);
        m.insert("keyj", Key::KeyJ);
        m.insert("keyk", Key::KeyK);
        m.insert("keyl", Key::KeyL);
        m.insert("semicolon", Key::SemiColon);
        m.insert("quote", Key::Quote);
        m.insert("backslash", Key::BackSlash);
        m.insert("intlbackslash", Key::IntlBackslash);
        m.insert("keyz", Key::KeyZ);
        m.insert("keyx", Key::KeyX);
        m.insert("keyc", Key::KeyC);
        m.insert("keyv", Key::KeyV);
        m.insert("keyb", Key::KeyB);
        m.insert("keyn", Key::KeyN);
        m.insert("keym", Key::KeyM);
        m.insert("comma", Key::Comma);
        m.insert("dot", Key::Dot);
        m.insert("slash", Key::Slash);
        // ... add more keys as needed ...
        m
    };
}

fn string_to_rdev_key(key_name: &str) -> Option<Key> {
    KEY_MAP.get(key_name.to_lowercase().as_str()).cloned()
}

// --- CLI Arguments (Keep as is) ---
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about,
    long_about = "Listens for a key press, performs OCR on clipboard image via Tesseract CLI, pastes text, restores image."
)]
struct Args {
    #[arg(
        short,
        long,
        help = "Key name to trigger OCR (e.g., F1, ScrollLock, Home, KeyX). See code for more options."
    )]
    trigger_key: String,

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

// --- Core OCR and Paste Logic ---
fn perform_ocr_and_paste(
    lang: &str,
    tesseract_cmd: &str,
    tessdata_path: Option<&str>,
    extra_args: &[String],
) -> Result<()> {
    println!("Trigger key pressed. Processing clipboard image...");

    // 1. Backup current clipboard content (Use formats::Bitmap type)
    //    Map the ErrorCode to anyhow::Error before adding context.
    let clipboard_dib_content = clipboard_win::get_clipboard(formats::Bitmap)
        .map_err(|e| anyhow!("Failed to get bitmap from clipboard: {}", e)) // Convert ErrorCode to anyhow::Error
        .context("Is an image (Bitmap format) copied to the clipboard?")?; // Now context() works

    println!(
        "Got bitmap data ({} bytes) from clipboard.",
        clipboard_dib_content.len()
    );

    // Create a temporary file path
    let temp_dir = std::env::temp_dir();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let temp_image_filename = format!("clipboard_ocr_{}_{}.png", std::process::id(), timestamp);
    let temp_image_path = temp_dir.join(&temp_image_filename);

    // RAII Guard for temporary file cleanup
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

    // 2. Decode DIB and save as PNG (Use ImageFormat::Png)
    {
        // Assuming DIB data is loadable as BMP
        let img = image::load_from_memory_with_format(&clipboard_dib_content, ImageFormat::Bmp)
            .context("Failed to decode clipboard DIB data as BMP. Clipboard content might not be a standard BMP format.")?;

        println!(
            "Decoded image. Saving temporary file to {:?}",
            temp_image_path
        );

        // Use ImageFormat::Png instead of ImageOutputFormat::Png
        img.save_with_format(&temp_image_path, ImageFormat::Png)
            .with_context(|| {
                format!(
                    "Failed to save temporary PNG image to {:?}",
                    temp_image_path
                )
            })?;
        println!("Temporary image saved.");
    }

    // 3. Perform OCR using Tesseract CLI (Keep as is)
    println!("Running Tesseract CLI...");
    let mut command = Command::new(tesseract_cmd);
    command.arg(&temp_image_path); // Input file
    command.arg("stdout"); // Output to stdout
    command.arg("-l").arg(lang); // Language

    if let Some(tessdata) = tessdata_path {
        command.arg("--tessdata-dir").arg(tessdata);
    }

    for arg in extra_args {
        command.arg(arg);
    }

    let output = command.output().with_context(|| format!("Failed to execute Tesseract command: '{}'. Is it installed and in PATH, or is --tesseract-cmd correct?", tesseract_cmd))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            // Use anyhow! macro
            "Tesseract CLI failed with status: {}.\nStderr:\n{}",
            output.status,
            stderr
        ));
    }

    let ocr_text =
        String::from_utf8(output.stdout).context("Failed to read Tesseract CLI output as UTF-8")?;

    // 4. Cleanup (Handled by TempFileGuard) is automatic

    let trimmed_text = ocr_text.trim();
    if trimmed_text.is_empty() {
        println!("OCR resulted in empty text. Skipping paste.");
        return Ok(());
    }

    println!("OCR Result (first 100 chars): {:.100}...", trimmed_text);

    // 5. Paste Text
    // Map error before context
    set_clipboard_string(trimmed_text)
        .map_err(|e| anyhow!("Failed to set clipboard string: {}", e)) // Convert ErrorCode
        .context("Failed to place OCR text onto clipboard")?; // Now context() works

    println!("OCR text placed on clipboard. Simulating paste (Ctrl+V)...");
    thread::sleep(Duration::from_millis(150));

    send_ctrl_v().context("Failed to simulate Ctrl+V paste")?;

    // 6. Restore Original Image
    thread::sleep(Duration::from_millis(150));
    // Use formats::Bitmap and map error before context
    set_clipboard(formats::Bitmap, &clipboard_dib_content)
        .map_err(|e| anyhow!("Failed to set clipboard bitmap: {}", e)) // Convert ErrorCode
        .context("Failed to restore original bitmap to clipboard")?; // Now context() works
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

// --- Main Function (Keep as is) ---
fn main() -> Result<()> {
    let args = Args::parse();

    let target_key = string_to_rdev_key(&args.trigger_key).ok_or_else(|| {
        anyhow!(
            "Invalid or unsupported trigger key name: '{}'. Check key map in code.",
            args.trigger_key
        )
    })?; // Use anyhow!

    let lang = args.lang.clone();
    let tesseract_cmd = args.tesseract_cmd.clone();
    let tessdata_path_opt = args.tessdata_path.clone();
    let extra_args = args.tesseract_args.clone();

    println!("Clipboard OCR Listener Started (Using Tesseract CLI).");
    println!("Trigger Key: '{}' ({:?})", args.trigger_key, target_key);
    println!("OCR Language: '{}'", lang);
    println!("Tesseract Command: '{}'", tesseract_cmd);
    if let Some(p) = &args.tessdata_path {
        println!("Tessdata Path: '{}'", p);
    }
    if !extra_args.is_empty() {
        println!("Extra Tesseract Args: {:?}", extra_args);
    }
    println!("---");
    println!(
        "Press '{}' when an image is in the clipboard to perform OCR and paste.",
        args.trigger_key
    );
    println!(
        "Ensure Tesseract CLI ('{}') is installed and accessible (in PATH or via --tesseract-cmd).",
        tesseract_cmd
    );
    println!(
        "Ensure '{}' language data is available (use --tessdata-path if needed).",
        lang
    );
    println!("NOTE: This program likely requires administrator privileges to capture global key presses.");
    println!("Ctrl+C in this window to exit.");
    println!("---");

    let callback = move |event: Event| {
        match event.event_type {
            EventType::KeyPress(key) if key == target_key => {
                if let Err(e) = perform_ocr_and_paste(
                    &lang,
                    &tesseract_cmd,
                    tessdata_path_opt.as_deref(),
                    &extra_args,
                ) {
                    eprintln!("ERROR: {:?}", e); // anyhow errors print nicely with :?
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
        eprintln!("This might be a permissions issue. Try running the program as administrator.");
        return Err(anyhow!("Keyboard listener error: {:?}", error)); // Use anyhow!
    }

    Ok(())
}
