//! `state.bin` —— 项目**状态**。**非明文。**
//!
//! 设计见 `docs/DESIGN.md` §4.10;来历见 devlog [260925-30] 第 2 条。
//!
//! # 它装什么,不装什么
//!
//! 只装**推不出来的东西**:进度、核验戳、看图结论、转录结论、补图候选的处置、拍板记录。
//! 这些都是"有人做过一个决定",文件里没有别的痕迹能复原它。
//!
//! 反过来,凡是能从别处推出来的都不进来:报告在不在看文件在不在、转录原文在
//! `transcript.json` 里、图是哪一帧在 `shots.json` 里。
//!
//! # 为什么不做成明文
//!
//! 模型手里有宿主给的文件工具,而这道门的判据(谁审过、哪张图看过、哪一步拍过板)
//! 全在文件里。第五轮联调实测:两样被直接改过,其中 `report.at` 被编成了**未来时刻**
//! (devlog [260925-30])。明文状态 = 门是纸糊的。
//!
//! 所以状态写成二进制,**文件头放格式版本与整段内容的 sha256**。
//!
//! **不加密** —— 加密要密钥,而密钥无论放项目里还是放工具里模型都读得到
//! (工具就在它手上跑),挡不住执意作弊,却多出一个"密钥丢了状态读不回来"的失败模式。
//! 这里要的不是保密,是"**改不动、坏了看得出来**":手写改不动这一坨二进制;
//! 改了校验和对不上,工具当场报错,而不是把改过的内容当成真的接着往下走。
//!
//! # 读不动 ≠ 没有
//!
//! [`ProjectState::read_file`] 是三态:**不在** → `Ok(None)`(新项目,合法);
//! **在、读得动** → `Ok(Some)`;**在、读不动** → `Err`(报错,并且门要按最保守的那头算)。
//!
//! 这一条是第五轮那次事故的全部差别所在:当时 `.ok()` 把一份读坏的候选清单当成了
//! "没有候选",门数出 0 条、一路放行。**把"看不懂"和"没有"混成一件事,门就瞎了。**
//! 对状态文件更要命:状态读不动意味着"**不知道谁审过什么**" —— 那只能按"都还没审"算。
//!
//! # 明文文件退回"投影"
//!
//! `shots.json` / `transcript.fixed.json` / `supplement_requests.json` 里那几栏状态
//! **只是上一次写出去的投影**,给人读、给视图画。工具**不把它们读回来当判据**:
//! 读的是这里。两者对不上时由 [`crate::state`] 之外的体检去报告(见 `vidbrief status`),
//! 但**门不看投影** —— 手改投影骗不过门,只会被体检记一笔。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::ShotId;
use crate::project::{Progress, Review};
use crate::shots::{ShotCheck, ShotStatus, Shots};
use crate::supplement::{SupplementRequest, SupplementRequests, SupplementStatus};
use crate::transcript::{Conclusion, CorrectionType, Segment, Transcript};

/// 文件开头的固定标识。**不是本工具写的,一眼就能认出来。**
pub const MAGIC: &[u8; 9] = b"VDBFSTATE";

/// 载荷格式版本。字段增删改就 +1,并在 [`unframe`] 里报"这是哪一版写的"。
///
/// - v1 → v2:加 [`ProjectState::content`](材料内容底账)。
///   加字段就得 +1:bincode 是**位置编码**,老文件按新结构解会当场报"内容解不开" ——
///   而"版本不对"是一句能照做的话(跑 `vidbrief upgrade`),"内容解不开"不是。
pub const FORMAT_VERSION: u32 = 2;

/// 文件头长度:magic + 版本(u32)+ 长度(u64)+ sha256(32 字节)。
pub const HEADER_LEN: usize = MAGIC.len() + 4 + 8 + 32;

// ── 文件格式 ─────────────────────────────────────────────────────────

/// 文件头读不动的原因。**分开说**,因为读的人要采取的行动不一样:
/// 版本不对是"换个工具",校验和对不上是"这份文件被人动过"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFormatError {
    /// 比文件头还短 —— 被截断了。
    TooShort,
    /// 开头那串标识不对:这不是本工具写的状态文件。
    NotOurs,
    /// 版本比本工具认的新(或旧)。
    Version { found: u32, ours: u32 },
    /// 头里写的长度与文件实际长度对不上。
    Length { declared: u64, actual: usize },
    /// 内容与头里的 sha256 对不上 —— **被改过**。
    Digest,
}

impl std::fmt::Display for StateFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "文件短于文件头 —— 它被截断了"),
            Self::NotOurs => write!(f, "开头不是本工具的状态标识 —— 这不是一份状态文件"),
            Self::Version { found, ours } => write!(
                f,
                "这是第 {found} 版的状态文件,本工具认的是第 {ours} 版"
            ),
            Self::Length { declared, actual } => write!(
                f,
                "头里写着 {declared} 字节,实际有 {actual} 字节 —— 长度对不上"
            ),
            Self::Digest => write!(f, "内容与文件头里的校验和对不上 —— 它被改过"),
        }
    }
}

/// 把载荷包成落盘的那一串字节。
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&Sha256::digest(payload));
    out.extend_from_slice(payload);
    out
}

/// 拆掉文件头,**逐项检查**。
pub fn unframe(bytes: &[u8]) -> std::result::Result<&[u8], StateFormatError> {
    if bytes.len() < HEADER_LEN {
        return Err(StateFormatError::TooShort);
    }
    if bytes.get(..MAGIC.len()) != Some(MAGIC.as_slice()) {
        return Err(StateFormatError::NotOurs);
    }
    // 下面几处按下标取的都是**定长 ASCII 区**,不存在中文那种字符边界问题
    let version = u32::from_le_bytes(
        bytes[MAGIC.len()..MAGIC.len() + 4]
            .try_into()
            .expect("4 字节"),
    );
    if version != FORMAT_VERSION {
        return Err(StateFormatError::Version {
            found: version,
            ours: FORMAT_VERSION,
        });
    }
    let declared = u64::from_le_bytes(
        bytes[MAGIC.len() + 4..MAGIC.len() + 12]
            .try_into()
            .expect("8 字节"),
    );
    let payload = &bytes[HEADER_LEN..];
    if declared != payload.len() as u64 {
        return Err(StateFormatError::Length {
            declared,
            actual: payload.len(),
        });
    }
    let want = &bytes[MAGIC.len() + 12..HEADER_LEN];
    if Sha256::digest(payload).as_slice() != want {
        return Err(StateFormatError::Digest);
    }
    Ok(payload)
}

// ── 状态本身 ─────────────────────────────────────────────────────────

/// 一张截图的状态。**内容那一半**(时刻、文件名、来源)在 `shots.json` 里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShotState {
    pub status: ShotStatus,
    /// 补图理由(补的时候写的)。
    pub reason: Option<String>,
    /// 看图结论。
    pub check: Option<ShotCheck>,
}

impl Default for ShotState {
    fn default() -> Self {
        Self {
            status: ShotStatus::Keep,
            reason: None,
            check: None,
        }
    }
}

impl ShotState {
    /// 什么都没说 —— 这种条目不必存。
    pub fn is_empty(&self) -> bool {
        self.status == ShotStatus::Keep && self.reason.is_none() && self.check.is_none()
    }
}

impl From<&crate::shots::Shot> for ShotState {
    fn from(s: &crate::shots::Shot) -> Self {
        Self {
            status: s.status,
            reason: s.reason.clone(),
            check: s.check.clone(),
        }
    }
}

/// 一段转录的状态。
///
/// **结论那一栏的落盘表示与 JSON 那边不是同一个枚举**(见 [`StoredConclusion`]),
/// 所以它不公开、只留 `conclusion()` / `set_conclusion()` 两个口子 ——
/// 外面看到的永远是 [`Conclusion`] 本身。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SegmentState {
    conclusion: Option<StoredConclusion>,
    /// 模型**核验过这一段**的时刻。`None` = 还没核验。
    pub checked_at: Option<DateTime<FixedOffset>>,
}

impl SegmentState {
    pub fn conclusion(&self) -> Option<Conclusion> {
        self.conclusion.as_ref().map(Conclusion::from)
    }

    pub fn set_conclusion(&mut self, conclusion: Option<Conclusion>) {
        self.conclusion = conclusion.as_ref().map(StoredConclusion::from);
    }

    pub fn is_empty(&self) -> bool {
        self.conclusion.is_none() && self.checked_at.is_none()
    }
}

impl From<&Segment> for SegmentState {
    fn from(s: &Segment) -> Self {
        Self {
            conclusion: s.conclusion.as_ref().map(StoredConclusion::from),
            checked_at: s.checked_at,
        }
    }
}

/// 结论的**落盘表示** —— 与 [`Conclusion`] 等价,换一个形状而已。
///
/// 为什么要换:`Conclusion` 用的是**内部标签**(`{"outcome":"corrected", ...}`),
/// 那种形状要求序列化器支持 `deserialize_any`(它得先读完所有字段再决定是哪个变体),
/// 而 bincode 是位置编码,不支持。外边标签的枚举它认。
///
/// 换的是编码,不是事实:两种表示一一对应,`From` 两个方向都有测试钉着。
/// 也**只在这里换** —— JSON 那边的契约一个字节都不动。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum StoredConclusion {
    Corrected {
        text: String,
        kind: CorrectionType,
        note: String,
        reason: String,
    },
    Kept {
        note: String,
        reason: String,
    },
    Uncertain {
        note: String,
        reason: String,
    },
}

impl From<&Conclusion> for StoredConclusion {
    fn from(c: &Conclusion) -> Self {
        match c {
            Conclusion::Corrected {
                text,
                kind,
                note,
                reason,
            } => Self::Corrected {
                text: text.clone(),
                kind: *kind,
                note: note.clone(),
                reason: reason.clone(),
            },
            Conclusion::Kept { note, reason } => Self::Kept {
                note: note.clone(),
                reason: reason.clone(),
            },
            Conclusion::Uncertain { note, reason } => Self::Uncertain {
                note: note.clone(),
                reason: reason.clone(),
            },
        }
    }
}

impl From<&StoredConclusion> for Conclusion {
    fn from(c: &StoredConclusion) -> Self {
        match c {
            StoredConclusion::Corrected {
                text,
                kind,
                note,
                reason,
            } => Self::Corrected {
                text: text.clone(),
                kind: *kind,
                note: note.clone(),
                reason: reason.clone(),
            },
            StoredConclusion::Kept { note, reason } => Self::Kept {
                note: note.clone(),
                reason: reason.clone(),
            },
            StoredConclusion::Uncertain { note, reason } => Self::Uncertain {
                note: note.clone(),
                reason: reason.clone(),
            },
        }
    }
}

/// 一条补图候选的状态。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SupplementState {
    pub status: SupplementStatus,
    pub shot_id: Option<ShotId>,
    pub note: Option<String>,
}

impl Default for SupplementStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl SupplementState {
    pub fn is_empty(&self) -> bool {
        self.status == SupplementStatus::Pending && self.shot_id.is_none() && self.note.is_none()
    }
}

impl From<&SupplementRequest> for SupplementState {
    fn from(r: &SupplementRequest) -> Self {
        Self {
            status: r.status,
            shot_id: r.shot_id.clone(),
            note: r.note.clone(),
        }
    }
}

/// 一份材料**内容**的底账。
///
/// # 为什么要有它
///
/// 门的第①条数的"还没核验的有哪些"是**从明文文件里数出来的** ——
/// 文件里没有的行,它当然看不见。于是模型不必审 322 段:
/// 把没审的那些从 `transcript.json` 里拿掉,门就以为活干完了
/// (这条有一个**专门钉它的测试**,见 `vidbrief-ops::review`)。
///
/// 用户当时问的是「门控不放过,它改了不还得改回去吗」—— **改**是这样,
/// **拿掉**不是:改多了得改回去,而删掉了行,门反而看不见那部分活。
///
/// 所以状态里记一份**内容**的底账:工具每次写这份材料都记一遍,
/// 门开口之前先对一遍,对不上就不放行(去路见门口那句话)。
///
/// # 为什么按内容算,不按文件字节算
///
/// - 文件里那几栏**状态**(`check` / `status` / `conclusion`…)本来就允许人手改 ——
///   它们是投影,门不看它们;拿它们当"材料变了"会误伤。
/// - 格式(缩进、键序、换行)变了也不该算材料变了:那是同一份内容换个写法。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPrint {
    /// 底账记的是**几条**(段 / 张 / 条)。报错时因此说得清"原来几条,现在几条"。
    pub items: usize,
    /// 规范化之后的内容摘要(整条 sha256)。
    pub digest: String,
}

/// 项目状态。**只装推不出来的东西。**
///
/// 字段顺序即 bincode 的落盘顺序 —— 往后加字段只能加在**末尾**,改结构就要
/// [`FORMAT_VERSION`] +1(见模块文档)。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProjectState {
    /// 乐观并发修订号。工具每成功写一次 +1。
    pub rev: u64,
    /// 四步进度。
    pub progress: Progress,
    /// 用户对各步的确认 —— 审核门。
    pub review: Review,
    /// 看图状态,键是 `sNNN`。
    pub shots: BTreeMap<ShotId, ShotState>,
    /// 转录结论,键是段 id。
    pub segments: BTreeMap<u32, SegmentState>,
    /// 补图候选的处置,键是候选号。
    pub supplements: BTreeMap<u32, SupplementState>,
    /// **材料内容的底账**:项目根相对路径 → 工具最后一次写出去时它长什么样。
    ///
    /// 键是 `layout` 里那几个常量(`01_preprocess/transcript.json` / `…/shots.json` /
    /// `02_calibrate/supplement_requests.json`),外加 `03_summary/summary.md`。
    ///
    /// **为什么是这三份材料**:门的第①条要**数行数**("还有哪些没核验"),数的就是这三份。
    /// 别的材料(差帧图、密集区、元数据)没有一条规则去数它们里面的行 ——
    /// 给它们也记一份,只是多一份要维护、会对不上的东西。
    ///
    /// **`summary.md` 为什么也进来**:步 3→4 那道门问的是另一个问题 ——
    /// "出报告之前,那份总结还是工具写出去的那一份吗"。判据同为内容指纹,
    /// 机制同一套,所以走同一张表(见 [`doc_print`] 与 `review::gate_summarize`)。
    #[serde(default)]
    pub content: BTreeMap<String, ContentPrint>,
}

impl ProjectState {
    pub fn new() -> Self {
        Self::default()
    }

    // ── 读写 ────────────────────────────────────────────────────────

    /// 从磁盘读。**三态**,见模块文档。
    pub fn read_file(path: &Path) -> Result<Option<Self>> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("读取状态文件失败:{}", path.display()));
            }
        };
        let payload = unframe(&bytes).map_err(|why| anyhow!("{}", broken_message(path, &why)))?;
        let state: Self = bincode::deserialize(payload)
            .map_err(|e| anyhow!("{}", broken_message(path, &format!("内容解不开:{e}"))))?;
        Ok(Some(state))
    }

    /// 写盘,并把 `rev` +1。**全项目只此一处修改 `rev`。**
    pub fn save(&mut self, path: &Path) -> Result<()> {
        self.rev += 1;
        self.write(path)
    }

    /// 写盘但**不动 `rev`** —— 给"照材料重新起一份状态"用,那不是一次内容修改。
    pub fn write(&self, path: &Path) -> Result<()> {
        let payload = self.to_payload()?;
        let bytes = frame(&payload);
        crate::json::write_bytes_atomic(path, &bytes)
            .with_context(|| format!("写入状态文件失败:{}", path.display()))
    }

    /// 把载荷原样取出来 —— 给测试与"看一眼里面是什么"的排查用。
    pub fn to_payload(&self) -> Result<Vec<u8>> {
        bincode::serialize(self).context("状态序列化失败")
    }

    /// 从载荷解回来。
    pub fn from_payload(payload: &[u8]) -> Result<Self> {
        bincode::deserialize(payload).context("状态反序列化失败")
    }

    // ── 条目访问 ─────────────────────────────────────────────────────

    /// 拿到某张图的状态条目(没有就现建一条默认的)。
    pub fn shot_mut(&mut self, id: ShotId) -> &mut ShotState {
        self.shots.entry(id).or_default()
    }

    pub fn shot(&self, id: ShotId) -> Option<&ShotState> {
        self.shots.get(&id)
    }

    pub fn segment_mut(&mut self, id: u32) -> &mut SegmentState {
        self.segments.entry(id).or_default()
    }

    pub fn segment(&self, id: u32) -> Option<&SegmentState> {
        self.segments.get(&id)
    }

    pub fn supplement_mut(&mut self, id: u32) -> &mut SupplementState {
        self.supplements.entry(id).or_default()
    }

    pub fn supplement(&self, id: u32) -> Option<&SupplementState> {
        self.supplements.get(&id)
    }

    /// 供乐观并发使用:调用方读到的 `rev` 是否仍是当前值。
    ///
    /// 修订号从 `project.json` 搬到这里 —— 它记的是"这份状态被改过几次",
    /// 那也是状态,不是身份(见 [`crate::project`] 的模块文档)。
    /// **只把 `rev` 读出来** —— 乐观并发的检查点用(`OpenProject::save`)。
    ///
    /// 状态文件不在时给 0(新项目);读不动时**报错**,而不是猜一个数:
    /// 猜出来的基准会让"有没有被别人改过"这个问题永远答"没有"。
    pub fn read_rev(path: &Path) -> Result<u64> {
        match Self::read_file(path)? {
            Some(state) => Ok(state.rev),
            None => Ok(0),
        }
    }

    pub fn check_rev(&self, seen: u64) -> Result<()> {
        if self.rev != seen {
            bail!(
                "项目状态已被其它会话修改(期望 rev={seen},实际 rev={})",
                self.rev
            );
        }
        Ok(())
    }

    /// 丢掉"什么都没说"的条目。
    ///
    /// 为什么不留着默认条目:状态要回答的是"**有人做过什么决定**",
    /// 一个 322 段的空条目表会把"什么都没决定"和"决定过 322 次"混成一样大。
    pub fn prune(&mut self) {
        self.shots.retain(|_, v| !v.is_empty());
        self.segments.retain(|_, v| !v.is_empty());
        self.supplements.retain(|_, v| !v.is_empty());
        // 底账不清:它是"工具写过这份材料"的记录,跟有没有结论无关
    }

    /// 记下这份材料**内容**的底账(键是项目根相对路径)。
    pub fn note_content(&mut self, rel: &str, print: ContentPrint) {
        self.content.insert(rel.to_string(), print);
    }

    /// 一份材料的内容跟底账对不上时的说法(对得上 → `None`)。
    ///
    /// `now` 是这份材料**当前**的内容指纹;`None` = 文件不在了。
    pub fn content_stale(&self, rel: &str, now: Option<ContentPrint>) -> Option<String> {
        let Some(then) = self.content.get(rel) else {
            return match now {
                // 有材料、没底账:这份材料不是走过工具写进来的
                Some(p) => Some(format!(
                    "{}:没有底账(工具没写过它,或者它是被手放进去的)—— 现在是 {} 条",
                    rel, p.items
                )),
                None => None,
            };
        };
        match now {
            None => Some(format!(
                "{}:工具写出去的那次是 {} 条,现在这份**不在了**",
                rel, then.items
            )),
            Some(p) if p.digest != then.digest => Some(format!(
                "{}:内容跟工具写出去的那次对不上(那次 {} 条,现在 {} 条)",
                rel, then.items, p.items
            )),
            Some(_) => None,
        }
    }

    // ── 合并 / 拆分:明文材料与状态 ──────────────────────────────────

    /// 把状态盖到一份截图清单上(键对不上就按状态里的默认值算)。
    ///
    /// **内容以清单为准,状态以这里为准** —— 清单里那几栏只是投影,可能是手改的。
    pub fn apply_to_shots(&self, shots: &mut Shots) {
        for shot in &mut shots.shots {
            let Some(state) = self.shots.get(&shot.id) else {
                shot.status = ShotStatus::Keep;
                shot.reason = None;
                shot.check = None;
                continue;
            };
            shot.status = state.status;
            shot.reason = state.reason.clone();
            shot.check = state.check.clone();
        }
    }

    /// 反过来:从一份(合并过或没合并过的)清单里收取状态。
    ///
    /// **不动清单里没有对应图的条目** —— 那种条目只会出现在"有人把图从清单里拿掉了"
    /// 的时候,留着它才看得出来([`Self::orphans`] 会报)。
    pub fn absorb_shots(&mut self, shots: &Shots) {
        for shot in &shots.shots {
            let state = ShotState::from(shot);
            if state.is_empty() {
                self.shots.remove(&shot.id);
            } else {
                self.shots.insert(shot.id, state);
            }
        }
    }

    /// 把状态盖到一份转录上(结论 + 核验戳)。
    pub fn apply_to_transcript(&self, transcript: &mut Transcript) {
        for seg in &mut transcript.segments {
            match self.segments.get(&seg.id) {
                Some(state) => {
                    seg.conclusion = state.conclusion();
                    seg.checked_at = state.checked_at;
                }
                None => {
                    seg.conclusion = None;
                    seg.checked_at = None;
                }
            }
        }
    }

    /// 从一份转录里收取结论。**没结论、没核验戳的段不进状态**。
    pub fn absorb_transcript(&mut self, transcript: &Transcript) {
        for seg in &transcript.segments {
            let state = SegmentState::from(seg);
            if state.is_empty() {
                self.segments.remove(&seg.id);
            } else {
                self.segments.insert(seg.id, state);
            }
        }
    }

    /// 把状态盖到一份补图候选清单上。
    pub fn apply_to_supplements(&self, list: &mut SupplementRequests) {
        for req in &mut list.requests {
            match self.supplements.get(&req.id) {
                Some(state) => {
                    req.status = state.status;
                    req.shot_id = state.shot_id.clone();
                    req.note = state.note.clone();
                }
                None => {
                    req.status = SupplementStatus::Pending;
                    req.shot_id = None;
                    req.note = None;
                }
            }
        }
    }

    /// 从一份候选清单里收取处置结论。
    pub fn absorb_supplements(&mut self, list: &SupplementRequests) {
        for req in &list.requests {
            let state = SupplementState::from(req);
            if state.is_empty() {
                self.supplements.remove(&req.id);
            } else {
                self.supplements.insert(req.id, state);
            }
        }
    }

    // ── 体检:状态与投影对不上的地方 ────────────────────────────────

    /// 明文投影跟状态对不上的地方(人读的一行一条)。
    ///
    /// **只报告,不当判据** —— 门读的是状态,投影改得再花也骗不过它;
    /// 这一栏是给"坏了能发现"用的:人一看就知道这份材料被动过。
    ///
    /// 两种对不上:
    /// 1. **投影里写着状态里没有的东西**(有人手改,或者状态被重置过);
    /// 2. **状态里有、清单里没有的条目**(有人把图/候选从明文里拿掉了)。
    pub fn mismatches(
        &self,
        shots: Option<&Shots>,
        fixed: Option<&Transcript>,
        supplements: Option<&SupplementRequests>,
    ) -> Vec<String> {
        let mut out = Vec::new();

        if let Some(shots) = shots {
            for shot in &shots.shots {
                let projection = ShotState::from(shot);
                let truth = self.shots.get(&shot.id).cloned().unwrap_or_default();
                if !projection.is_empty() && projection != truth {
                    out.push(format!(
                        "{}:明文里写着「{}」,状态里不是这个",
                        shot.id,
                        describe_shot(&projection)
                    ));
                }
            }
            for (id, state) in &self.shots {
                if !state.is_empty() && shots.get(*id).is_none() {
                    out.push(format!("{id}:状态里有它的记录,可清单里没有这张图"));
                }
            }
        }

        if let Some(fixed) = fixed {
            for seg in &fixed.segments {
                let projection = SegmentState::from(seg);
                let truth = self.segments.get(&seg.id).cloned().unwrap_or_default();
                if !projection.is_empty() && projection != truth {
                    out.push(format!("seg {}:明文里写着结论,状态里不是这个", seg.id));
                }
            }
            for (id, state) in &self.segments {
                if !state.is_empty() && !fixed.segments.iter().any(|s| s.id == *id) {
                    out.push(format!("seg {id}:状态里有它的核验记录,可转录里没有这一段"));
                }
            }
        }

        if let Some(list) = supplements {
            for req in &list.requests {
                let projection = SupplementState::from(req);
                let truth = self.supplements.get(&req.id).cloned().unwrap_or_default();
                if !projection.is_empty() && projection != truth {
                    out.push(format!("#{}:明文里写着处置结论,状态里不是这个", req.id));
                }
            }
            for (id, state) in &self.supplements {
                if !state.is_empty() && !list.requests.iter().any(|r| r.id == *id) {
                    out.push(format!("#{id}:状态里有它的处置,可清单里没有这条候选"));
                }
            }
        }

        out
    }
}

// ── 内容指纹:工具写出去时那份材料长什么样 ──────────────────────────

/// 转录**内容**的指纹:段号 / 区间 / 正文,加头部的时长与模型 —— 不含任何结论。
pub fn transcript_print(t: &Transcript) -> ContentPrint {
    let mut parts: Vec<String> = vec![
        t.model.clone(),
        t.language.clone(),
        format!("{:?}", t.duration),
        t.segments.len().to_string(),
    ];
    for seg in &t.segments {
        parts.push(seg.id.to_string());
        parts.push(format!("{:?}", seg.start));
        parts.push(format!("{:?}", seg.end));
        parts.push(seg.text.clone());
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    ContentPrint {
        items: t.segments.len(),
        digest: crate::digest::full(&refs),
    }
}

/// 截图清单**内容**的指纹:哪一张、哪一刻、存在哪个文件、从哪来 —— 不含结论与留用状态。
pub fn shots_print(shots: &Shots) -> ContentPrint {
    let mut parts: Vec<String> = vec![shots.shots.len().to_string()];
    for shot in &shots.shots {
        parts.push(shot.id.to_string());
        parts.push(format!("{:?}", shot.t));
        parts.push(shot.file.as_str().to_string());
        parts.push(format!("{:?}", shot.origin));
        parts.push(
            shot.region_id
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        parts.push(shot.created_at.to_rfc3339());
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    ContentPrint {
        items: shots.shots.len(),
        digest: crate::digest::full(&refs),
    }
}

/// 补图候选**内容**的指纹:候选号 / 时刻 / 指向哪一段 / 那段原文 / 为什么被列出来 ——
/// 不含处置结论。
pub fn supplements_print(list: &SupplementRequests) -> ContentPrint {
    let mut parts: Vec<String> = vec![
        format!("{:?}", list.window),
        list.requests.len().to_string(),
    ];
    for req in &list.requests {
        parts.push(req.id.to_string());
        parts.push(format!("{:?}", req.t));
        parts.push(req.segment_id.to_string());
        parts.push(req.text.clone());
        parts.push(format!("{:?}", req.trigger));
        parts.push(
            req.nearest_shot
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        parts.push(format!("{:?}", req.gap));
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    ContentPrint {
        items: list.requests.len(),
        digest: crate::digest::full(&refs),
    }
}

/// 一份**文档**(Markdown)的指纹:正文本身,行数当条数。
///
/// 用途只有一个:`summary.md` 守在**步 3→4 那道门**上 —— 门的判据是"出报告之前,
/// 那份总结还是不是工具写出去的那一份"(`review::gate_summarize`)。
///
/// 它与上面三个**同一套机制、同一张表**(`ProjectState::content`),
/// 只是数的东西不同:那三份数的是"有几行活",这份数的是"这版总结被换过没有"。
/// 接进来的理由与它们一样:门要拿明文当判据,而明文谁都能改。
pub fn doc_print(md: &str) -> ContentPrint {
    // 行尾统一:同一个内容换个换行符不算换了一版(与三份材料"换个缩进不算变"一致)
    let normalized: Vec<&str> = md.lines().map(str::trim_end).collect();
    ContentPrint {
        items: normalized.len(),
        digest: crate::digest::full(&normalized),
    }
}

/// 一条截图状态的人话说法(给体检那一行用)。
fn describe_shot(state: &ShotState) -> String {
    let mut parts = Vec::new();
    if state.status == ShotStatus::Drop {
        parts.push("判了弃用".to_string());
    }
    if let Some(check) = &state.check {
        parts.push(format!("看过图({})", check.type_read.trim()));
    }
    if parts.is_empty() {
        parts.push("有条目".to_string());
    }
    parts.join("、")
}

/// 状态文件读不动时给调用方看的那段话。
///
/// **两个去路都要写全**(devlog [260925-31] 第 4 条:没有出路的规则不许进门的话术),
/// 而且不许出现"删" —— 读这句话的常常是模型(`AGENTS.md` §2)。
fn broken_message(path: &Path, why: &dyn std::fmt::Display) -> String {
    format!(
        "状态文件读不动:{}\n\
         · {why}\n\
         \n\
         这份文件里装的是**谁审过、哪张图看过、哪一步拍过板**这类记录 —— 它读不动,\n\
         就意味着这些都不知道了,只能按「都还没核验」算。材料(转录、截图、候选)都还在。\n\
         \n\
         **两条去路,别自己猜:**\n\
         ① 停下这一轮,把这句话原样交给用户:让他跑一次 `vidbrief upgrade <项目目录>`,\n\
         照材料重新起一份状态(代价是已有的核验记录要重做一遍);\n\
         ② 或者让用户换**写这个项目的那一版工具**打开它(别在这一版里覆盖)。",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shots::{Confidence, Evidence, Shot, ShotOrigin};
    use crate::transcript::{CorrectionType, Segment};

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vidbrief-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn check() -> ShotCheck {
        ShotCheck {
            r#match: true,
            audio_visual_sync: true,
            type_read: "Python 列表 结构图".into(),
            neighbor_text: None,
            issues: vec![],
            confidence: Confidence::Low,
            evidence: Some(Evidence {
                segment: 7,
                quote: "切到主图".into(),
            }),
            checked_at: crate::now(),
        }
    }

    /// 一份**每栏都有内容**的状态 —— 拿它验往返。
    fn full() -> ProjectState {
        let mut s = ProjectState::new();
        s.rev = 4;
        s.progress.preprocess.mark_done();
        s.progress.calibrate.bump();
        s.review.calibrate = Some(crate::project::Approval {
            approved_at: crate::now(),
            counts: crate::project::ReviewCounts {
                unchecked: 0,
                needing_review: 1,
                supplement_pending: 2,
                uncertain: 3,
            },
            digests: Some(crate::project::MaterialDigests {
                segments: [("7".to_string(), "abcd1234".to_string())].into(),
                shots: [("s001".to_string(), "efab5678".to_string())].into(),
                supplements: [("2".to_string(), "0011aabb".to_string())].into(),
            }),
            note: Some("用户说他也不确定,先这样".into()),
        });
        s.shot_mut(ShotId::FIRST).check = Some(check());
        s.shot_mut(ShotId::new(2).unwrap()).status = ShotStatus::Drop;
        s.segment_mut(7).set_conclusion(Some(Conclusion::Corrected {
            text: "Python 列表".into(),
            kind: CorrectionType::Terminology,
            note: "派森 → Python".into(),
            reason: "画面标题".into(),
        }));
        s.segment_mut(7).checked_at = Some(crate::now());
        s.segment_mut(8).checked_at = Some(crate::now());
        s.supplement_mut(2).status = SupplementStatus::Rejected;
        s.supplement_mut(2).note = Some("旁边就有图".into());
        s
    }

    #[test]
    fn everything_survives_a_round_trip() {
        let dir = tmpdir("roundtrip");
        let path = dir.join("state.bin");
        let mut state = full();
        state.save(&path).unwrap();

        let back = ProjectState::read_file(&path).unwrap().expect("该读得回来");
        assert_eq!(back, state, "每一栏都该原样回来");
        // 比**原来那份里存着的**(不要再 `check()` 造一份:那个 `checked_at` 是"现在" ——
        // 跨过一次秒边界,这条测试就会莫名其妙地红。本文件其它地方也踩过这个坑)。
        assert_eq!(
            back.shot(ShotId::FIRST).unwrap().check,
            state.shot(ShotId::FIRST).unwrap().check,
            "看图结论该原样回来"
        );
        assert_eq!(back.review.calibrate.as_ref().unwrap().counts.uncertain, 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// **手改一个字节就该被发现。** 这是"非明文"的全部意义。
    #[test]
    fn a_hand_edited_byte_is_caught() {
        let dir = tmpdir("tamper");
        let path = dir.join("state.bin");
        full().write(&path).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        let err = ProjectState::read_file(&path).unwrap_err().to_string();
        assert!(err.contains("校验和"), "{err}");
        assert!(err.contains("被改过"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 改坏了要报错,而且**要把去路说出来** —— 读这句话的常常是模型。
    #[test]
    fn the_broken_message_names_a_way_out_and_never_says_delete() {
        let dir = tmpdir("message");
        let path = dir.join("state.bin");
        std::fs::write(&path, b"not ours at all").unwrap();

        let err = ProjectState::read_file(&path).unwrap_err().to_string();
        assert!(err.contains("upgrade"), "得说清怎么自救:{err}");
        assert!(!err.contains("删"), "不许出现『删』(AGENTS.md §2):{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn truncation_and_foreign_files_and_future_versions_are_all_caught() {
        let dir = tmpdir("shapes");
        let path = dir.join("state.bin");

        // 比文件头还短
        std::fs::write(&path, b"VDBF").unwrap();
        assert!(ProjectState::read_file(&path).is_err());

        // 开头的标识不对
        std::fs::write(&path, vec![0u8; HEADER_LEN + 4]).unwrap();
        let err = ProjectState::read_file(&path).unwrap_err().to_string();
        assert!(err.contains("状态标识"), "{err}");

        // 来自另一个版本
        let payload = full().to_payload().unwrap();
        let mut bytes = frame(&payload);
        bytes[MAGIC.len()] = 9;
        std::fs::write(&path, &bytes).unwrap();
        let err = ProjectState::read_file(&path).unwrap_err().to_string();
        assert!(err.contains("第 9 版"), "{err}");
        assert!(
            err.contains(&format!("第 {FORMAT_VERSION} 版")),
            "得说清本工具认哪一版:{err}"
        );
        assert!(err.contains("upgrade"), "还得说清怎么自救:{err}");

        // 长度对不上
        let mut bytes = frame(&payload);
        bytes[MAGIC.len() + 4] = 99;
        std::fs::write(&path, &bytes).unwrap();
        let err = ProjectState::read_file(&path).unwrap_err().to_string();
        assert!(err.contains("长度"), "{err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// **上一版格式的状态文件**要报得清:"这是第 1 版,本工具认第 2 版" + 去路。
    ///
    /// 加字段就 +1:bincode 是位置编码,老文件按新结构解会报"内容解不开" ——
    /// 那句话没人能照做;而"版本不对"能。
    #[test]
    fn a_previous_format_version_says_which_version_it_is() {
        let dir = tmpdir("old-format");
        let path = dir.join("state.bin");
        let payload = full().to_payload().unwrap();
        let mut bytes = frame(&payload);
        bytes[MAGIC.len()] = 1; // 第 1 版
        std::fs::write(&path, &bytes).unwrap();

        let err = ProjectState::read_file(&path).unwrap_err().to_string();
        assert!(err.contains("第 1 版"), "{err}");
        assert!(err.contains("第 2 版"), "{err}");
        assert!(err.contains("upgrade"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// **不在**是合法的(新项目);读不动才是错误。两者必须分得开。
    #[test]
    fn a_missing_file_is_none_not_an_error() {
        let dir = tmpdir("missing");
        assert_eq!(ProjectState::read_file(&dir.join("state.bin")).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 进度那几栏的助手函数还在(它们现在是状态的一部分)。
    #[test]
    fn progress_helpers_set_time() {
        let mut p = Progress::default();
        p.preprocess.mark_done();
        assert!(p.preprocess.done);
        assert!(p.preprocess.at.is_some());
        p.calibrate.bump();
        p.calibrate.bump();
        assert_eq!(p.calibrate.rounds, 2);
        assert!(p.calibrate.last_at.is_some());
    }

    /// 修订号跟着状态走:读到的值不是当前值,说明有人先改过了。
    #[test]
    fn check_rev_catches_a_stale_writer() {
        let state = ProjectState {
            rev: 7,
            ..ProjectState::default()
        };
        assert!(state.check_rev(7).is_ok());
        let err = state.check_rev(6).unwrap_err().to_string();
        assert!(err.contains("已被其它会话修改"), "{err}");
        assert!(err.contains("rev=6"), "{err}");
    }

    #[test]
    fn rev_counts_writes() {        let dir = tmpdir("rev");
        let path = dir.join("state.bin");
        let mut state = ProjectState::new();
        assert_eq!(state.rev, 0);
        state.save(&path).unwrap();
        state.save(&path).unwrap();
        assert_eq!(state.rev, 2);
        assert_eq!(ProjectState::read_file(&path).unwrap().unwrap().rev, 2);

        // `write` 是"重新起一份",不该悄悄改修订号
        state.write(&path).unwrap();
        assert_eq!(state.rev, 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 什么都没说的条目**不进状态** —— 否则"没决定过"和"决定过 322 次"一样大。
    #[test]
    fn entries_that_say_nothing_are_not_stored() {        let shots = Shots {
            generated_at: crate::now(),
            shots: vec![Shot {
                id: ShotId::FIRST,
                t: 1.0,
                file: crate::relpath::RelPath::new("01_preprocess/shots/s001_0001.0.jpg").unwrap(),
                origin: ShotOrigin::DenseRegion,
                region_id: None,
                status: ShotStatus::Keep,
                reason: None,
                created_at: crate::now(),
                check: None,
            }],
        };
        let mut state = ProjectState::new();
        state.absorb_shots(&shots);
        assert!(state.shots.is_empty(), "默认条目不该进状态");

        // 有结论的进去,再退回默认又出来
        let mut with_check = shots.clone();
        with_check.shots[0].check = Some(check());
        state.absorb_shots(&with_check);
        assert_eq!(state.shots.len(), 1);
        state.absorb_shots(&shots);
        assert!(state.shots.is_empty());
    }

    /// **拆开再合起来要一模一样**:内容在明文、状态在状态文件,两边都不许丢。
    #[test]
    fn split_then_merge_gives_back_the_same_shots() {
        let shots = Shots {
            generated_at: crate::now(),
            shots: vec![
                Shot {
                    id: ShotId::FIRST,
                    t: 1.0,
                    file: crate::relpath::RelPath::new("01_preprocess/shots/s001_0001.0.jpg")
                        .unwrap(),
                    origin: ShotOrigin::DenseRegion,
                    region_id: None,
                    status: ShotStatus::Keep,
                    reason: None,
                    created_at: crate::now(),
                    check: Some(check()),
                },
                Shot {
                    id: ShotId::new(2).unwrap(),
                    t: 9.0,
                    file: crate::relpath::RelPath::new("02_calibrate/shots_extra/s002_0009.0.jpg")
                        .unwrap(),
                    origin: ShotOrigin::AgentRequest,
                    region_id: None,
                    status: ShotStatus::Drop,
                    reason: Some("他说的那处其实没换画面".into()),
                    created_at: crate::now(),
                    check: None,
                },
            ],
        };

        let mut state = ProjectState::new();
        state.absorb_shots(&shots);

        // 明文那一份:状态栏位清空(它就是"只装内容"的样子)
        let mut plain = shots.clone();
        for shot in &mut plain.shots {
            shot.status = ShotStatus::Keep;
            shot.reason = None;
            shot.check = None;
        }

        // 合起来 = 原来那份
        let mut merged = plain;
        state.apply_to_shots(&mut merged);
        assert_eq!(merged, shots);
    }

    /// 转录与候选同理:状态盖上去、收回来都是一个来回。
    #[test]
    fn transcript_and_supplement_state_round_trip() {
        let mut transcript = Transcript {
            source: crate::relpath::RelPath::new("source/video.mp4").unwrap(),
            model: "fixture".into(),
            language: "zh".into(),
            duration: 60.0,
            generated_at: crate::now(),
            segments: vec![
                Segment {
                    id: 1,
                    start: 0.0,
                    end: 1.0,
                    text: "派森列表".into(),
                    conclusion: Some(Conclusion::Corrected {
                        text: "Python 列表".into(),
                        kind: CorrectionType::Terminology,
                        note: "派森 → Python".into(),
                        reason: "画面标题".into(),
                    }),
                    checked_at: Some(crate::now()),
                },
                Segment {
                    id: 2,
                    start: 1.0,
                    end: 2.0,
                    text: "各位好".into(),
                    conclusion: None,
                    checked_at: Some(crate::now()),
                },
            ],
        };
        let mut state = ProjectState::new();
        state.absorb_transcript(&transcript);
        assert_eq!(state.segments.len(), 2);

        let mut plain = transcript.clone();
        for seg in &mut plain.segments {
            seg.conclusion = None;
            seg.checked_at = None;
        }
        assert!(plain.is_untouched(), "原文那一份不该带任何结论");

        state.apply_to_transcript(&mut plain);
        assert_eq!(plain.stats().corrected, 1);
        assert_eq!(plain.stats().clean, 1);

        // 结论撤掉之后,条目也该跟着走
        transcript.segments[0].conclusion = None;
        transcript.segments[0].checked_at = None;
        state.absorb_transcript(&transcript);
        assert_eq!(state.segments.len(), 1);
        assert!(state.segments.get(&2).unwrap().checked_at.is_some());
    }

    /// 结论的两种表示(JSON 的内部标签 / 落盘的等价形状)**一一对应**。
    ///
    /// 换形状是因为 bincode 是位置编码,解不了内部标签那种"读完才知道是哪个变体"
    /// 的枚举(`deserialize_any`)。换的只是编码,事实一个都不能少。
    #[test]
    fn the_two_conclusion_shapes_carry_the_same_facts() {
        let all = vec![
            Conclusion::Corrected {
                text: "Python 列表".into(),
                kind: CorrectionType::Number,
                note: "n".into(),
                reason: "r".into(),
            },
            Conclusion::Kept {
                note: "n".into(),
                reason: "r".into(),
            },
            Conclusion::Uncertain {
                note: "n".into(),
                reason: "r".into(),
            },
        ];
        for c in all {
            let stored = StoredConclusion::from(&c);
            assert_eq!(Conclusion::from(&stored), c, "来回一趟该一样");
        }
    }

    /// 明文投影被人手改 → 体检要报出来(但门不看它,所以骗不过去)。
    #[test]
    fn a_hand_edited_projection_is_reported() {
        let shots = Shots {
            generated_at: crate::now(),
            shots: vec![Shot {
                id: ShotId::FIRST,
                t: 1.0,
                file: crate::relpath::RelPath::new("01_preprocess/shots/s001_0001.0.jpg").unwrap(),
                origin: ShotOrigin::DenseRegion,
                region_id: None,
                status: ShotStatus::Keep,
                reason: None,
                created_at: crate::now(),
                check: None,
            }],
        };
        let state = ProjectState::new();
        assert!(state.mismatches(Some(&shots), None, None).is_empty());

        // 有人在明文里给 s001 补了一条结论 —— 状态里没有
        let mut faked = shots.clone();
        faked.shots[0].check = Some(check());
        let lines = state.mismatches(Some(&faked), None, None);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("s001"), "{lines:?}");

        // 反过来:状态里有、清单里没有这张图
        let mut real = ProjectState::new();
        real.shot_mut(ShotId::FIRST).check = Some(check());
        let lines = real.mismatches(Some(&Shots::new()), None, None);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("清单里没有这张图"), "{lines:?}");
    }
}
