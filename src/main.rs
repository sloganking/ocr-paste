// src/main.rs

use anyhow::{anyhow, Context as AnyhowContext, Result};
use clap::Parser;
use clipboard_win::{formats, get_clipboard, Clipboard, Setter};
use dotenvy;
// Use winapi import
use winapi::um::utilapiset::Beep;

use bardecoder;
use image::ImageFormat;
use rdev::{listen, simulate, Button, Event, EventType, Key};
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
use eframe::egui;
use rodio::source::{SineWave, Source};
use rodio::Decoder;
use std::io::{BufReader, Cursor};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use tray_item::TrayItem;

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

// Map egui key events to rdev keys for trigger recording inside the GUI
fn map_egui_key_to_ptt(key: egui::Key) -> Option<PTTKey> {
    use egui::Key as EK;
    match key {
        // Function keys
        EK::F1 => Some(PTTKey::F1),
        EK::F2 => Some(PTTKey::F2),
        EK::F3 => Some(PTTKey::F3),
        EK::F4 => Some(PTTKey::F4),
        EK::F5 => Some(PTTKey::F5),
        EK::F6 => Some(PTTKey::F6),
        EK::F7 => Some(PTTKey::F7),
        EK::F8 => Some(PTTKey::F8),
        EK::F9 => Some(PTTKey::F9),
        EK::F10 => Some(PTTKey::F10),
        EK::F11 => Some(PTTKey::F11),
        EK::F12 => Some(PTTKey::F12),
        // Extended function keys if egui provides them
        EK::F13 => Some(PTTKey::F13),
        EK::F14 => Some(PTTKey::F14),
        EK::F15 => Some(PTTKey::F15),
        EK::F16 => Some(PTTKey::F16),
        EK::F17 => Some(PTTKey::F17),
        EK::F18 => Some(PTTKey::F18),
        EK::F19 => Some(PTTKey::F19),
        EK::F20 => Some(PTTKey::F20),
        // Some egui versions may not expose F21–F24

        // Letters
        EK::A => Some(PTTKey::KeyA),
        EK::B => Some(PTTKey::KeyB),
        EK::C => Some(PTTKey::KeyC),
        EK::D => Some(PTTKey::KeyD),
        EK::E => Some(PTTKey::KeyE),
        EK::F => Some(PTTKey::KeyF),
        EK::G => Some(PTTKey::KeyG),
        EK::H => Some(PTTKey::KeyH),
        EK::I => Some(PTTKey::KeyI),
        EK::J => Some(PTTKey::KeyJ),
        EK::K => Some(PTTKey::KeyK),
        EK::L => Some(PTTKey::KeyL),
        EK::M => Some(PTTKey::KeyM),
        EK::N => Some(PTTKey::KeyN),
        EK::O => Some(PTTKey::KeyO),
        EK::P => Some(PTTKey::KeyP),
        EK::Q => Some(PTTKey::KeyQ),
        EK::R => Some(PTTKey::KeyR),
        EK::S => Some(PTTKey::KeyS),
        EK::T => Some(PTTKey::KeyT),
        EK::U => Some(PTTKey::KeyU),
        EK::V => Some(PTTKey::KeyV),
        EK::W => Some(PTTKey::KeyW),
        EK::X => Some(PTTKey::KeyX),
        EK::Y => Some(PTTKey::KeyY),
        EK::Z => Some(PTTKey::KeyZ),

        // Digits (top row)
        EK::Num0 => Some(PTTKey::Num0),
        EK::Num1 => Some(PTTKey::Num1),
        EK::Num2 => Some(PTTKey::Num2),
        EK::Num3 => Some(PTTKey::Num3),
        EK::Num4 => Some(PTTKey::Num4),
        EK::Num5 => Some(PTTKey::Num5),
        EK::Num6 => Some(PTTKey::Num6),
        EK::Num7 => Some(PTTKey::Num7),
        EK::Num8 => Some(PTTKey::Num8),
        EK::Num9 => Some(PTTKey::Num9),

        // Navigation and control
        EK::Enter => Some(PTTKey::Return),
        EK::Space => Some(PTTKey::Space),
        EK::Backspace => Some(PTTKey::Backspace),
        EK::Tab => Some(PTTKey::Tab),
        EK::Escape => Some(PTTKey::Escape),
        EK::Insert => Some(PTTKey::Insert),
        EK::Home => Some(PTTKey::Home),
        EK::End => Some(PTTKey::End),
        EK::PageUp => Some(PTTKey::PageUp),
        EK::PageDown => Some(PTTKey::PageDown),
        EK::ArrowUp => Some(PTTKey::UpArrow),
        EK::ArrowDown => Some(PTTKey::DownArrow),
        EK::ArrowLeft => Some(PTTKey::LeftArrow),
        EK::ArrowRight => Some(PTTKey::RightArrow),

        // Unknown / not mapped
        _ => None,
    }
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
            println!("Processing clipboard image...");
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

            // First, try to detect and decode QR codes
            println!("Checking for QR codes...");
            let decoder = bardecoder::default_decoder();
            let qr_results: Vec<_> = decoder.decode(&img).into_iter().collect();

            let qr_text = if !qr_results.is_empty() {
                // Found QR codes, use the first successful result
                let mut qr_text = None;
                for result in qr_results {
                    match result {
                        Ok(text) => {
                            println!("QR code detected and decoded successfully!");
                            qr_text = Some(text);
                            break;
                        }
                        Err(e) => {
                            println!("QR code detected but failed to decode: {:?}", e);
                        }
                    }
                }
                qr_text
            } else {
                println!("No QR codes detected, proceeding with OCR...");
                None
            };

            // If we found a QR code, return it; otherwise try OCR
            if let Some(text) = qr_text {
                Ok(text)
            } else {
                // No QR codes found or all failed to decode, try OCR
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

                let output = command.output().map_err(|err| {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        anyhow!(
                            "Tesseract command '{}' not found. Please install Tesseract and ensure it is in your PATH.",
                            args.tesseract_cmd
                        )
                    } else {
                        anyhow!(
                            "Failed to execute Tesseract command '{}': {}",
                            args.tesseract_cmd,
                            err
                        )
                    }
                })?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(anyhow!(
                        "Tesseract CLI failed (Status: {}):\n{}",
                        output.status,
                        stderr
                    ))
                } else {
                    let text = String::from_utf8(output.stdout)
                        .with_context(|| "Tesseract output was not valid UTF-8")?;
                    Ok(text)
                }
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

// --- Trigger type and Settings GUI ---
#[derive(Debug, Clone, Copy, PartialEq)]
enum Trigger {
    Key(Key),
    Mouse(Button),
}

fn format_trigger(trigger: &Trigger) -> String {
    match trigger {
        Trigger::Key(k) => format!("Key: {:?}", k),
        Trigger::Mouse(b) => format!("Mouse: {:?}", b),
    }
}

struct SettingsApp {
    shared_trigger: Arc<Mutex<Trigger>>,
    is_recording: bool,
    last_set: Option<Trigger>,
    record_request: Arc<AtomicBool>,
}

impl SettingsApp {
    fn new(shared_trigger: Arc<Mutex<Trigger>>, record_request: Arc<AtomicBool>) -> Self {
        Self {
            shared_trigger,
            is_recording: false,
            last_set: None,
            record_request,
        }
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Recording handled by worker via global hook; GUI only toggles record flag

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("OCR Paste Settings");
            ui.add_space(8.0);

            // Current trigger display
            let current = {
                let guard = self.shared_trigger.lock().unwrap();
                format_trigger(&*guard)
            };
            ui.label(format!("Current trigger: {}", current));

            if let Some(tr) = self.last_set {
                ui.label(format!("Last set: {}", format_trigger(&tr)));
            }

            ui.add_space(10.0);
            if !self.is_recording {
                if ui.button("Set trigger...").clicked() {
                    self.is_recording = true;
                    self.record_request.store(true, Ordering::SeqCst);
                }
            } else {
                ui.colored_label(egui::Color32::YELLOW, "Press any key or mouse button...");
                if ui.button("Cancel").clicked() {
                    self.is_recording = false;
                    self.record_request.store(false, Ordering::SeqCst);
                }

                // Capture next egui input while recording and map to rdev trigger
                let events = ctx.input(|i| i.events.clone());
                for ev in events {
                    match ev {
                        egui::Event::Key {
                            key,
                            pressed,
                            repeat,
                            ..
                        } => {
                            if pressed && !repeat {
                                if let Some(ptt) = map_egui_key_to_ptt(key) {
                                    let rkey: Key = ptt.into();
                                    if let Ok(mut guard) = self.shared_trigger.lock() {
                                        *guard = Trigger::Key(rkey);
                                        self.last_set = Some(*guard);
                                    }
                                    self.is_recording = false;
                                    self.record_request.store(false, Ordering::SeqCst);
                                    break;
                                }
                            }
                        }
                        egui::Event::PointerButton {
                            button, pressed, ..
                        } => {
                            if pressed {
                                let rbutton = match button {
                                    egui::PointerButton::Primary => Button::Left,
                                    egui::PointerButton::Secondary => Button::Right,
                                    egui::PointerButton::Middle => Button::Middle,
                                    // Map extras to Unknown codes 4/5 for lack of exact mapping
                                    egui::PointerButton::Extra1 => Button::Unknown(4),
                                    egui::PointerButton::Extra2 => Button::Unknown(5),
                                };
                                if let Ok(mut guard) = self.shared_trigger.lock() {
                                    *guard = Trigger::Mouse(rbutton);
                                    self.last_set = Some(*guard);
                                }
                                self.is_recording = false;
                                self.record_request.store(false, Ordering::SeqCst);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }

            ui.add_space(12.0);
            ui.label("Close this window after setting the trigger.");
        });
    }
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
    let current_trigger = Arc::new(Mutex::new(Trigger::Key(target_key)));
    let current_trigger_for_worker = Arc::clone(&current_trigger);
    let current_trigger_for_gui = Arc::clone(&current_trigger);
    let record_request = Arc::new(AtomicBool::new(false));
    let record_request_for_worker = Arc::clone(&record_request);
    let record_request_for_gui = Arc::clone(&record_request);
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
    // Create system tray with embedded icon resource (built via build.rs)
    #[cfg(target_os = "windows")]
    let mut _tray: Option<TrayItem> = None;
    #[cfg(target_os = "windows")]
    {
        match TrayItem::new("OCR Paste", tray_item::IconSource::Resource("IDI_ICON1")) {
            Ok(mut tray) => {
                // Settings menu
                let trigger_for_menu = Arc::clone(&current_trigger_for_gui);
                let _ = tray.add_menu_item("Settings", move || {
                    let trigger_for_gui = Arc::clone(&trigger_for_menu);
                    let record_for_gui = Arc::clone(&record_request_for_gui);
                    std::thread::spawn(move || {
                        let mut native_options = eframe::NativeOptions::default();
                        native_options.event_loop_builder = Some(Box::new(|builder| {
                            #[cfg(target_os = "windows")]
                            {
                                use winit::platform::windows::EventLoopBuilderExtWindows;
                                builder.with_any_thread(true);
                            }
                        }));
                        let _ = eframe::run_native(
                            "OCR Paste Settings",
                            native_options,
                            Box::new(move |_cc| {
                                Box::new(SettingsApp::new(trigger_for_gui, record_for_gui))
                            }),
                        );
                    });
                });
                let _ = tray.add_menu_item("Exit", move || {
                    // Immediate exit on menu click
                    std::process::exit(0);
                });
                println!("System tray ready. Right-click for options (Exit).\n");
                _tray = Some(tray); // keep alive for program lifetime
            }
            Err(e) => {
                eprintln!("Warning: Failed to create system tray: {}", e);
            }
        }
    }

    let (event_tx, event_rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();

    // Spawn Worker Thread (Conditional Beeps)
    let worker_handle = thread::spawn(move || {
        // println!("Worker thread started.");
        // let rt = match Runtime::new() {
        //     Ok(rt) => rt,
        //     Err(e) => {
        //         eprintln!(
        //             "FATAL: Failed to create Tokio runtime in worker thread: {}",
        //             e
        //         );
        //         return;
        //     }
        // };

        for event in event_rx {
            // print the event unless it's MouseMove:
            if !matches!(event.event_type, EventType::MouseMove { .. }) {
                println!("Event: {:?}", event);
            }

            if record_request_for_worker.load(Ordering::SeqCst) {
                match event.event_type {
                    EventType::KeyPress(k) => {
                        if let Ok(mut guard) = current_trigger_for_worker.lock() {
                            *guard = Trigger::Key(k);
                        }
                        record_request_for_worker.store(false, Ordering::SeqCst);
                        println!("Set trigger (KeyPress) to {:?}", k);
                        continue;
                    }
                    EventType::ButtonPress(b) => {
                        if let Ok(mut guard) = current_trigger_for_worker.lock() {
                            *guard = Trigger::Mouse(b);
                        }
                        record_request_for_worker.store(false, Ordering::SeqCst);
                        println!("Set trigger (ButtonPress) to {:?}", b);
                        continue;
                    }
                    _ => {}
                }
            }

            //     let should_trigger = match event.event_type {
            //         EventType::KeyPress(key) => {
            //             let guard = current_trigger_for_worker.lock().unwrap();
            //             matches!(*guard, Trigger::Key(k) if k == key)
            //         }
            //         EventType::ButtonPress(button) => {
            //             let guard = current_trigger_for_worker.lock().unwrap();
            //             matches!(*guard, Trigger::Mouse(b) if b == button)
            //         }
            //         _ => false,
            //     };

            //     if should_trigger {
            //         println!("\n--- Trigger key pressed (received by worker) ---");

            //         // Play START sound only if flag is set
            //         if args_clone_for_worker.beeps {
            //             play_sound(SoundType::Start);
            //         }

            //         let process_result = {
            //             match get_clipboard_content() {
            //                 Ok(original_content) => process_clipboard_and_paste(
            //                     original_content,
            //                     &args_clone_for_worker,
            //                     &rt,
            //                 ),
            //                 Err(e) => {
            //                     eprintln!("ERROR getting clipboard content: {:?}", e);
            //                     Err(e)
            //                 }
            //             }
            //         };

            //         // Check result and play appropriate sound
            //         match process_result {
            //             Ok(_) => {
            //                 // Play SUCCESS sound only if flag is set
            //                 if args_clone_for_worker.beeps {
            //                     play_sound(SoundType::Success);
            //                 }
            //             }
            //             Err(e) => {
            //                 // Always play ERROR sound
            //                 play_sound(SoundType::Error);
            //                 // Print error for visibility
            //                 eprintln!("{}", e);
            //             }
            //         }

            //         println!("--- Worker ready for next trigger ---");
            //     }
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
