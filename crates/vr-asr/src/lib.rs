//! 语音转文字(步 1 的第二个产出)。
//!
//! 设计见 `docs/DESIGN.md` §5.1 ②。
//!
//! 职责(待实现):
//! - 用 ffmpeg 抽 16kHz 单声道 wav
//! - 调 `whisper-cli`(whisper.cpp,二进制与模型在 `tools/whisper/`)输出 txt/srt/vtt
//! - 解析 SRT → `transcript.json`(结构化分段)
//!
//! 注意:本模块**不做任何纠错**。识别错误由 agent 在步 2 修正(§6.2 ②)。
