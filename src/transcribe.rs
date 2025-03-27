// src/transcribe.rs
pub mod trans {

    use anyhow::{anyhow, bail, Context, Result};
    use async_openai::{config::OpenAIConfig, types::CreateTranscriptionRequestArgs, Client};
    use std::{
        path::{Path, PathBuf},
        process::Command,
    };
    use tempfile::tempdir;

    /// Converts audio to mp3 using ffmpeg if needed.
    /// Returns the path to the (potentially converted) mp3 file.
    /// The output mp3 is placed in a temporary directory managed by the caller.
    fn ensure_mp3(input: &Path, temp_dir_path: &Path) -> Result<PathBuf> {
        let input_extension = input.extension().unwrap_or_default().to_ascii_lowercase();

        if input_extension == "mp3" {
            // If it's already mp3, we can try using it directly.
            // Copying might be safer if the original path is weird, but let's try direct first.
            return Ok(input.to_path_buf());
        }

        // If not mp3, convert it to a temporary mp3 file
        let mut output_mp3_path = temp_dir_path.to_path_buf();
        // Create a unique filename within the temp dir
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let filename = format!("transcribe_temp_{}_{}.mp3", std::process::id(), timestamp);
        output_mp3_path.push(filename);

        println!(
            "Converting audio {:?} to temporary MP3: {:?}",
            input, output_mp3_path
        );

        // `ffmpeg -i input.ext -vn -ar 44100 -ac 2 -b:a 192k -f mp3 output.mp3`
        // Added common parameters for better compatibility
        let ffmpeg_output = Command::new("ffmpeg")
            .args([
                "-i",
                input
                    .to_str()
                    .context("Input path contains invalid UTF-8")?,
                "-vn", // No video
                "-ar",
                "44100", // Audio sample rate
                "-ac",
                "2", // Audio channels
                "-b:a",
                "192k", // Audio bitrate
                "-f",
                "mp3", // Force format to MP3
                output_mp3_path
                    .to_str()
                    .context("Output path contains invalid UTF-8")?,
            ])
            .output();

        match ffmpeg_output {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!(
                        "ffmpeg failed to convert audio (Status: {}). Stderr:\n{}",
                        output.status,
                        stderr
                    );
                }
                println!("ffmpeg conversion successful.");
                Ok(output_mp3_path)
            }
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    // Changed from panic! to Result
                    Err(anyhow!(
                        "ffmpeg command not found. Please install ffmpeg and ensure it's in your system's PATH."
                    ))
                } else {
                    Err(anyhow!("Failed to execute ffmpeg: {}", err))
                }
            }
        }
    }

    pub async fn transcribe(
        client: &Client<OpenAIConfig>,
        input_audio_path: &Path,
    ) -> Result<String> {
        // Changed return type to anyhow::Result

        // Create a temporary directory for potential ffmpeg conversion
        let temp_dir =
            tempdir().context("Failed to create temporary directory for audio processing")?;

        // Ensure we have an MP3 file, converting if necessary
        let input_mp3_path = ensure_mp3(input_audio_path, temp_dir.path())
            .context("Failed to prepare MP3 file for transcription")?;

        println!("Using audio file for transcription: {:?}", input_mp3_path);

        // Check file size before uploading (OpenAI has a 25MB limit)
        let metadata =
            std::fs::metadata(&input_mp3_path).context("Failed to get metadata for audio file")?;
        if metadata.len() > 25 * 1024 * 1024 {
            // Approx 25MB
            return Err(anyhow!(
                "Audio file size ({} bytes) exceeds the 25MB limit for Whisper API.",
                metadata.len()
            ));
        }
        if metadata.len() == 0 {
            return Err(anyhow!("Audio file is empty."));
        }

        // Build the transcription request
        // Consider making the prompt configurable if needed later
        let request = CreateTranscriptionRequestArgs::default()
            .file(input_mp3_path) // Pass the PathBuf directly
            .model("whisper-1")
            // .prompt("Optional prompt to guide the model.")
            .build()
            .context("Failed to build OpenAI transcription request")?;

        println!("Sending transcription request to OpenAI...");

        // Perform the transcription
        let response = client
            .audio()
            .transcribe(request)
            .await
            .context("OpenAI API request for transcription failed")?;

        println!("Transcription received from OpenAI.");
        Ok(response.text)

        // The temp_dir (and the converted mp3 within it, if created)
        // will be automatically deleted when `temp_dir` goes out of scope here.
    }
}
