// src/main.rs

use anyhow::{anyhow, Context as AnyhowContext, Result}; // Keep AnyhowContext if used elsewhere, otherwise just Context
use clap::Parser;
use clipboard_win::{
    formats,
    get_clipboard,
    Clipboard, // Keep for explicit open/close
    Setter,
};
use dotenvy;

use image::ImageFormat;
use rdev::{listen, simulate, Event, EventType, Key};
use std::{
    env,
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender}, // Keep channel imports
    thread,                               // Keep thread import
    time::Duration,
};
use tempfile::Builder as TempFileBuilder;

mod easy_rdev_key;
use easy_rdev_key::PTTKey;
mod transcribe;

use async_openai::{config::OpenAIConfig, Client};
use tokio::runtime::Runtime;

const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "ogg", "flac", "aac", "wma", "opus", "aiff", "aif",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "mpeg", "mpg", "m4v", "3gp",
];

const CLIPBRD_E_UNSUPPORTEDFORMAT: i32 = -2147221040;

// --- Args struct remains the same (with Clone) ---
#[derive(Parser, Debug, Clone)]
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
        help = "Tesseract language code(s)."
    )]
    lang: String,
    #[arg(long, default_value = "tesseract", help = "Tesseract command/path.")]
    tesseract_cmd: String,
    #[arg(long, help = "Path to Tesseract data directory.")]
    tessdata_path: Option<String>,
    #[arg(long, help = "Additional Tesseract CLI args.", num_args = 0..)]
    tesseract_args: Vec<String>,
    #[arg(long, help = "OpenAI API Key (overrides .env/env var).")]
    openai_api_key: Option<String>,
}

// --- ClipboardContent enum remains the same ---
#[derive(Debug)]
enum ClipboardContent {
    Bitmap(Vec<u8>),
    FileList(Vec<String>),
}

// --- Helper Functions (Restored with map_err for clipboard results) ---
fn get_clipboard_content() -> Result<ClipboardContent> {
    fn try_get_clipboard_content() -> Result<ClipboardContent, clipboard_win::ErrorCode> {
        let _clip = Clipboard::new_attempts(10)?; // Open clipboard

        match get_clipboard::<Vec<String>, _>(formats::FileList) {
            Ok(files) => {
                println!("Clipboard contains FileList: {:?}", files);
                return Ok(ClipboardContent::FileList(files));
            }
            Err(e) => {
                if e.raw_code() != CLIPBRD_E_UNSUPPORTEDFORMAT {
                    println!("Warning: Failed to get FileList: {}. Trying Bitmap.", e);
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
                    println!("Warning: Failed to get Bitmap: {}", e);
                } else {
                    println!("Clipboard does not contain Bitmap format either.");
                }
                return Err(e); // Return specific error
            }
        }
        // _clip drops here
    }

    try_get_clipboard_content().map_err(|e| {
        // Map ErrorCode -> anyhow::Error
        anyhow!(
            "Failed to get supported content (FileList/Bitmap) from clipboard: {}",
            e
        )
    })
}

fn restore_clipboard(content: ClipboardContent) -> Result<()> {
    let _clip = Clipboard::new_attempts(10)
        .map_err(|e| anyhow!("Failed to open clipboard for restoration: {}", e))?; // Map ErrorCode

    match content {
        ClipboardContent::Bitmap(data) => {
            println!("Restoring Bitmap to clipboard...");
            formats::Bitmap
                .write_clipboard(&data)
                .map_err(|e| anyhow!("Failed to restore Bitmap to clipboard: {}", e))
            // Map ErrorCode
        }
        ClipboardContent::FileList(files) => {
            println!("Restoring FileList to clipboard...");
            formats::FileList
                .write_clipboard(&files)
                .map_err(|e| anyhow!("Failed to restore FileList to clipboard: {}", e))
            // Map ErrorCode
        }
    }
    // _clip drops here
}

fn set_clipboard_string_helper(text: &str) -> Result<()> {
    let _clip = Clipboard::new_attempts(10)
        .map_err(|e| anyhow!("Failed to open clipboard to set string: {}", e))?; // Map ErrorCode

    clipboard_win::set_clipboard_string(text)
        .map_err(|e| anyhow!("Failed to set clipboard string: {}", e)) // Map ErrorCode
                                                                       // _clip drops here
}

// --- process_clipboard_and_paste (Restored Full Implementation) ---
fn process_clipboard_and_paste(
    original_content: ClipboardContent,
    args: &Args,
    rt: &Runtime,
) -> Result<()> {
    let mut _temp_audio_file_guard = None;
    let mut _temp_image_file_guard = None;

    let processed_text_result = match &original_content {
        ClipboardContent::FileList(files) => {
            if files.len() == 1 {
                // file_path is declared here and used within this block
                let file_path = PathBuf::from(&files[0]);
                let extension = file_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.to_lowercase())
                    .unwrap_or_default();

                let audio_path_to_transcribe: PathBuf;

                if AUDIO_EXTENSIONS.contains(&extension.as_str()) {
                    println!("Detected single audio file: {:?}", file_path);
                    audio_path_to_transcribe = file_path.clone();
                } else if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
                    println!(
                        "Detected single video file: {:?}. Extracting audio...",
                        file_path
                    );
                    println!("INFO: Video processing requires ffmpeg in PATH.");

                    let temp_audio_file = TempFileBuilder::new()
                        .prefix("extracted_audio_")
                        .suffix(".mp3")
                        .tempfile_in(std::env::temp_dir())
                        // Use AnyhowContext here as tempfile returns std::io::Result
                        .with_context(|| "Failed to create temporary file for extracted audio")?;

                    let temp_audio_path_obj = temp_audio_file.path().to_path_buf();
                    _temp_audio_file_guard = Some(temp_audio_file);

                    println!(
                        "Extracting audio via ffmpeg to temporary file: {:?}",
                        temp_audio_path_obj
                    );
                    let ffmpeg_output = Command::new("ffmpeg")
                        .arg("-i")
                        .arg(&file_path)
                        .arg("-vn")
                        .arg("-q:a")
                        .arg("0")
                        .arg("-y")
                        .arg(&temp_audio_path_obj)
                        .output()
                        // Use AnyhowContext here as output() returns std::io::Result
                        .with_context(|| {
                            "Failed to execute ffmpeg command. Is ffmpeg installed and in PATH?"
                        })?;

                    if !ffmpeg_output.status.success() {
                        let stderr = String::from_utf8_lossy(&ffmpeg_output.stderr);
                        return Err(anyhow!(
                            "ffmpeg failed to extract audio (Status: {}):\n{}",
                            ffmpeg_output.status,
                            stderr
                        ));
                    }

                    println!("Audio extraction successful.");
                    audio_path_to_transcribe = temp_audio_path_obj;
                } else {
                    return Err(anyhow!(
                        "Clipboard contains a single file, but it's not a supported audio or video format (Checked extensions: {:?}, {:?}, Found: {}).",
                        AUDIO_EXTENSIONS, VIDEO_EXTENSIONS, extension
                    ));
                }

                // Perform Transcription
                let api_key = args.openai_api_key.as_ref().ok_or_else(|| {
                    anyhow!("OpenAI API Key is missing (checked arg, .env, env var).")
                })?;
                let config = OpenAIConfig::new().with_api_key(api_key);
                let client = Client::with_config(config);

                rt.block_on(transcribe::trans::transcribe(
                    &client,
                    &audio_path_to_transcribe,
                ))
                // Use AnyhowContext as transcribe returns anyhow::Result
                .with_context(|| {
                    format!(
                        "Audio transcription failed for: {:?}",
                        audio_path_to_transcribe
                    )
                })
            } else {
                Err(anyhow!(
                    "Clipboard contains {} files. Only single audio/video file processing is supported.",
                    files.len()
                ))
            }
        }
        ClipboardContent::Bitmap(bitmap_data) => {
            println!("Processing clipboard image with Tesseract OCR...");
            let temp_image_file = TempFileBuilder::new()
                .prefix("clipboard_ocr_")
                .suffix(".png")
                .tempfile_in(std::env::temp_dir())
                // Use AnyhowContext here as tempfile returns std::io::Result
                .with_context(|| "Failed to create temporary file for OCR image")?;

            let temp_image_path = temp_image_file.path().to_path_buf();
            _temp_image_file_guard = Some(temp_image_file);

            // Use AnyhowContext as load_from_memory returns image::ImageResult
            let img = image::load_from_memory(bitmap_data)
                .with_context(|| "Failed to decode clipboard image data")?;
            println!(
                "Decoded image. Saving temporary PNG to {:?}",
                temp_image_path
            );
            // Use AnyhowContext as save_with_format returns image::ImageResult
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

            // Use AnyhowContext as output() returns std::io::Result
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
                // Use AnyhowContext as from_utf8 returns std::result::Result
                String::from_utf8(output.stdout)
                    .with_context(|| "Tesseract output was not valid UTF-8")
            }
        }
    };

    // Handle result of processing
    match processed_text_result {
        Ok(processed_text) => {
            let trimmed_text = processed_text.trim();
            if trimmed_text.is_empty() {
                println!("Processing resulted in empty text. Skipping paste.");
                // Use AnyhowContext as restore_clipboard now returns anyhow::Result
                restore_clipboard(original_content).with_context(|| {
                    "Failed to restore original clipboard content after empty result"
                })?;
                Ok(())
            } else {
                println!("Processed Text (first 100 chars): {:.100}...", trimmed_text);

                // Use AnyhowContext as helper returns anyhow::Result
                set_clipboard_string_helper(trimmed_text)
                    .with_context(|| "Failed to place processed text onto clipboard")?;
                println!("Processed text placed on clipboard. Simulating paste (Ctrl+V)...");
                thread::sleep(Duration::from_millis(150));
                // Use AnyhowContext as send_ctrl_v returns anyhow::Result if mapped, or specific error
                send_ctrl_v().map_err(|e| anyhow!("Simulate Ctrl+V error: {}", e))?; // Map error if needed

                thread::sleep(Duration::from_millis(150));
                // Use AnyhowContext as restore_clipboard returns anyhow::Result
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
    // Temp guards drop here
}

// --- send_ctrl_v (No changes) ---
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

// --- Main Function (Restored dotenv match, fixed imports) ---
fn main() -> Result<()> {
    // Load .env file
    // Corrected match statement
    match dotenvy::dotenv() {
        Ok(path) => println!("Loaded environment variables from: {:?}", path),
        Err(e) => {
            if !e.not_found() {
                eprintln!("Warning: Failed to load .env file: {}", e);
            } else {
                println!("No .env file found, proceeding without it.");
            }
        }
    };

    let mut args = Args::parse();
    if args.openai_api_key.is_none() {
        if let Ok(key) = env::var("OPENAI_API_KEY") {
            if !key.is_empty() {
                args.openai_api_key = Some(key);
            }
        }
    }

    let target_key: rdev::Key = args.trigger_key.into();
    let args_clone_for_worker = args.clone();

    // Startup Info
    println!("Clipboard Processor Started.");
    println!(
        "Trigger Key: {:?} (Converted to {:?})",
        args.trigger_key, target_key
    );
    // ... (rest of startup messages) ...
    if args.openai_api_key.is_some() {
        println!(" > Audio/Video Transcription: Enabled (Whisper API via ffmpeg)");
        println!("   Requires: ffmpeg in PATH, valid API Key.");
    } else {
        println!(" > Audio/Video Transcription: Disabled (API Key not provided via arg or found in .env/environment)");
    }
    println!("---");
    println!(
        "Press '{:?}' when an image OR a single audio/video file is in the clipboard to process.",
        args.trigger_key
    );
    // ...

    let (event_tx, event_rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();

    // Spawn Worker Thread
    let worker_handle = thread::spawn(move || {
        println!("Worker thread started.");
        // Create Tokio runtime inside the worker thread
        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!(
                    "FATAL: Failed to create Tokio runtime in worker thread: {}",
                    e
                );
                return;
            }
        };

        for event in event_rx {
            if let EventType::KeyPress(key) = event.event_type {
                if key == target_key {
                    println!("\n--- Trigger key pressed (received by worker) ---");
                    match get_clipboard_content() {
                        Ok(original_content) => {
                            if let Err(e) = process_clipboard_and_paste(
                                original_content,
                                &args_clone_for_worker,
                                &rt,
                            ) {
                                eprintln!("ERROR during clipboard processing/restoration: {:?}", e);
                            }
                        }
                        Err(e) => {
                            eprintln!("ERROR getting clipboard content: {:?}", e);
                        }
                    }
                    println!("--- Worker ready for next trigger ---");
                }
            }
        }
        println!("Worker thread finished.");
    });

    // Setup and Run Keyboard Listener
    println!("Setting up keyboard listener...");
    let callback = move |event: Event| {
        let _ = event_tx.send(event);
    };

    if let Err(error) = listen(callback) {
        eprintln!(
            "FATAL ERROR setting up global keyboard listener: {:?}",
            error
        );
        eprintln!("This might be a permissions issue. Try running as administrator.");
        return Err(anyhow!("Keyboard listener setup failed: {:?}", error));
    }

    // Optional: Join worker handle if listen could ever finish (unlikely)
    // worker_handle.join().expect("Worker thread panicked");

    Ok(())
}
