// src/main.rs

use anyhow::{anyhow, Context as AnyhowContext, Result};
use clap::Parser;
use clipboard_win::{formats, get_clipboard, Setter};
use dotenvy; // <-- Import dotenvy

use image::ImageFormat;
use rdev::{listen, simulate, Event, EventType, Key};
use std::{
    env, // Keep env for manual var reading as fallback/confirmation
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

mod easy_rdev_key;
use easy_rdev_key::PTTKey;
mod transcribe;

use async_openai::{config::OpenAIConfig, Client};
use tokio::runtime::Runtime;

const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "ogg", "flac", "aac", "wma", "opus", "aiff", "aif",
];

const CLIPBRD_E_UNSUPPORTEDFORMAT: i32 = -2147221040; // 0x800401D0

// --- CLI Arguments ---
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about,
    long_about = "Listens for a key press, processes clipboard content (image OCR or audio transcription), pastes text, and restores original clipboard."
)]
struct Args {
    #[arg(short, long, value_enum, help = "Key to trigger processing.")]
    trigger_key: PTTKey,

    #[arg(
        short = 'l',
        long,
        default_value = "eng",
        help = "Tesseract language code(s) (e.g., 'eng', 'eng+fra') for OCR. Passed via '-l'."
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

    #[arg(
        long,
        help = "Additional arguments to pass directly to the Tesseract CLI.",
        num_args = 0..
    )]
    tesseract_args: Vec<String>,

    // --- OpenAI API Key (CLI arg is optional, .env is checked first) ---
    #[arg(
        long,
        help = "OpenAI API Key (overrides .env or OPENAI_API_KEY env var)."
    )]
    openai_api_key: Option<String>,
}

#[derive(Debug)]
enum ClipboardContent {
    Bitmap(Vec<u8>),
    FileList(Vec<String>),
}

fn get_clipboard_content() -> Result<ClipboardContent> {
    match get_clipboard::<Vec<String>, _>(formats::FileList) {
        Ok(files) => {
            println!("Clipboard contains FileList: {:?}", files);
            return Ok(ClipboardContent::FileList(files));
        }
        Err(e) => {
            if e.raw_code() != CLIPBRD_E_UNSUPPORTEDFORMAT {
                println!(
                    "Warning: Failed to get FileList for unexpected reason (Error {}): {}. Trying Bitmap.",
                    e.raw_code(), e
                );
            } else {
                println!("Clipboard does not contain FileList format. Trying Bitmap.");
            }
        }
    }

    match get_clipboard::<Vec<u8>, _>(formats::Bitmap) {
        Ok(bitmap_data) => {
            println!(
                "Clipboard contains Bitmap data ({} bytes).",
                bitmap_data.len()
            );
            return Ok(ClipboardContent::Bitmap(bitmap_data));
        }
        Err(e) => {
            if e.raw_code() != CLIPBRD_E_UNSUPPORTEDFORMAT {
                println!(
                    "Warning: Failed to get Bitmap for unexpected reason (Error {}): {}",
                    e.raw_code(),
                    e
                );
            } else {
                println!("Clipboard does not contain Bitmap format either.");
            }
            return Err(anyhow!("Failed to get Bitmap from clipboard: {}", e));
        }
    }
}

fn restore_clipboard(content: ClipboardContent) -> Result<()> {
    match content {
        ClipboardContent::Bitmap(data) => {
            println!("Restoring Bitmap to clipboard...");
            formats::Bitmap
                .write_clipboard(&data)
                .map_err(|e| anyhow!("Failed to restore Bitmap to clipboard: {}", e))
        }
        ClipboardContent::FileList(files) => {
            println!("Restoring FileList to clipboard...");
            formats::FileList
                .write_clipboard(&files)
                .map_err(|e| anyhow!("Failed to restore FileList to clipboard: {}", e))
        }
    }
}

fn process_clipboard_and_paste(
    original_content: ClipboardContent, // Takes ownership
    args: &Args,                        // Now contains key from arg OR env
    rt: &Runtime,
) -> Result<()> {
    let processed_text_result = match &original_content {
        ClipboardContent::FileList(files) => {
            if files.len() == 1 {
                let file_path = PathBuf::from(&files[0]);
                let extension = file_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.to_lowercase())
                    .unwrap_or_default();

                if AUDIO_EXTENSIONS.contains(&extension.as_str()) {
                    println!("Detected single audio file: {:?}", file_path);
                    // API key should be present in args.openai_api_key if found via arg or .env/env var
                    let api_key = args.openai_api_key.as_ref().ok_or_else(|| {
                        // This error should ideally not happen if logic in main is correct,
                        // but keep it as a safeguard.
                        anyhow!("OpenAI API Key is missing (checked arg, .env, env var).")
                    })?;
                    println!("INFO: Audio transcription requires ffmpeg to be installed and in your PATH.");

                    let config = OpenAIConfig::new().with_api_key(api_key);
                    let client = Client::with_config(config);

                    rt.block_on(transcribe::trans::transcribe(&client, &file_path))
                        .with_context(|| {
                            format!("Audio transcription failed for file: {:?}", file_path)
                        })
                } else {
                    Err(anyhow!(
                         "Clipboard contains a single file, but it's not a supported audio format (Checked: {:?}, Ext: {}).",
                         AUDIO_EXTENSIONS, extension
                     ))
                }
            } else {
                Err(anyhow!(
                     "Clipboard contains {} files. Only single audio file transcription is supported.",
                     files.len()
                 ))
            }
        }
        ClipboardContent::Bitmap(bitmap_data) => {
            println!("Processing clipboard image with Tesseract OCR...");
            let temp_dir = std::env::temp_dir();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis();
            let temp_image_filename =
                format!("clipboard_ocr_{}_{}.png", std::process::id(), timestamp);
            let temp_image_path = temp_dir.join(&temp_image_filename);

            struct TempFileGuard<'a>(&'a Path);
            impl Drop for TempFileGuard<'_> {
                fn drop(&mut self) {
                    if self.0.exists() {
                        if let Err(e) = fs::remove_file(self.0) {
                            eprintln!(
                                "Warning: Failed to delete temporary image file {:?}: {}",
                                self.0, e
                            );
                        } else {
                            println!("Temporary image file {:?} deleted.", self.0);
                        }
                    }
                }
            }
            let _temp_file_guard = TempFileGuard(&temp_image_path);

            let img = image::load_from_memory(bitmap_data)
                .with_context(|| "Failed to decode clipboard image data")?;
            println!(
                "Decoded image. Saving temporary PNG to {:?}",
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

            println!("Running Tesseract CLI...");
            let mut command = Command::new(&args.tesseract_cmd);
            command.arg(&temp_image_path);
            command.arg("stdout");
            command.arg("-l").arg(&args.lang);
            if let Some(tessdata) = &args.tessdata_path {
                command.arg("--tessdata-dir").arg(tessdata);
            }
            for arg in &args.tesseract_args {
                command.arg(arg);
            }

            let output = command.output().with_context(|| {
                format!(
                    "Failed to execute Tesseract command: '{}'",
                    args.tesseract_cmd
                )
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(anyhow!(
                    "Tesseract CLI failed (Status: {}):\n{}",
                    output.status,
                    stderr
                ))
            } else {
                String::from_utf8(output.stdout)
                    .with_context(|| "Tesseract output was not valid UTF-8")
            }
        }
    };

    match processed_text_result {
        Ok(processed_text) => {
            let trimmed_text = processed_text.trim();
            if trimmed_text.is_empty() {
                println!("Processing resulted in empty text. Skipping paste.");
                restore_clipboard(original_content).with_context(|| {
                    "Failed to restore original clipboard content after empty result"
                })?;
                Ok(())
            } else {
                println!("Processed Text (first 100 chars): {:.100}...", trimmed_text);

                set_clipboard_string_helper(trimmed_text)
                    .with_context(|| "Failed to place processed text onto clipboard")?;
                println!("Processed text placed on clipboard. Simulating paste (Ctrl+V)...");
                thread::sleep(Duration::from_millis(150));
                send_ctrl_v().context("Failed to simulate Ctrl+V paste")?;

                thread::sleep(Duration::from_millis(150));
                restore_clipboard(original_content)
                    .with_context(|| "Failed to restore original content to clipboard")?;
                println!("Original clipboard content restored.");
                Ok(())
            }
        }
        Err(e) => {
            eprintln!("ERROR processing clipboard content: {:?}", e);
            if let Err(restore_err) = restore_clipboard(original_content) {
                eprintln!(
                    "Additionally failed to restore clipboard: {:?}",
                    restore_err
                );
            }
            Err(e)
        }
    }
}

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
    // --- Load .env file BEFORE parsing args ---
    // Ignore errors (e.g., if .env file is missing)
    match dotenvy::dotenv() {
        Ok(path) => println!("Loaded environment variables from: {:?}", path),
        Err(e) => {
            // Don't fail if .env is missing, but maybe warn if it failed unexpectedly
            if !e.not_found() {
                eprintln!("Warning: Failed to load .env file: {}", e);
            } else {
                println!("No .env file found, proceeding without it.");
            }
        }
    };

    // --- Parse Arguments ---
    // Make args mutable so we can potentially update openai_api_key
    let mut args = Args::parse();

    // --- Check/Load API Key (Priority: Argument > Environment Variable) ---
    if args.openai_api_key.is_none() {
        println!("--openai-api-key argument not provided, checking OPENAI_API_KEY environment variable (loaded from system or .env)...");
        // This will read the variable set by dotenvy OR the system environment
        match env::var("OPENAI_API_KEY") {
            Ok(key_from_env) => {
                if !key_from_env.is_empty() {
                    println!("Using OpenAI API Key found in environment variable.");
                    args.openai_api_key = Some(key_from_env);
                } else {
                    println!("OPENAI_API_KEY environment variable is set but empty.");
                }
            }
            Err(_) => {
                println!("OPENAI_API_KEY environment variable not found or not set.");
            }
        }
    } else {
        println!("Using OpenAI API Key provided via command-line argument.");
    }
    // --- End API Key Check ---

    let rt = Runtime::new().context("Failed to create Tokio runtime")?;
    let target_key: rdev::Key = args.trigger_key.into();

    // --- Startup Information ---
    println!("Clipboard Processor Started.");
    println!(
        "Trigger Key: {:?} (Converted to {:?})",
        args.trigger_key, target_key
    );
    println!("--- Modes ---");
    println!(
        " > Image OCR: Lang='{}', Tesseract='{}'",
        args.lang, args.tesseract_cmd
    );
    if let Some(p) = &args.tessdata_path {
        println!("   Tessdata: '{}'", p);
    }
    if !args.tesseract_args.is_empty() {
        println!("   Extra Tesseract Args: {:?}", args.tesseract_args);
    }
    // Check the potentially updated args.openai_api_key
    if args.openai_api_key.is_some() {
        println!(" > Audio Transcription: Enabled (Whisper API)");
        println!("   Requires: ffmpeg in PATH, valid API Key.");
    } else {
        // Make message more informative
        println!(" > Audio Transcription: Disabled (API Key not provided via arg or found in .env/environment)");
    }
    println!("---");
    println!(
        "Press '{:?}' when an image OR a single audio file is in the clipboard to process.",
        args.trigger_key
    );
    println!(
        "NOTE: This program likely requires administrator privileges for global key listening."
    );
    println!("Ctrl+C in this window to exit.");
    println!("---");

    // Clone args for the callback closure
    let args_clone = {
        Args {
            trigger_key: args.trigger_key,
            lang: args.lang.clone(),
            tesseract_cmd: args.tesseract_cmd.clone(),
            tessdata_path: args.tessdata_path.clone(),
            tesseract_args: args.tesseract_args.clone(),
            openai_api_key: args.openai_api_key.clone(), // Clone the potentially updated key
        }
    };

    let callback = move |event: Event| {
        if let EventType::KeyPress(key) = event.event_type {
            if key == target_key {
                println!("\n--- Trigger key pressed! ---");
                match get_clipboard_content() {
                    Ok(original_content) => {
                        // Pass the cloned args which contain the resolved API key
                        if let Err(e) =
                            process_clipboard_and_paste(original_content, &args_clone, &rt)
                        {
                            eprintln!("ERROR during clipboard processing/restoration: {:?}", e);
                        }
                    }
                    Err(e) => {
                        eprintln!("ERROR getting clipboard content: {:?}", e);
                    }
                }
                println!("--- Ready for next trigger ---");
            }
        }
    };

    if let Err(error) = listen(callback) {
        eprintln!(
            "FATAL ERROR setting up global keyboard listener: {:?}",
            error
        );
        eprintln!("This might be a permissions issue. Try running as administrator.");
        return Err(anyhow!("Keyboard listener setup failed: {:?}", error));
    }

    Ok(())
}

fn set_clipboard_string_helper(text: &str) -> Result<()> {
    clipboard_win::set_clipboard_string(text)
        .map_err(|e| anyhow!("Failed to set clipboard string: {}", e))
}
