// src/main.rs

use anyhow::{anyhow, Context as AnyhowContext, Result};
use clap::Parser;
use clipboard_win::{formats, get_clipboard, Clipboard, Setter};
use dotenvy;
// Use winapi import
use winapi::um::utilapiset::Beep;

use image::ImageFormat;
use rdev::{listen, simulate, Event, EventType, Key};
use std::{
    env,
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};
use tempfile::Builder as TempFileBuilder;

mod easy_rdev_key;
use easy_rdev_key::PTTKey;
mod transcribe;

use async_openai::{config::OpenAIConfig, Client};
use default_device_sink::DefaultDeviceSink;
use rodio::source::{SineWave, Source};
use rodio::Decoder;
use std::io::{BufReader, Cursor};
use tokio::runtime::Runtime;

// --- Constants ---
const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "ogg", "flac", "aac", "wma", "opus", "aiff", "aif",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "mpeg", "mpg", "m4v", "3gp",
];
const CLIPBRD_E_UNSUPPORTEDFORMAT: i32 = -2147221040;

// --- Args Struct ---
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
    // --- Added Beeps Flag ---
    #[arg(long, help = "Enable start and success notification beeps.")]
    beeps: bool,
}

// --- ClipboardContent Enum ---
#[derive(Debug)]
enum ClipboardContent {
    Bitmap(Vec<u8>),
    FileList(Vec<String>),
}

// --- Sound Type Enum ---
enum SoundType {
    Start,
    Success,
    Error,
}

// --- Helper: Play Sound (Windows Version) ---
fn play_sound(sound: SoundType) {
    let (freq_hz, dur_ms) = match sound {
        SoundType::Start => (880, 150),    // A5
        SoundType::Success => (1047, 300), // C6 (rounded)
        SoundType::Error => (262, 500),    // C4 (rounded)
    };
    unsafe {
        // Beep returns 0 on failure, non-zero on success. We ignore the result.
        let _ = Beep(freq_hz, dur_ms);
    }
    // Small delay to prevent sounds overlapping if triggered quickly
    thread::sleep(Duration::from_millis(50));
}

// --- Audio Helpers ---
static TICK_BYTES: &[u8] = include_bytes!("../assets/tick.mp3");
static FAILED_BYTES: &[u8] = include_bytes!("../assets/failed.mp3");

fn tick_loop(stop_rx: mpsc::Receiver<()>) {
    let tick_sink = DefaultDeviceSink::new();
    loop {
        if stop_rx.try_recv().is_ok() {
            tick_sink.stop();
            break;
        }
        if tick_sink.empty() {
            let cursor = Cursor::new(TICK_BYTES);
            if let Ok(decoder) = Decoder::new(BufReader::new(cursor)) {
                tick_sink.stop();
                tick_sink.append(decoder);
            } else {
                tick_sink.stop();
                tick_sink.append(
                    SineWave::new(880.0)
                        .take_duration(Duration::from_millis(50))
                        .amplify(0.20),
                );
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn play_failure_sound() {
    let sink = DefaultDeviceSink::new();
    if let Ok(decoder) = Decoder::new(BufReader::new(Cursor::new(FAILED_BYTES))) {
        sink.append(decoder);
    } else {
        sink.append(
            SineWave::new(440.0)
                .take_duration(Duration::from_millis(150))
                .amplify(0.20),
        );
    }
    sink.sleep_until_end();
}

// --- Helper Functions (Full Implementations) ---
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

// --- process_clipboard_and_paste (Full Implementation) ---
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

                let (tick_tx, tick_rx) = mpsc::channel();
                let tick_handle = thread::spawn(move || tick_loop(tick_rx));

                let transcription_result = rt.block_on(transcribe::trans::transcribe(
                    &client,
                    &audio_path_to_transcribe,
                ));

                let _ = tick_tx.send(());
                let _ = tick_handle.join();

                transcription_result
                    .with_context(|| {
                        format!(
                            "Audio transcription failed for: {:?}",
                            audio_path_to_transcribe
                        )
                    })
                    .map_err(|e| {
                        play_failure_sound();
                        e
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
                .with_context(|| "Failed to create temporary file for OCR image")?;

            let temp_image_path = temp_image_file.path().to_path_buf();
            _temp_image_file_guard = Some(temp_image_file);

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

    // Handle result of processing
    match processed_text_result {
        Ok(processed_text) => {
            let trimmed_text = processed_text.trim();
            if trimmed_text.is_empty() {
                println!("Processing resulted in empty text. Skipping paste.");
                restore_clipboard(original_content).with_context(|| {
                    "Failed to restore original clipboard content after empty result"
                })?;
                // Still consider this a "success" in terms of overall operation completion,
                // so Success beep might still be appropriate if enabled.
                Ok(())
            } else {
                println!("Processed Text (first 100 chars): {:.100}...", trimmed_text);

                set_clipboard_string_helper(trimmed_text)
                    .with_context(|| "Failed to place processed text onto clipboard")?;
                println!("Processed text placed on clipboard. Simulating paste (Ctrl+V)...");
                thread::sleep(Duration::from_millis(150));
                send_ctrl_v().map_err(|e| anyhow!("Simulate Ctrl+V error: {}", e))?;

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
            Err(e) // Propagate the error
        }
    }
    // Temp guards drop here
}

// --- send_ctrl_v (Full Implementation) ---
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

// --- Main Function (Conditional Sound Calls) ---
fn main() -> Result<()> {
    // Load .env file
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
    let args_clone_for_worker = args.clone(); // Clone includes the 'beeps' flag state

    // Startup Info
    println!("Clipboard Processor Started.");
    println!(
        "Trigger Key: {:?} (Converted to {:?})",
        args.trigger_key, target_key
    );
    println!("Optional Beeps Enabled: {}", args.beeps); // Log beep flag status
                                                        // ... (rest of startup messages) ...
    if args.openai_api_key.is_some() { /* ... */
    } else { /* ... */
    }
    println!("---");
    println!(
        "Press '{:?}' when an image OR a single audio/video file is in the clipboard to process.",
        args.trigger_key
    );
    // ...

    let (event_tx, event_rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();

    // Spawn Worker Thread (Conditional Beeps)
    let worker_handle = thread::spawn(move || {
        println!("Worker thread started.");
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

                    // Play START sound only if flag is set
                    if args_clone_for_worker.beeps {
                        play_sound(SoundType::Start);
                    }

                    let process_result = {
                        match get_clipboard_content() {
                            Ok(original_content) => process_clipboard_and_paste(
                                original_content,
                                &args_clone_for_worker,
                                &rt,
                            ),
                            Err(e) => {
                                eprintln!("ERROR getting clipboard content: {:?}", e);
                                Err(e)
                            }
                        }
                    };

                    // Check result and play appropriate sound
                    match process_result {
                        Ok(_) => {
                            // Play SUCCESS sound only if flag is set
                            if args_clone_for_worker.beeps {
                                play_sound(SoundType::Success);
                            }
                        }
                        Err(_) => {
                            // Always play ERROR sound
                            play_sound(SoundType::Error);
                            // Error message is already printed within process_clipboard_and_paste or get_clipboard_content
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

    // Optional: Join worker handle
    worker_handle.join().expect("Worker thread panicked");

    Ok(())
}
