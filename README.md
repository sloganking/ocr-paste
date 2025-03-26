# OCRP - Clipboard Image OCR & Paste (Windows)

A simple Rust utility for Windows that listens for a global hotkey. When the key is pressed, it:

1.  Retrieves the image currently stored in the Windows clipboard.
2.  Uses the external **Tesseract OCR command-line tool** to extract text from that image.
3.  Simulates a `Ctrl+V` (paste) command to insert the extracted text into the currently active application.
4.  Restores the original image back to the clipboard.

This allows for quickly converting images (like screenshots of text) into actual text without manually opening an OCR tool, saving the image, processing it, and copying the text.

## Workflow Suggestion with Flameshot

A highly effective way to use OCRP is in combination with a screen capture tool like [Flameshot](https://flameshot.org/).

1.  Install Flameshot and configure it (e.g., set up a hotkey to launch its capture mode).
2.  Use Flameshot to select a region of your screen containing the text you want to OCR.
3.  Instead of saving the file, simply press `Ctrl+C` within Flameshot to copy the captured image directly to your clipboard.
4.  Now you have two choices:
    *   Press `Ctrl+V` in an application to paste the captured *image*.
    *   Press your configured OCRP **trigger key** (e.g., `F13`) to paste the extracted *text* from the image.

## Features

*   **Global Hotkey Trigger:** Runs in the background and activates anywhere when you press the configured key.
*   **Configurable Key:** Choose your preferred trigger key via a command-line argument (uses `clap` and `rdev`).
*   **Uses Tesseract CLI:** Avoids complex native build dependencies by calling the standard `tesseract.exe`.
*   **Language Support:** Specify the language(s) for Tesseract OCR.
*   **Preserves Original Image:** The image is restored to the clipboard after the text is pasted.

## Prerequisites

1.  **Rust:** You need the Rust toolchain (including `cargo`) installed. Get it from [rustup.rs](https://rustup.rs/).
2.  **Tesseract OCR:**
    *   The Tesseract OCR engine **must be installed separately**. Pre-compiled binaries for Windows are often available (e.g., from the [UB Mannheim Tesseract Wiki](https://github.com/UB-Mannheim/tesseract/wiki)).
    *   The `tesseract.exe` executable **must be in your system's `PATH` environment variable**, OR you must specify its full path using the `--tesseract-cmd` argument when running OCRP.
    *   You need the **language data files** (e.g., `eng.traineddata` for English) for the languages you want to use. These usually come with the installer or need to be downloaded separately and placed in Tesseract's `tessdata` directory. You can tell OCRP where this directory is using the `--tessdata-path` argument if it's not found automatically.
    *   **Test your Tesseract installation:** Open a Command Prompt or PowerShell and run `tesseract --version` and `tesseract --list-langs` to ensure it works and has the languages you need.
3.  **Windows Operating System:** This tool relies on `clipboard-win` and `rdev`'s Windows implementation.
4.  **Administrator Privileges:** Running the program **as Administrator** is usually required for `rdev` to capture global keyboard events reliably.

## Installation / Building

1.  **Clone the repository:**
    ```bash
    git clone <your-repo-url>
    cd ocrp # Or your project directory name
    ```
2.  **Build the release executable:**
    ```bash
    cargo build --release
    ```
3.  The compiled program will be located at `target/release/ocrp.exe`.

## Usage

Run the executable from a terminal (preferably one opened **as Administrator**). You *must* provide the `--trigger-key` argument.

```bash
# If running directly from target/release
.\ocrp.exe --trigger-key <KEY_NAME> [OPTIONS]

# Or using cargo run (from the project root)
cargo run --release -- --trigger-key <KEY_NAME> [OPTIONS]
