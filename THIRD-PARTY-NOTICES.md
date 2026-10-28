# Third-party notices

Better Dictation's own code is MIT licensed (see LICENSE). The installer also ships these:

| Component | Used for | Licence | Source |
|---|---|---|---|
| Kroko ASR streaming zipformer (English, 2025-08-06), by Banafo | Live typing | CC-BY-SA 4.0 | https://huggingface.co/Banafo/Kroko-ASR (packaged by sherpa-onnx) |
| NVIDIA Parakeet TDT 0.6B v2 (int8 export) | Accuracy pass | CC-BY-4.0 | https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2 (int8 export by sherpa-onnx) |
| sherpa-onnx, by k2-fsa | Speech recognition engine | Apache-2.0 | https://github.com/k2-fsa/sherpa-onnx |
| ONNX Runtime, by Microsoft | Model runtime (inside sherpa-onnx) | MIT | https://github.com/microsoft/onnxruntime |
| Tauri | App framework | MIT / Apache-2.0 | https://tauri.app |
| cpal | Microphone capture | Apache-2.0 | https://github.com/RustAudio/cpal |

The two model files are redistributed unchanged. Under CC-BY-SA 4.0, anyone who changes and
redistributes the Kroko model must share that changed model under the same licence; this does not
extend to Better Dictation's code.
