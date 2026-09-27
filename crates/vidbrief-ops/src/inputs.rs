//! 步 1 的**输入指纹**:增量重跑靠它判断「这一步要不要重算」。
//!
//! # 它解决什么
//!
//! 从前 `preprocess` 一律重算,而且重算就作废成果:画面重算 → 看图结论全清
//! (`state.shots.clear()`),转录重跑 → 核验戳全清(`state.segments.clear()`)——
//! `PreprocessReport` 里 `shot_checks_cleared` / `segment_checks_cleared` 报的就是这个数。
//! 而校准是**项目里最贵的活**(模型逐张看图、逐段核验是几十分钟的事),
//! 任何一次重跑 —— 手滑、改参数试一下、中途失败重来 —— 都把它归零。
//!
//! # 两半分开算
//!
//! 画面与转录的输入根本不同:
//!
//! | 那半 | 输入 |
//! |------|------|
//! | 画面 | 源视频 + `fps` + `width` + 抽帧分析算法的口径版本 |
//! | 转录 | 源视频 + whisper 模型 + 语言 + 转录的口径版本 |
//!
//! 所以「改个 `fps` 试一下」只该重算画面那一半:转录跟抽帧参数毫无关系,
//! 它那几分钟不该跟着白跑,那批核验戳更不该跟着作废。
//!
//! # 源视频指纹**每半各带一份**
//!
//! 不放在顶层合成一个。差别在这条路上:「源视频换了,而这趟只跑转录
//! (`--no-shots`)」—— 那时画面那半的成果还是**旧视频**跑出来的,它得留住旧的那份
//! 指纹;顶层一个共用字段表达不了这件事,下次就会拿「新视频的指纹 + 旧截图」
//! 当成一套,把旧帧当新帧复用。
//!
//! # 这份文件是**记录**,不是判据
//!
//! 判据永远是**当场算出来的**那一份:源视频 sha256 每次重跑都重算,不读
//! `project.json` 里 init 时记的那个 —— 硬链接场景下源文件被就地改,项目里那份
//! 跟着变,只有重算才发现([`super::preprocess`] 也会把这件事报出来)。
//!
//! 这份文件被人手改的后果最多是**多算一次**(保守方向),不会让旧帧冒充新帧 ——
//! 因为它只回答「上次是拿什么跑出来的」,而「上次的东西还在不在、还是不是那一份」
//! 要另外过一遍内容底账(`state.content`,见 [`super::preprocess`] 里的复用判据)。
//!
//! # 判不准就不复用
//!
//! 复用的前提是每一项都**证明**相同。任何一项读不出来(材料不在、读不动、
//! 跟底账对不上、版本对不上),答案都是重算。反过来那条路(「文件看着还在,
//! 那就复用吧」)的代价是拿旧材料配新视频 —— 那是静默出错,而重算只是多花几分钟。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use vidbrief_project::Layout;

/// 这份记录的版本。加字段就 +1(旧版本按「没有记录」处理 → 重算,安全方向)。
pub const INPUTS_SCHEMA_VERSION: u32 = 1;

/// 抽帧分析算法的**口径版本**:改了 `vidbrief-core` 那套算法(差帧评分、密集区、
/// 抽帧落点)就 +1。
///
/// 为什么不拿 `CARGO_PKG_VERSION` 顶替:开发期版本号一动不动,而算法天天在改 ——
/// 那会让「改完算法重跑一次」变成「沿用旧帧」。版本号管发布,这个数字管口径。
pub const VISUAL_ALGO_VERSION: u32 = 1;

/// 转录的**口径版本**:改了 whisper 的调用方式(参数、后处理)就 +1。
pub const TRANSCRIBE_ALGO_VERSION: u32 = 1;

/// 跑这一步的是哪个二进制版本(`0.1.0` 这种)。发布升版时它会变 → 那一半不复用。
pub fn tool_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 步 1 的输入指纹。落在 `01_preprocess/inputs.json`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreprocessInputs {
    pub schema_version: u32,
    /// 画面那半的输入;`None` = 这个项目还没有画面成果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual: Option<VisualInputs>,
    /// 转录那半的输入;`None` = 还没有转录成果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptInputs>,
}

/// 画面那半的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualInputs {
    /// 这半成果是**拿哪一份视频**跑出来的。
    pub source_sha256: String,
    /// 采样帧率。
    pub fps: f64,
    /// 参与打分的缩小宽度。
    pub width: u32,
    pub algo: u32,
    pub tool_version: String,
}

/// 转录那半的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptInputs {
    /// 这半成果是**拿哪一份视频**跑出来的。
    pub source_sha256: String,
    /// 模型名(如 `ggml-small`),与 `transcript.json` 里那一栏同一口径。
    pub model: String,
    pub language: String,
    pub algo: u32,
    pub tool_version: String,
}

impl PreprocessInputs {
    /// 读上次的记录。**读不动 = 没有记录**,不是错误。
    ///
    /// 这份文件只决定「能不能省一次重算」,坏掉的代价最多是多算一遍;
    /// 为此让整步 1 停下来是不划算的。版本对不上也按「没有记录」算。
    pub fn load_optional(layout: &Layout) -> Option<Self> {
        let text = std::fs::read_to_string(layout.preprocess_inputs()).ok()?;
        let parsed: Self = serde_json::from_str(&text).ok()?;
        (parsed.schema_version == INPUTS_SCHEMA_VERSION).then_some(parsed)
    }

    /// 记下这一趟的输入(跑完之后叫)。
    pub fn save(&self, layout: &Layout) -> Result<()> {
        let path = layout.preprocess_inputs();
        vidbrief_project::write_atomic(&path, self)
            .with_context(|| format!("写入 {} 失败", path.display()))
    }

    /// 画面那半「现在的输入」长什么样。
    pub fn visual_now(source_sha256: &str, fps: f64, width: u32) -> VisualInputs {
        VisualInputs {
            source_sha256: source_sha256.to_string(),
            fps,
            width,
            algo: VISUAL_ALGO_VERSION,
            tool_version: tool_version(),
        }
    }

    /// 转录那半「现在的输入」长什么样。
    pub fn transcript_now(source_sha256: &str, model: &str, language: &str) -> TranscriptInputs {
        TranscriptInputs {
            source_sha256: source_sha256.to_string(),
            model: model.to_string(),
            language: language.to_string(),
            algo: TRANSCRIBE_ALGO_VERSION,
            tool_version: tool_version(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_layout(tag: &str) -> Layout {
        let root =
            std::env::temp_dir().join(format!("vidbrief-inputs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = Layout::new(&root);
        std::fs::create_dir_all(layout.preprocess_dir()).unwrap();
        layout
    }

    #[test]
    fn inputs_round_trip_through_the_file() {
        let layout = temp_layout("roundtrip");
        let inputs = PreprocessInputs {
            schema_version: INPUTS_SCHEMA_VERSION,
            visual: Some(PreprocessInputs::visual_now(&"a".repeat(64), 1.0, 480)),
            transcript: Some(PreprocessInputs::transcript_now(
                &"a".repeat(64),
                "ggml-small",
                "zh",
            )),
        };
        inputs.save(&layout).unwrap();
        assert_eq!(PreprocessInputs::load_optional(&layout), Some(inputs));
        let _ = std::fs::remove_dir_all(layout.root());
    }

    /// 文件不在、读不动、版本对不上、多了陌生字段 —— 四种都按「没有记录」算
    /// (→ 重算,安全那侧)。
    #[test]
    fn a_broken_record_reads_as_no_record() {
        let layout = temp_layout("broken");
        assert_eq!(PreprocessInputs::load_optional(&layout), None, "文件不在");

        std::fs::write(layout.preprocess_inputs(), "{ 这不是 json").unwrap();
        assert_eq!(PreprocessInputs::load_optional(&layout), None, "读不动");

        let newer = PreprocessInputs {
            schema_version: INPUTS_SCHEMA_VERSION + 1,
            visual: Some(PreprocessInputs::visual_now(&"b".repeat(64), 1.0, 480)),
            transcript: None,
        };
        newer.save(&layout).unwrap();
        assert_eq!(PreprocessInputs::load_optional(&layout), None, "版本对不上");

        std::fs::write(
            layout.preprocess_inputs(),
            r#"{"schema_version":1,"surprise":true}"#,
        )
        .unwrap();
        assert_eq!(PreprocessInputs::load_optional(&layout), None, "有陌生字段");

        let _ = std::fs::remove_dir_all(layout.root());
    }

    /// 两半可以各自单独存在:「只跑过画面」的项目不会凭空长出一条转录记录。
    #[test]
    fn the_two_halves_stand_on_their_own() {
        let visual = PreprocessInputs {
            schema_version: INPUTS_SCHEMA_VERSION,
            visual: Some(PreprocessInputs::visual_now(&"c".repeat(64), 2.0, 320)),
            transcript: None,
        };
        let text = serde_json::to_string(&visual).unwrap();
        assert!(
            !text.contains("transcript"),
            "没跑过的半不该在文件里留个空壳:{text}"
        );

        let transcript = PreprocessInputs {
            schema_version: INPUTS_SCHEMA_VERSION,
            visual: None,
            transcript: Some(PreprocessInputs::transcript_now(
                &"c".repeat(64),
                "ggml-small",
                "zh",
            )),
        };
        let text = serde_json::to_string(&transcript).unwrap();
        assert!(!text.contains("visual"), "{text}");
    }

    /// 源视频指纹**在两半里各存一份** —— 顶层放一个共用字段的话,
    /// 「换了视频、只跑转录」会把画面那半的旧指纹冲掉(见模块文档)。
    #[test]
    fn each_half_carries_its_own_video_fingerprint() {
        let text = serde_json::to_string(&PreprocessInputs::visual_now("ff", 1.0, 480)).unwrap();
        assert!(text.contains("\"source_sha256\":\"ff\""), "{text}");
    }
}
