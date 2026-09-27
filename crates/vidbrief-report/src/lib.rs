//! 步 4:报告渲染。
//!
//! 设计见 `docs/DESIGN.md` §8。**渲染链是数据为源**([260926-07]):
//! `report.html` 与 `report.md` 都由同一份 `04_report/report.json`
//! ([`vidbrief_project::Report`])渲染 —— 两个输出都是它的**投影**,谁也不从谁反推。
//! 交互与浮层位置 Markdown 表达不了,所以页面才是主形态;换样式只动渲染器,数据一个字节不动。
//!
//! | 模块 | 干什么 |
//! |------|--------|
//! | [`md`] | 导出 Markdown(`report.md`) |
//! | [`html`] | 把数据塞进 `ui/report.html` 模板(`report.html`) |
//!
//! 图片默认走**相对路径**(避免 base64 单文件膨胀,旧样例报告曾因此到 889KB);
//! 要**单文件分享**时切 [`ImageMode::Embedded`](图内嵌成 `data:` URL)。
//! HTML 的视频回跳复用 `vidbrief-viz` 的播放器与本地查看服务。

pub mod html;
pub mod md;

pub use html::{Notices, render_report, render_report_html};
pub use md::{export_markdown, write_markdown};

/// **图怎么进产物** —— 两个投影共用这一档。
///
/// | 模式 | 图长什么样 | 用在哪 |
/// |------|-----------|--------|
/// | [`ImageMode::Relative`] | `assets/s007.jpg`(相对 `04_report/`) | 默认:体积小,素材就在项目里 |
/// | [`ImageMode::Embedded`] | `data:image/jpeg;base64,…` | **单文件分享**:把 `report.html` 单独发出去 |
///
/// 两件事要知道:
/// 1. **视频不内嵌**(几百 MB 起,base64 还要再涨三分之一)—— 所以单文件版是
///    "图能带走,点锚点回跳那一半要本机素材 + 本地服务"。页面里也这么写,不藏着。
/// 2. 体积:一张 1280px 的截图约 100–200 KB,base64 之后 ×1.37;36 张的页面会上到几 MB。
///    **分享用**可以,当默认路径不行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageMode {
    /// 相对路径(默认)。
    #[default]
    Relative,
    /// 内嵌 base64 —— 单文件分享。
    Embedded,
}

impl ImageMode {
    /// 把一张图变成它在产物里的样子。
    ///
    /// 读不动就退回相对路径 —— 产物里留一张裂图,正是"这张图不在了"该有的样子;
    /// 悄悄把这条图去掉才是坏事(报告引了它,读者要能看出它没了)。
    pub(crate) fn encode(&self, layout: &Layout, rel: &str, relative_url: &str) -> String {
        match self {
            Self::Relative => relative_url.to_string(),
            Self::Embedded => match std::fs::read(layout.rel(rel)) {
                Ok(bytes) => format!("data:{};base64,{}", mime_of(rel), BASE64.encode(bytes)),
                Err(_) => relative_url.to_string(),
            },
        }
    }
}

use base64::Engine as _;
use vidbrief_project::Layout;

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// 按扩展名判 MIME。截图是 JPEG(DESIGN §5.1 ③);别的格式也认一下,免得发出去打不开。
fn mime_of(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}
