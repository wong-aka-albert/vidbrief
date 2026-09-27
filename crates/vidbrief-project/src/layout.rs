//! 项目目录骨架:`projects/<id>/` 下的全部路径与建目录。
//!
//! 目录结构见 `docs/DESIGN.md` §4.1。**这里是路径的唯一来源** ——
//! 其它模块不允许自己拼 `"01_preprocess/..."` 这样的字面量。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};


/// 所有项目放在这个目录下(相对当前工作目录)。
pub const PROJECTS_DIR: &str = "projects";

// ── 项目根 ───────────────────────────────────────────────────────────
pub const PROJECT_FILE: &str = "project.json";
pub const LOCK_FILE: &str = "project.lock";
/// 项目状态(**非明文**):进度、核验戳、看图结论、拍板记录。见 [`crate::state`]。
pub const STATE_FILE: &str = "state.bin";

// ── source/ ─────────────────────────────────────────────────────────
pub const SOURCE_DIR: &str = "source";
pub const VIDEO_META_FILE: &str = "source/video.meta.json";

// ── 01_preprocess/ ──────────────────────────────────────────────────
pub const PREPROCESS_DIR: &str = "01_preprocess";
pub const CHANGE_CURVE_FILE: &str = "01_preprocess/change_curve.json";
pub const DENSE_REGIONS_FILE: &str = "01_preprocess/dense_regions.json";
pub const TRANSCRIPT_FILE: &str = "01_preprocess/transcript.json";
/// 步 1 算法抽出的截图(origin ∈ {dense_region, isolated_peak})。
pub const SHOTS_REL_DIR: &str = "01_preprocess/shots";
pub const SHOTS_FILE: &str = "01_preprocess/shots.json";
pub const PREPROCESS_TIMELINE_FILE: &str = "01_preprocess/timeline.html";
/// 步 1 的**输入指纹**(源视频 sha256 + 抽帧参数 + 模型 + 口径版本)。
///
/// 增量重跑靠它判断"这一步要不要重算":指纹逐项相同 → 不重算、也不作废成果
/// (尤其那批看图结论与核验戳 —— 重跑一次就全清,而校正是项目里最贵的活)。
/// 它是**记录**不是判据:判据永远是当场重算的那一份(见 `vidbrief-ops::inputs`)。
pub const PREPROCESS_INPUTS_FILE: &str = "01_preprocess/inputs.json";

// ── 02_calibrate/ ───────────────────────────────────────────────────
pub const CALIBRATE_DIR: &str = "02_calibrate";
pub const TRANSCRIPT_FIXED_FILE: &str = "02_calibrate/transcript.fixed.json";
pub const TRANSCRIPT_DIFF_FILE: &str = "02_calibrate/transcript.diff.md";
pub const SUPPLEMENT_REQUESTS_FILE: &str = "02_calibrate/supplement_requests.json";
pub const CROSS_VALIDATE_FILE: &str = "02_calibrate/cross_validate.md";
pub const CALIBRATION_LOG_FILE: &str = "02_calibrate/calibration_log.md";
/// 步 2 补图(origin ∈ {agent_request, manual})。
pub const SHOTS_EXTRA_REL_DIR: &str = "02_calibrate/shots_extra";
pub const CALIBRATE_TIMELINE_FILE: &str = "02_calibrate/timeline.html";
/// 校准对照表视图(一屏看完:图 + 转录 + 结论 + 校准日志)。见 `docs/DESIGN.md` §9.3。
pub const CALIBRATE_TABLE_FILE: &str = "02_calibrate/calibrate.html";

// ── 03_summary/ ─────────────────────────────────────────────────────
pub const SUMMARY_DIR: &str = "03_summary";
pub const AGENT_LOG_FILE: &str = "03_summary/agent_log.md";
pub const NOTES_REL_DIR: &str = "03_summary/notes";
pub const SUMMARY_FILE: &str = "03_summary/summary.md";
/// 步 3 的**结构化产物**:分段(`sections.json`)。
///
/// 报告页由它组装 —— 渲染那一步**不再需要模型给任何数据**(见 `crate::sections`)。
pub const SUMMARY_SECTIONS_FILE: &str = "03_summary/sections.json";
/// 步 3 的二次补图。
pub const SHOTS_EXTRA2_REL_DIR: &str = "03_summary/shots_extra2";

// ── 04_report/ ──────────────────────────────────────────────────────
pub const REPORT_DIR: &str = "04_report";
/// 报告**数据**(渲染器的输入,判据在它这儿;下面两份是它的投影)。
pub const REPORT_JSON_FILE: &str = "04_report/report.json";
pub const REPORT_MD_FILE: &str = "04_report/report.md";
pub const REPORT_HTML_FILE: &str = "04_report/report.html";
pub const REPORT_ASSETS_REL_DIR: &str = "04_report/assets";

/// 需要预先建出来的目录(建项目时一次性创建)。
///
/// 只建目录,不建空文件 —— 空文件会让"这步做完了没有"变得难以判断。
pub const DIRS: &[&str] = &[
    SOURCE_DIR,
    PREPROCESS_DIR,
    SHOTS_REL_DIR,
    CALIBRATE_DIR,
    SHOTS_EXTRA_REL_DIR,
    SUMMARY_DIR,
    NOTES_REL_DIR,
    SHOTS_EXTRA2_REL_DIR,
    REPORT_DIR,
    REPORT_ASSETS_REL_DIR,
];

/// 一个项目的路径集合。本身是纯计算,不检查目录是否存在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `projects/<id>`。
    pub fn for_project(projects_root: impl Into<PathBuf>, id: &str) -> Self {
        Self::new(projects_root.into().join(id))
    }

    /// 相对当前工作目录的默认项目根:`projects/<id>`。
    pub fn in_cwd(id: &str) -> Self {
        Self::for_project(PROJECTS_DIR, id)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── 项目根 ───────────────────────────────────────────────────────
    pub fn project_json(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }
    pub fn state_file(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }
    pub fn lock_file(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    // ── source/ ─────────────────────────────────────────────────────
    pub fn source_dir(&self) -> PathBuf {
        self.root.join(SOURCE_DIR)
    }
    pub fn video_meta(&self) -> PathBuf {
        self.rel(VIDEO_META_FILE)
    }

    // ── 01_preprocess/ ──────────────────────────────────────────────
    pub fn preprocess_dir(&self) -> PathBuf {
        self.root.join(PREPROCESS_DIR)
    }
    pub fn change_curve(&self) -> PathBuf {
        self.rel(CHANGE_CURVE_FILE)
    }
    pub fn dense_regions(&self) -> PathBuf {
        self.rel(DENSE_REGIONS_FILE)
    }
    pub fn transcript(&self) -> PathBuf {
        self.rel(TRANSCRIPT_FILE)
    }
    pub fn shots_dir(&self) -> PathBuf {
        self.rel(SHOTS_REL_DIR)
    }
    pub fn shots_json(&self) -> PathBuf {
        self.rel(SHOTS_FILE)
    }
    pub fn preprocess_timeline(&self) -> PathBuf {
        self.rel(PREPROCESS_TIMELINE_FILE)
    }
    /// 步 1 的输入指纹(增量重跑的比较对象)。
    pub fn preprocess_inputs(&self) -> PathBuf {
        self.rel(PREPROCESS_INPUTS_FILE)
    }

    // ── 02_calibrate/ ───────────────────────────────────────────────
    pub fn calibrate_dir(&self) -> PathBuf {
        self.root.join(CALIBRATE_DIR)
    }
    pub fn transcript_fixed(&self) -> PathBuf {
        self.rel(TRANSCRIPT_FIXED_FILE)
    }
    pub fn transcript_diff(&self) -> PathBuf {
        self.rel(TRANSCRIPT_DIFF_FILE)
    }
    pub fn supplement_requests(&self) -> PathBuf {
        self.rel(SUPPLEMENT_REQUESTS_FILE)
    }
    pub fn cross_validate(&self) -> PathBuf {
        self.rel(CROSS_VALIDATE_FILE)
    }
    pub fn calibration_log(&self) -> PathBuf {
        self.rel(CALIBRATION_LOG_FILE)
    }
    pub fn shots_extra_dir(&self) -> PathBuf {
        self.rel(SHOTS_EXTRA_REL_DIR)
    }
    pub fn calibrate_timeline(&self) -> PathBuf {
        self.rel(CALIBRATE_TIMELINE_FILE)
    }
    pub fn calibrate_table(&self) -> PathBuf {
        self.rel(CALIBRATE_TABLE_FILE)
    }

    // ── 03_summary/ ─────────────────────────────────────────────────
    pub fn summary_dir(&self) -> PathBuf {
        self.root.join(SUMMARY_DIR)
    }
    pub fn agent_log(&self) -> PathBuf {
        self.rel(AGENT_LOG_FILE)
    }
    pub fn notes_dir(&self) -> PathBuf {
        self.rel(NOTES_REL_DIR)
    }
    pub fn summary_md(&self) -> PathBuf {
        self.rel(SUMMARY_FILE)
    }
    /// 分段的落点(步 3 的结构化产物;报告页从它组装)。
    pub fn summary_sections(&self) -> PathBuf {
        self.rel(SUMMARY_SECTIONS_FILE)
    }
    pub fn shots_extra2_dir(&self) -> PathBuf {
        self.rel(SHOTS_EXTRA2_REL_DIR)
    }

    // ── 04_report/ ──────────────────────────────────────────────────
    pub fn report_dir(&self) -> PathBuf {
        self.root.join(REPORT_DIR)
    }
    /// 报告**数据**文件 —— 渲染器的输入。`report.md` / `report.html` 是它的投影。
    pub fn report_json(&self) -> PathBuf {
        self.rel(REPORT_JSON_FILE)
    }
    pub fn report_md(&self) -> PathBuf {
        self.rel(REPORT_MD_FILE)
    }
    pub fn report_html(&self) -> PathBuf {
        self.rel(REPORT_HTML_FILE)
    }
    pub fn report_assets_dir(&self) -> PathBuf {
        self.rel(REPORT_ASSETS_REL_DIR)
    }

    /// 项目根相对路径 → 绝对路径。
    pub fn rel(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// `project.json` 是否存在(判断"这是不是一个项目"的最低标准)。
    pub fn exists(&self) -> bool {
        self.project_json().is_file()
    }

    /// 建出全部目录(幂等)。已存在不算错。
    pub fn create_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("创建项目根目录失败:{}", self.root.display()))?;
        for dir in DIRS {
            let path = self.rel(dir);
            fs::create_dir_all(&path)
                .with_context(|| format!("创建目录失败:{}", path.display()))?;
        }
        Ok(())
    }

    /// 目录骨架里应存在的相对目录,供测试与 `status` 使用。
    pub fn rel_dirs(&self) -> Vec<(&'static str, PathBuf)> {
        DIRS.iter().map(|d| (*d, self.rel(d))).collect()
    }
}

/// 一个项目的**身份**:同一个项目无论用哪种写法指过来,都该得到同一个值。
///
/// 拿它当进程内 map 的键。为什么不能用调用方给的路径原样:
/// `projects/e2e`、`./projects/e2e`、`E:\…\projects\e2e`、`PROJECTS/E2E`
/// 在 Windows 上是**同一个目录**,但作为字符串是四个不同的值 ——
/// 拿原样当键,"同一个项目"在内存里就成了四个,轻则重复登记,重则**自己把自己锁在门外**
/// (第二个键没命中缓存,真去加锁,而 OS 锁是同进程跨 handle 也生效的)。
///
/// 两步都不能省:
/// 1. `canonicalize` 解析 `.` / `..` / 相对路径 / 软链接,并在 Windows 上取回**磁盘上的真实大小写**;
/// 2. [`simplify_path`](crate::source::simplify_path) 剥掉它顺手加上的 `\\?\` 前缀 ——
///    否则 `\\?\E:\a` 与 `E:\a` 又成了两个值,等于白做。
///
/// 目录还不存在时(建项目**之前**)`canonicalize` 会失败,那时退回原样:
/// 反正这一刻也还没有别人认得这个项目。
pub fn identity(dir: &Path) -> PathBuf {
    match dir.canonicalize() {
        Ok(real) => crate::source::simplify_path(&real),
        Err(_) => dir.to_path_buf(),
    }
}

impl Layout {
    /// 这个项目的身份。见 [`identity`]。
    pub fn identity(&self) -> PathBuf {
        identity(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relpath::RelPath;

    #[test]
    fn layout_paths_are_under_root() {
        let l = Layout::in_cwd("demo");
        assert_eq!(l.root(), Path::new("projects/demo"));
        assert_eq!(l.project_json(), Path::new("projects/demo/project.json"));
        assert_eq!(
            l.shots_json(),
            Path::new("projects/demo/01_preprocess/shots.json")
        );
        assert_eq!(
            l.report_assets_dir(),
            Path::new("projects/demo/04_report/assets")
        );
    }

    #[test]
    fn all_dir_constants_are_valid_relpaths() {
        for d in DIRS {
            assert!(RelPath::new(*d).is_ok(), "目录常量不是合法相对路径:{d}");
        }
        for f in [
            VIDEO_META_FILE,
            CHANGE_CURVE_FILE,
            DENSE_REGIONS_FILE,
            TRANSCRIPT_FILE,
            SHOTS_FILE,
            PREPROCESS_TIMELINE_FILE,
            PREPROCESS_INPUTS_FILE,
            TRANSCRIPT_FIXED_FILE,
            TRANSCRIPT_DIFF_FILE,
            SUPPLEMENT_REQUESTS_FILE,
            CROSS_VALIDATE_FILE,
            CALIBRATION_LOG_FILE,
            CALIBRATE_TIMELINE_FILE,
            CALIBRATE_TABLE_FILE,
            AGENT_LOG_FILE,
            SUMMARY_FILE,
            REPORT_MD_FILE,
            REPORT_HTML_FILE,
        ] {
            assert!(RelPath::new(f).is_ok(), "文件常量不是合法相对路径:{f}");
        }
    }

    #[test]
    fn create_dirs_is_idempotent() {
        let root = std::env::temp_dir().join(format!("vidbrief-layout-{}", std::process::id()));
        let l = Layout::new(&root);
        l.create_dirs().unwrap();
        l.create_dirs().unwrap();
        for (name, path) in l.rel_dirs() {
            assert!(path.is_dir(), "目录未建出:{name}");
        }
        assert!(!l.exists(), "还没写 project.json,不应算已存在项目");
        fs::remove_dir_all(&root).unwrap();
    }
}
