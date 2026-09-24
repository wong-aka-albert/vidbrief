//! 步 4:报告渲染。
//!
//! 设计见 `docs/DESIGN.md` §8。
//!
//! 职责(待实现):
//! - 把 `summary.md` / `report.md` 渲染为图表化 HTML:时间轴导航、关键帧图册、
//!   差帧图重现、图文对照卡片、纠错附录
//! - 图片默认走**相对路径** `04_report/assets/`(避免 base64 单文件膨胀);
//!   `self_contained` 为可选模式
//! - HTML 亦支持视频回跳(复用 `vidbrief-viz` 的播放器组件)
