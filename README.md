# Better Dictation

Live, private dictation for Windows. Press **Win + H**, talk, and your words are typed into
whatever you're working in, word by word, as you say them. Everything runs on your own PC; no
audio ever leaves it.

- **Replaces Windows dictation.** Win + H opens a small mic bubble that glows with your voice.
  The shortcut can be changed in Settings.
- **Live typing.** Words appear as you speak, not after you finish.
- **Accuracy pass.** A larger model re-checks your words as you talk and fixes mistakes in place,
  a few words behind the cursor. It never rewinds more than a few words, and only changes a word
  once it is sure. Auto (default) runs it only if your PC keeps up; it can also be set to Always or Off.
- **Clear numbers as digits.** "thirty five" becomes 35, "twenty twenty six" becomes 2026.
- **Stops when you're done.** Press Esc or the shortcut to stop (the last sentence is kept and
  corrected), or click anywhere or press any key.

## Install

Download the installer from Releases and run it. It installs for your user only (no admin
needed) and you can choose the folder. The speech models are included (about 520 MB).

Right-click the tray icon for Settings or Exit.

## Requirements

- Windows 10 or 11, 64-bit
- A microphone
- About 1 GB of free memory with the accuracy pass on (set it to Off for low-memory laptops)

## Known limits (0.1)

- Text can't be typed into apps running as administrator yet (Windows blocks it). A
  "run as administrator" option is planned.
- No "start with Windows" option yet.
- English only.

## Building

Rust (MSVC), Node.js, then:

```
npm install
npx tauri build
```

The models go in `models\` next to the project (see `src-tauri/tauri.conf.json` for the exact
folders); they come from the sherpa-onnx `asr-models` release.

## Licence

GPL-3.0 for the app's code. The bundled speech models have their own licences; see
THIRD-PARTY-NOTICES.md.

Made with AI assistance.
