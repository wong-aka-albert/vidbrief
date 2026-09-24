//! 项目目录与数据结构(材料即接口)。
//!
//! 设计见 `docs/DESIGN.md` §4。
//!
//! 职责(待实现):
//! - 项目目录骨架:`01_preprocess/` `02_calibrate/` `03_summary/` `04_report/`(`§4.1`)
//! - 数据结构与 (反)序列化:`change_curve` / `dense_regions` / `transcript` / `shots` / `project.json`
//! - 进度标记 `progress`(`§4.2`)
//! - 并发控制:项目写锁 `project.lock`、原子写、修订号 `rev`(`§4.7`)
