//! Prompt 构建器 — 统一管理系统级开发约束和规范
//!
//! 将所有发送给 AI 的 prompt 中需要包含的架构约束、开发规范、
//! 技术要求集中管理，确保每次与 AI 交互都携带完整的开发指令。
//!
//! ## 核心约束
//!
//! 1. **前沿技术** — 使用最新最前沿的技术和研究成果
//! 2. **SOLID 原则** — SRP/OCP/LSP/ISP/DIP
//! 3. **Spec-Driven Development** — Mission → Tech Stack → Roadmap → Feature Phase
//! 4. **TDD** — 先写测试再写实现
//! 5. **代码质量** — 可编译、可测试、可维护

// ============================================================================
//  SystemPrompt — 系统级开发约束
// ============================================================================

/// 系统级开发约束 — 注入到所有发送给 AI 的 prompt 中
///
/// 包含:
/// - 前沿技术要求
/// - SOLID 架构原则
/// - Spec-Driven Development 流程
/// - TDD 开发模式
/// - 代码质量标准
/// - 文件输出格式
#[derive(Debug, Clone)]
pub struct SystemPrompt;

impl SystemPrompt {
    /// 构建完整的系统级约束 prompt
    ///
    /// 约束详情见项目根目录 constraints/SYSTEM_CONSTRAINTS.md
    /// 此方法生成简化的约束引用，完整约束请查看附件或约束文件
    pub fn build() -> String {
        Self::build_attachment_reference()
    }

    /// 构建规划阶段专用约束 — 在拆解目标时注入
    pub fn build_for_planning() -> String {
        Self::build()
    }

    /// 构建任务执行专用约束 — 在执行任务时注入
    pub fn build_for_task() -> String {
        Self::build()
    }

    /// 构建简短约束摘要 — 用于上下文衔接等 token 受限场景
    pub fn build_brief() -> String {
        let mut prompt = String::new();

        prompt.push_str("─── 🔧 开发约束 ───\n");
        prompt.push_str("  • 详见项目根目录 .cursorrules 或 constraints/SYSTEM_CONSTRAINTS.md\n");
        prompt.push_str("  • 前沿技术/SOLID/Spec-Driven/TDD/代码质量/安全/性能/API/文档\n");
        prompt.push_str("─── 约束结束 ───\n\n");

        prompt
    }

    /// 构建附件引用模式 — 用于支持附件上传的 AI (DeepSeek/Z.ai)
    ///
    /// 当 AI 支持文件上传时，使用此模式：
    /// 1. 上传 SYSTEM_CONSTRAINTS.md 附件
    /// 2. 使用此 prompt 引用附件
    ///
    /// 优势：
    /// - 减少主 prompt 的 token 消耗
    /// - 约束可以更长更详细
    /// - 便于版本管理和复用
    pub fn build_attachment_reference() -> String {
        let mut prompt = String::new();

        prompt.push_str("╔══════════════════════════════════════════════════════════════════╗\n");
        prompt.push_str("║  🔥 FORGE 系统级开发约束 — 必须严格执行的铁律 🔥                  ║\n");
        prompt.push_str("╚══════════════════════════════════════════════════════════════════╝\n\n");

        prompt.push_str("⚠️  铁律声明 (违反将导致代码被拒绝):\n");
        prompt.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        prompt.push_str("❌ 禁止: 不遵循附件《Forge 系统级开发约束》的任何行为\n");
        prompt.push_str("❌ 禁止: 跳过测试直接写实现代码\n");
        prompt.push_str("❌ 禁止: 使用 unwrap()/expect() 而不处理错误\n");
        prompt.push_str("❌ 禁止: 输出不完整的文件内容或省略代码\n");
        prompt.push_str("❌ 禁止: 生成无效格式的 Cargo.toml\n");
        prompt.push_str("❌ 禁止: 违反 SOLID 原则 (特别是 DIP 依赖倒置)\n");
        prompt.push_str("❌ 禁止: 在单元测试中访问真实的外部依赖\n");
        prompt.push_str("❌ 禁止: 大括号/圆括号/方括号不配对 (最常见的 AI 代码生成错误)\n");
        prompt.push_str("❌ 禁止: 使用 todo!()/unimplemented!()/panic!() (非测试代码)\n");
        prompt
            .push_str("❌ 禁止: 使用 unsafe 块/函数/实现 (非必要不使用, 必须时添加 SAFETY 注释)\n");
        prompt.push_str("❌ 禁止: 使用 unreachable!() 宏 (非测试代码)\n");
        prompt
            .push_str("❌ 禁止: 滥用 unwrap_or()/unwrap_or_default() 掩盖错误 (确认是否应传播)\n");
        prompt.push_str("❌ 禁止: 使用 ? 操作符的函数不返回 Result/Option 类型 (Session 119)\n");
        prompt.push_str(
            "❌ 禁止: 修改函数返回类型为 Result 后遗漏 use anyhow::Result; 导入 (Session 120)\n",
        );
        prompt.push_str(
            "❌ 禁止: 修改函数签名为 Result<T, E> 后遗漏函数体 Ok(...) 包装 (Session 121)\n",
        );
        prompt.push_str(
            "❌ 禁止: 使用 bail!()/ensure!() 宏但未导入 use anyhow::{bail, ensure}; (Session 122)\n",
        );
        prompt.push_str(
            "❌ 禁止: 使用 anyhow!() 宏或 .context() 但未导入 use anyhow::{anyhow, Context}; (Session 123)\n",
        );
        prompt.push_str(
            "❌ 禁止: 使用 quote! 宏时括号不配对 — #(#field),* 等重复语法需确保 () {} [] 配对 (Session 123)\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 HashMap/HashSet/BTreeMap 等标准库类型但未导入 use std::collections::... (Session 124)\n",
);
        prompt.push_str(
"❌ 禁止: 使用 Arc/Mutex/RwLock/Cell/RefCell 等类型但未导入 use std::sync::... / use std::cell::... (Session 125)\n",
);
        prompt.push_str(
"❌ 禁止: 使用 Command/Instant/Duration/TcpListener 等类型但未导入对应 use std::process::... / use std::time::... / use std::net::... (Session 125)\n",
);
        prompt.push_str(
 "❌ 禁止: 使用 thread/Thread/JoinHandle/PhantomData/Cow/Sender/Receiver/AtomicBool 等类型但未导入对应 use std::thread::... / use std::marker::... / use std::borrow::... / use std::sync::mpsc::... / use std::sync::atomic::... (Session 126)\n",
 );
        prompt.push_str(
"❌ 禁止: 使用 Pin/Ordering/Range/RangeInclusive/TypeId/Any/Formatter/Display/Debug/FromIterator/Peekable/Hash/Hasher/NonZeroU32/NonZeroU64/NonZeroUsize/Entry 等类型但未导入对应 use std::pin::... / use std::cmp::... / use std::ops::... / use std::any::... / use std::fmt::... / use std::iter::... / use std::hash::... / use std::num::... / use std::collections::hash_map::... (Session 127)\n",
);
        prompt.push_str(
"❌ 禁止: 使用 Serialize/Deserialize/Regex/DateTime/NaiveDateTime 等外部 crate 类型但未导入 use serde::... / use regex::... / use chrono::... (Session 127)\n",
);
        prompt.push_str(
"❌ 禁止: 使用 info!/warn!/error!/debug!/trace! 等 tracing 宏但未导入 use tracing::{...}; (Session 127)\n",
);
        prompt.push_str(
"❌ 禁止: 使用 Future/Poll/Waker/Layout/CString/CStr/Pattern 等类型但未导入 use std::future::... / use std::task::... / use std::alloc::... / use std::ffi::... / use std::str::pattern::... (Session 128)\n",
);
        prompt.push_str(
"❌ 禁止: 使用 Client/Response/StatusCode (reqwest) / Value/json! (serde_json) / JoinHandle/spawn/join!/select! (tokio) 等外部 crate 类型或宏但未导入 use reqwest::... / use serde_json::... / use tokio::... (Session 128)\n\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 Child/Stdio (std::process) / OsStr/OsString (std::ffi) 等类型但未导入 use std::process::... / use std::ffi::... (Session 129)\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 IterTools/iproduct!/izip!/multiunzip! (itertools) / #[derive(Error)] (thiserror) / #[async_trait] (async_trait) 等外部 crate 类型或宏但未导入 use itertools::... / use thiserror::... / use async_trait::... (Session 129)\n\n",
);
        prompt.push_str(
"❌ 禁止: 使用 RawFd/OwnedFd/BorrowedFd (std::os::unix::io) / RawHandle/OwnedHandle/BorrowedHandle (std::os::windows::io) 等平台特定类型但未导入 use std::os::unix::io::... / use std::os::windows::io::... (Session 130)\n",
);
        prompt.push_str(
            "❌ 禁止: 使用 Arg/Subcommand/ArgAction (clap) / Uuid (uuid) / Url (url) / Level/LevelFilter/log! (log) / EnvFilter (tracing_subscriber) 等外部 crate 类型或宏但未导入 use clap::... / use uuid::... / use url::... / use log::... / use tracing_subscriber::... (Session 130)\n\n",
        );
        prompt.push_str(
 "❌ 禁止: 使用 Rng/ThreadRng (rand) / DashMap (dashmap) / ParallelIterator/.par_iter() (rayon) / Array1/Array2 (ndarray) / StreamExt/Stream (tokio_stream) 等外部 crate 类型或 trait 方法但未导入 use rand::... / use dashmap::... / use rayon::prelude::... / use ndarray::... / use tokio_stream::... (Session 131)\n",
 );
        prompt.push_str(
  "❌ 禁止: 调用 .lock() 但未导入 use std::sync::Mutex; / 调用 .gen_range() 但未导入 use rand::Rng; / 调用 .par_iter() 但未导入 use rayon::prelude::ParallelIterator; (Session 131 trait 方法检测)\n\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 Router/Json/Handler/IntoResponse (axum) / Tera/Context (tera) / #[derive(Template)] (askama) / Connection/Statement/Row/params! (rusqlite) / Cmd/AsyncConnection (redis) 等外部 crate 类型或宏但未导入 use axum::... / use tera::... / use askama::... / use rusqlite::... / use redis::... (Session 132)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .read()/.write() 但未导入 use std::sync::RwLock; / 调用 .send() 但未导入 use std::sync::mpsc::Sender; / 调用 .recv() 但未导入 use std::sync::mpsc::Receiver; (Session 132 trait 方法检测)\n\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 Pool/QueryBuilder/Executor/AnyPool/MySqlPool/PgPool/SqlitePool/query!/query_as! (sqlx) / TypedHeader (axum-extra) / Message/Transport/SmtpTransport/AsyncTransport (lettre) / Config (config) 等外部 crate 类型或宏但未导入 use sqlx::... / use axum_extra::... / use lettre::... / use config::... (Session 133)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .try_read()/.try_write() 但未导入 use std::sync::RwLock; / 调用 .try_send() 但未导入 use std::sync::mpsc::Sender; (Session 133 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 Body/HeaderMap/Uri/Method (hyper) / Service/ServiceExt/Layer/BoxError (tower) / Status/Code/Channel (tonic) / TokenStream/Span/Ident/Literal (proc_macro2) / DeriveInput/ItemFn/ItemStruct/ItemEnum (syn) / quote!/format_ident!/parse_quote!/parse_str! (quote/syn 宏) 等外部 crate 类型或宏但未导入 use hyper::... / use tower::... / use tonic::... / use proc_macro2::... / use syn::... / use quote::... (Session 134)\n",
        );
        prompt.push_str(
  "❌ 禁止: 使用 .await? 但函数未返回 Result / 调用 .spawn() 但未导入 use tokio::spawn; (Session 134 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 Filter/Reply (warp) / HttpResponse/HttpRequest/Responder/HttpServer (actix-web) / EntityTrait/Database/DbConn/PaginatorTrait (sea-orm) / QueryDsl/RunQueryDsl/ExpressionMethods/PgConnection/SqliteConnection (diesel) 等外部 crate 类型但未导入 use warp::... / use actix_web::... / use sea_orm::... / use diesel::... (Session 135)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .collect() 但未导入 use std::iter::FromIterator; / 调用 .into_iter() 但未导入 use std::iter::IntoIterator; (Session 135 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 Mailgun/Recipient (mailgun) / Charge/Customer/PaymentIntent (stripe) / PutObjectOutput/GetObjectOutput (aws-sdk-s3) / SdkConfig/BehaviorVersion (aws-config) 等外部 crate 类型但未导入 use mailgun::... / use stripe::... / use aws_sdk_s3::... / use aws_config::... (Session 136)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .map()/.filter() 但未导入 use std::iter::Iterator; (Session 136 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 BasicClient/AuthorizationCode/AccessToken/CsrfToken/PkceCodeVerifier (oauth2) / Pattern/GlobBuilder (glob) / Cookie/CookieJar (cookie) 等外部 crate 类型但未导入 use oauth2::... / use glob::... / use cookie::... (Session 137)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .zip()/.chain()/.enumerate() 但未导入 use std::iter::Iterator; (Session 137 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 EnvError (dotenv) / AppBuilder/AppHandle/Manager/Invoke (tauri) / Device/Queue/Surface/SurfaceConfiguration/ShaderModule (wgpu) 等外部 crate 类型但未导入 use dotenv::... / use tauri::... / use wgpu::... (Session 138)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .flat_map()/.peekable()/.skip() 但未导入 use std::iter::Iterator; (Session 138 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 Builder/Target/Filter (env_logger) / Watcher/EventKind/Event (notify) / ShadowBuilder (shadow-rs) 等外部 crate 类型但未导入 use env_logger::... / use notify::... / use shadow_rs::... (Session 139)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .take()/.rev()/.step_by() 但未导入 use std::iter::Iterator; (Session 139 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 System/CpuCore/Disk (sysinfo) / SerialPort (serialport) / machine_uid (machine-uid) 等外部 crate 类型但未导入 use sysinfo::... / use serialport::... / use machine_uid::... (Session 141)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .cloned()/.copied()/.fuse() 但未导入 use std::iter::Iterator; (Session 141 trait 方法检测)\n",
  );
        prompt.push_str(
"❌ 禁止: 使用 EnvLoader/EnvIter (dotenvy) / FdLock (fd-lock) / NixPath/Errno (nix) / Utf8PathBuf/Utf8Path (camino) 等外部 crate 类型但未导入 use dotenvy::... / use fd_lock::... / use nix::... / use camino::... (Session 142)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .flatten()/.max()/.min()/.sum()/.product() 但未导入 use std::iter::Iterator; (Session 142 trait 方法检测)\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 Enigo (enigo) / Display (x11) / HMODULE (winapi) / CGContext (core-graphics) 等外部 crate 类型但未导入 use enigo::... / use x11::... / use winapi::... / use core_graphics::... (Session 144)\n",
        );
        prompt.push_str(
"❌ 禁止: 调用 .any()/.all()/.find()/.position()/.count()/.fold()/.reduce()/.partition()/.for_each() 但未导入 use std::iter::Iterator; (Session 144 trait 方法检测)\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 ImageBuffer/Rgba (image) / Drawing (imageproc) / Font/PositionedGlyph (rusttype) / ChartContext (plotters) 等外部 crate 类型但未导入 use image::... / use imageproc::... / use rusttype::... / use plotters::... (Session 145)\n",
        );
        prompt.push_str(
"❌ 禁止: 调用 .scan()/.unzip()/.cycle() 但未导入 use std::iter::Iterator; (Session 145 trait 方法检测)\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 Line/Layout/Block/Widget (ratatui) / execute/queue/terminal (crossterm) / Frame/Terminal (tui) / Display/Surface (glium) / VkHandle/VulkanObject (vulkano) / open_npz/save_npz (ndarray-npy) 等外部 crate 类型但未导入 use ratatui::... / use crossterm::... / use tui::... / use glium::... / use vulkano::... / use ndarray_npy::... (Session 146)\n",
        );
        prompt.push_str(
"❌ 禁止: 调用 .chunks()/.windows()/.rchunks()/.as_chunks()/.array_chunks() 但未导入 use std::iter::Iterator; (Session 146 trait 方法检测)\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 App/Frame (eframe) / Context/Ui (egui) / Application/Command (iced) / AppDelegate/Widget (druid) / ComponentHandle/Model (slint) 等外部 crate 类型但未导入 use eframe::... / use egui::... / use iced::... / use druid::... / use slint::... (Session 147)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .first()/.last()/.nth()/.next_back()/.rposition()/.rfold()/.rfind() 但未导入 use std::iter::Iterator; (Session 147 trait 方法检测)\n",
        );
        prompt.push_str(
"❌ 禁止: 使用 num_cpus (num_cpus) / Lazy (once_cell) / GzEncoder/GzDecoder (flate2) / Archive/Entry (tar) / WalkDir (walkdir) / TempDir/NamedTempFile (tempfile) / ProgressBar/MultiProgress (indicatif) / Input/Select (dialoguer) / Term/Style (console) 等外部 crate 类型但未导入 use num_cpus::... / use once_cell::... / use flate2::... / use tar::... / use walkdir::... / use tempfile::... / use indicatif::... / use dialoguer::... / use console::... (Session 148)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .iter_mut()/.split()/.splitn()/.rsplit()/.rsplitn()/.lines()/.chars()/.bytes() 但未导入 use std::iter::Iterator; (Session 148 trait 方法检测)\n",
        );
        prompt.push_str(
  "❌ 禁止: 调用 .iter()/.try_fold()/.try_for_each()/.is_sorted()/.collect_into() 但未导入 use std::iter::Iterator; (Session 149 trait 方法检测)\n",
        );
        prompt.push_str(
  "❌ 禁止: 输出不完整的文件 — 每个文件必须从第一行到最后一行完整输出, 不要省略中间部分 (Session 140 截断检测)\n",
  );
        prompt.push_str(
  "❌ 禁止: 在集成测试(#[cfg(test)] 模块外的测试函数)中使用 ? 操作符操作 Option 类型 — Option 的 ? 只能用于返回 Option 的函数, 返回 Result 的函数中不能用 ? 操作 Option (Session 150 测试代码质量)\n",
  );
        prompt.push_str(
  "❌ 禁止: 在测试代码中生成不配对的括号 — 每个测试函数的 { } ( ) [ ] 必须严格配对, 特别是 assert_eq! / assert! 宏调用中的括号 (Session 150 测试代码质量)\n",
  );
        prompt.push_str(
  "❌ 禁止: 在测试函数中混合返回类型 — 如果测试函数返回 Result<T, E>, 则所有 ? 操作符必须操作 Result 类型, 不能操作 Option 类型 (Session 150 测试代码质量)\n",
  );
        prompt.push_str(
  "❌ 禁止: 使用 FromStr/Write/Deref/DerefMut/Index/IndexMut/Drop/FusedIterator/DoubleEndedIterator/ExactSizeIterator 等 trait 但未导入 use std::str::FromStr / use std::fmt::Write / use std::ops::{...} / use std::iter::{...} (Session 150 导入检测)\n",
  );

        prompt.push_str("✅ 必须: 严格遵循附件《Forge 系统级开发约束》中的全部 10 大约束\n");
        prompt.push_str("✅ 必须: TDD 模式 — 先写测试，再写实现，最后重构\n");
        prompt.push_str("✅ 必须: 每个公共函数都有对应的单元测试\n");
        prompt.push_str("✅ 必须: 使用 ```file:路径``` 格式输出完整文件内容\n");
        prompt.push_str("✅ 必须: 代码零警告、零 clippy 警告\n");
        prompt.push_str("✅ 必须: 使用 trait 抽象外部依赖，支持无 Chrome 环境测试\n");
        prompt.push_str("✅ 必须: 确保所有 { } ( ) [ ] 配对 — 输出前逐个检查\n");
        prompt.push_str("✅ 必须: 公共 API (pub fn/struct/enum/trait) 有 /// 文档注释\n");
        prompt.push_str(
            "✅ 必须: 返回 Result/Option/bool/Vec/String/&str/Box/Rc/Arc/Cow/PathBuf 的公共函数添加 #[must_use] 属性\n",
        );
        prompt.push_str(
            "✅ 必须: 每个文件输出完整 — 从 use 语句到最后的 } 闭合, 不要省略任何中间代码 (Session 140)\n\n",
        );

        prompt.push_str("📎 附件内容 (必须逐条执行):\n");
        prompt.push_str("  1. 前沿技术要求 — 使用最新最前沿的技术\n");
        prompt.push_str("  2. SOLID 架构原则 — SRP/OCP/LSP/ISP/DIP\n");
        prompt.push_str("  3. Spec-Driven Development — Mission→Tech Stack→Roadmap→Feature\n");
        prompt.push_str("  4. TDD 开发模式 — 测试金字塔 70:20:10、Mock 规范\n");
        prompt.push_str("  5. 代码质量标准 — 零警告、anyhow 错误处理\n");
        prompt.push_str("  6. 安全与可靠性 — 输入验证/防御式编程/RAII\n");
        prompt.push_str("  7. 性能与可观测性 — async/await、tracing 追踪\n");
        prompt.push_str("  8. API 设计规范 — RESTful、幂等性、统一错误格式\n");
        prompt.push_str("  9. 文档规范 — README、代码注释、ADR\n");
        prompt.push_str("  10. 文件输出格式 — ```file:路径```、完整 TOML\n\n");

        prompt.push_str("🔴 重要: 如有冲突，以附件《Forge 系统级开发约束》为准。\n");
        prompt.push_str("🔴 重要: 每次回复前，请自检是否违反了上述任何铁律。\n\n");

        prompt.push_str("╔══════════════════════════════════════════════════════════════════╗\n");
        prompt.push_str("║  开始执行 — 请严格遵循上述铁律生成代码                             ║\n");
        prompt.push_str("╚══════════════════════════════════════════════════════════════════╝\n\n");

        prompt
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== SystemPrompt::build =====

    #[test]
    fn test_build_contains_attachment_reference() {
        let prompt = SystemPrompt::build();
        assert!(prompt.contains("FORGE 系统级开发约束"), "必须引用约束文件");
        assert!(prompt.contains("铁律"), "必须提及铁律");
        assert!(prompt.contains("禁止:"), "必须列出禁止事项");
        assert!(prompt.contains("必须:"), "必须列出必须事项");
    }

    #[test]
    fn test_build_is_deterministic() {
        let p1 = SystemPrompt::build();
        let p2 = SystemPrompt::build();
        assert_eq!(p1, p2, "SystemPrompt::build() 应是确定性的");
    }

    // ===== SystemPrompt::build_for_planning =====

    #[test]
    fn test_build_for_planning_contains_attachment_ref() {
        let prompt = SystemPrompt::build_for_planning();
        assert!(
            prompt.contains("FORGE 系统级开发约束"),
            "规划 prompt 必须引用约束文件"
        );
        assert!(prompt.contains("铁律"), "规划 prompt 必须提及铁律");
    }

    // ===== SystemPrompt::build_for_task =====

    #[test]
    fn test_build_for_task_contains_attachment_ref() {
        let prompt = SystemPrompt::build_for_task();
        assert!(
            prompt.contains("FORGE 系统级开发约束"),
            "任务 prompt 必须引用约束文件"
        );
        assert!(prompt.contains("铁律"), "任务 prompt 必须提及铁律");
    }

    // ===== build_brief =====

    #[test]
    fn test_build_brief_is_shorter_than_full() {
        let full = SystemPrompt::build();
        let brief = SystemPrompt::build_brief();
        assert!(
            brief.len() < full.len(),
            "简短约束应比完整约束短 ({} < {})",
            brief.len(),
            full.len()
        );
    }

    #[test]
    fn test_build_brief_contains_constraints_ref() {
        let brief = SystemPrompt::build_brief();
        assert!(
            brief.contains(".cursorrules") || brief.contains("SYSTEM_CONSTRAINTS.md"),
            "简短约束必须引用约束文件"
        );
    }

    // ===== 不可变性测试 =====

    #[test]
    fn test_build_for_planning_is_deterministic() {
        let p1 = SystemPrompt::build_for_planning();
        let p2 = SystemPrompt::build_for_planning();
        assert_eq!(p1, p2, "SystemPrompt::build_for_planning() 应是确定性的");
    }

    #[test]
    fn test_build_for_task_is_deterministic() {
        let p1 = SystemPrompt::build_for_task();
        let p2 = SystemPrompt::build_for_task();
        assert_eq!(p1, p2, "SystemPrompt::build_for_task() 应是确定性的");
    }

    // ===== Session 113: 大括号匹配提醒测试 =====

    #[test]
    fn test_build_contains_brace_matching_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("大括号"),
            "系统 prompt 应包含大括号匹配警告"
        );
        assert!(prompt.contains("配对"), "系统 prompt 应包含括号配对提醒");
    }

    #[test]
    fn test_build_contains_brace_check_instruction() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("逐个检查"),
            "系统 prompt 应包含输出前逐个检查括号的指令"
        );
    }

    // ===== Session 114: 代码质量禁止项测试 =====

    #[test]
    fn test_build_contains_todo_unimplemented_panic_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("todo!()"),
            "系统 prompt 应包含 todo!() 禁止项"
        );
        assert!(
            prompt.contains("unimplemented!()"),
            "系统 prompt 应包含 unimplemented!() 禁止项"
        );
        assert!(
            prompt.contains("panic!()"),
            "系统 prompt 应包含 panic!() 禁止项"
        );
    }

    // ===== Session 115: unsafe + 公共 API 文档要求测试 =====

    #[test]
    fn test_build_contains_unsafe_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("unsafe"),
            "系统 prompt 应包含 unsafe 禁止项"
        );
        assert!(
            prompt.contains("SAFETY"),
            "系统 prompt 应提及 SAFETY 注释要求"
        );
    }

    #[test]
    fn test_build_contains_doc_comment_requirement() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("文档注释"),
            "系统 prompt 应包含公共 API 文档注释要求"
        );
        assert!(
            prompt.contains("pub fn/struct/enum/trait"),
            "系统 prompt 应明确列出需要文档注释的公共 API 类型"
        );
    }

    // ===== Session 116: unreachable!() + #[must_use] 要求测试 =====

    #[test]
    fn test_build_contains_unreachable_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("unreachable!()"),
            "系统 prompt 应包含 unreachable!() 禁止项"
        );
    }

    #[test]
    fn test_build_contains_must_use_requirement() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("#[must_use]"),
            "系统 prompt 应包含 #[must_use] 属性要求"
        );
        assert!(
            prompt.contains("Result/Option/bool"),
            "系统 prompt 应明确列出需要 #[must_use] 的返回类型"
        );
    }

    // ===== Session 117: unwrap_or + 扩展 must_use 类型测试 =====

    #[test]
    fn test_build_contains_unwrap_or_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("unwrap_or"),
            "系统 prompt 应包含 unwrap_or() 滥用警告"
        );
        assert!(
            prompt.contains("unwrap_or_default"),
            "系统 prompt 应包含 unwrap_or_default() 滥用警告"
        );
    }

    #[test]
    fn test_build_contains_expanded_must_use_types() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Vec"),
            "系统 prompt 应在 #[must_use] 要求中包含 Vec 类型"
        );
        assert!(
            prompt.contains("String"),
            "系统 prompt 应在 #[must_use] 要求中包含 String 类型"
        );
        assert!(
            prompt.contains("&str"),
            "系统 prompt 应在 #[must_use] 要求中包含 &str 类型"
        );
    }

    // ===== Session 118: 扩展 must_use 类型测试 =====

    #[test]
    fn test_build_contains_extended_must_use_types() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Box"),
            "系统 prompt 应在 #[must_use] 要求中包含 Box 类型"
        );
        assert!(
            prompt.contains("Arc"),
            "系统 prompt 应在 #[must_use] 要求中包含 Arc 类型"
        );
        assert!(
            prompt.contains("PathBuf"),
            "系统 prompt 应在 #[must_use] 要求中包含 PathBuf 类型"
        );
    }

    // ===== Session 119: ? 操作符返回类型约束测试 =====

    #[test]
    fn test_build_contains_question_mark_result_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("? 操作符的函数不返回 Result/Option"),
            "系统 prompt 应包含 ? 操作符函数必须返回 Result/Option 的约束"
        );
    }

    #[test]
    fn test_build_contains_anyhow_import_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("use anyhow::Result;"),
            "系统 prompt 应包含 use anyhow::Result 导入约束 (Session 120)"
        );
    }

    // ===== Session 121: Ok 包装约束测试 =====

    #[test]
    fn test_build_contains_ok_wrapping_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Ok(...)"),
            "系统 prompt 应包含 Ok(...) 包装约束 (Session 121)"
        );
    }

    // ===== Session 124: std 导入约束测试 =====

    #[test]
    fn test_build_contains_std_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("HashMap"),
            "系统 prompt 应包含 HashMap 导入约束 (Session 124)"
        );
        assert!(
            prompt.contains("std::collections"),
            "系统 prompt 应包含 std::collections 导入约束 (Session 124)"
        );
    }

    // ===== Session 125: std sync/cell/process/time/net 导入约束测试 =====

    #[test]
    fn test_build_contains_std_sync_cell_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Arc/Mutex/RwLock"),
            "系统 prompt 应包含 Arc/Mutex/RwLock 导入约束 (Session 125)"
        );
        assert!(
            prompt.contains("std::sync"),
            "系统 prompt 应包含 std::sync 导入约束 (Session 125)"
        );
        assert!(
            prompt.contains("std::cell"),
            "系统 prompt 应包含 std::cell 导入约束 (Session 125)"
        );
    }

    #[test]
    fn test_build_contains_std_process_time_net_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Command"),
            "系统 prompt 应包含 Command 导入约束 (Session 125)"
        );
        assert!(
            prompt.contains("std::process"),
            "系统 prompt 应包含 std::process 导入约束 (Session 125)"
        );
        assert!(
            prompt.contains("std::time"),
            "系统 prompt 应包含 std::time 导入约束 (Session 125)"
        );
        assert!(
            prompt.contains("std::net"),
            "系统 prompt 应包含 std::net 导入约束 (Session 125)"
        );
    }

    // ===== Session 123: anyhow!/Context + quote! 宏约束测试 =====

    #[test]
    fn test_build_contains_anyhow_macro_context_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("anyhow!()"),
            "系统 prompt 应包含 anyhow!() 宏导入约束 (Session 123)"
        );
        assert!(
            prompt.contains("Context"),
            "系统 prompt 应包含 Context trait 导入约束 (Session 123)"
        );
    }

    #[test]
    fn test_build_contains_quote_macro_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("quote!"),
            "系统 prompt 应包含 quote! 宏括号配对约束 (Session 123)"
        );
    }

    // ===== Session 126: thread/marker/borrow/mpsc/atomic 导入约束测试 =====

    #[test]
    fn test_build_contains_thread_marker_borrow_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("thread"),
            "系统 prompt 应包含 thread 导入约束 (Session 126)"
        );
        assert!(
            prompt.contains("PhantomData"),
            "系统 prompt 应包含 PhantomData 导入约束 (Session 126)"
        );
        assert!(
            prompt.contains("Cow"),
            "系统 prompt 应包含 Cow 导入约束 (Session 126)"
        );
    }

    #[test]
    fn test_build_contains_mpsc_atomic_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Sender"),
            "系统 prompt 应包含 Sender 导入约束 (Session 126)"
        );
        assert!(
            prompt.contains("AtomicBool"),
            "系统 prompt 应包含 AtomicBool 导入约束 (Session 126)"
        );
        assert!(
            prompt.contains("std::sync::atomic"),
            "系统 prompt 应包含 std::sync::atomic 导入约束 (Session 126)"
        );
    }

    // ===== Session 127: 新增 std 类型 + 外部 crate 导入约束测试 =====

    #[test]
    fn test_build_contains_pin_ordering_range_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Pin"),
            "系统 prompt 应包含 Pin 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("Ordering"),
            "系统 prompt 应包含 Ordering 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("Range"),
            "系统 prompt 应包含 Range 导入约束 (Session 127)"
        );
    }

    #[test]
    fn test_build_contains_typeid_any_formatter_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("TypeId"),
            "系统 prompt 应包含 TypeId 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("Formatter"),
            "系统 prompt 应包含 Formatter 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("Display"),
            "系统 prompt 应包含 Display 导入约束 (Session 127)"
        );
    }

    #[test]
    fn test_build_contains_nonzero_entry_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("NonZeroU32"),
            "系统 prompt 应包含 NonZeroU32 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("Entry"),
            "系统 prompt 应包含 Entry 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("std::num"),
            "系统 prompt 应包含 std::num 导入约束 (Session 127)"
        );
    }

    #[test]
    fn test_build_contains_external_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Serialize"),
            "系统 prompt 应包含 Serialize 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("Deserialize"),
            "系统 prompt 应包含 Deserialize 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("Regex"),
            "系统 prompt 应包含 Regex 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("DateTime"),
            "系统 prompt 应包含 DateTime 导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("use serde::"),
            "系统 prompt 应包含 use serde:: 导入约束 (Session 127)"
        );
    }

    #[test]
    fn test_build_contains_tracing_macro_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("info!"),
            "系统 prompt 应包含 info! 宏导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("warn!"),
            "系统 prompt 应包含 warn! 宏导入约束 (Session 127)"
        );
        assert!(
            prompt.contains("use tracing::"),
            "系统 prompt 应包含 use tracing:: 导入约束 (Session 127)"
        );
    }

    #[test]
    fn test_build_contains_s128_std_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Future"),
            "系统 prompt 应包含 Future 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("Poll"),
            "系统 prompt 应包含 Poll 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("Waker"),
            "系统 prompt 应包含 Waker 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("std::future"),
            "系统 prompt 应包含 std::future 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("std::task"),
            "系统 prompt 应包含 std::task 导入约束 (Session 128)"
        );
    }

    #[test]
    fn test_build_contains_s128_external_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("reqwest"),
            "系统 prompt 应包含 reqwest 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("serde_json"),
            "系统 prompt 应包含 serde_json 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("tokio"),
            "系统 prompt 应包含 tokio 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("Client"),
            "系统 prompt 应包含 Client 导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("json!"),
            "系统 prompt 应包含 json! 宏导入约束 (Session 128)"
        );
        assert!(
            prompt.contains("spawn"),
            "系统 prompt 应包含 spawn 导入约束 (Session 128)"
        );
    }

    #[test]
    fn test_build_contains_s129_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("itertools"),
            "系统 prompt 应包含 itertools 导入约束 (Session 129)"
        );
        assert!(
            prompt.contains("thiserror"),
            "系统 prompt 应包含 thiserror 导入约束 (Session 129)"
        );
        assert!(
            prompt.contains("async_trait"),
            "系统 prompt 应包含 async_trait 导入约束 (Session 129)"
        );
        assert!(
            prompt.contains("IterTools"),
            "系统 prompt 应包含 IterTools 导入约束 (Session 129)"
        );
        assert!(
            prompt.contains("Child"),
            "系统 prompt 应包含 Child 导入约束 (Session 129)"
        );
        assert!(
            prompt.contains("OsStr"),
            "系统 prompt 应包含 OsStr 导入约束 (Session 129)"
        );
    }

    #[test]
    fn test_build_contains_s130_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("clap"),
            "系统 prompt 应包含 clap 导入约束 (Session 130)"
        );
        assert!(
            prompt.contains("uuid"),
            "系统 prompt 应包含 uuid 导入约束 (Session 130)"
        );
        assert!(
            prompt.contains("url"),
            "系统 prompt 应包含 url 导入约束 (Session 130)"
        );
        assert!(
            prompt.contains("tracing_subscriber"),
            "系统 prompt 应包含 tracing_subscriber 导入约束 (Session 130)"
        );
        assert!(
            prompt.contains("RawFd"),
            "系统 prompt 应包含 RawFd 导入约束 (Session 130)"
        );
        assert!(
            prompt.contains("RawHandle"),
            "系统 prompt 应包含 RawHandle 导入约束 (Session 130)"
        );
        assert!(
            prompt.contains("EnvFilter"),
            "系统 prompt 应包含 EnvFilter 导入约束 (Session 130)"
        );
    }

    #[test]
    fn test_build_contains_s131_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("rand"),
            "系统 prompt 应包含 rand 导入约束 (Session 131)"
        );
        assert!(
            prompt.contains("dashmap"),
            "系统 prompt 应包含 dashmap 导入约束 (Session 131)"
        );
        assert!(
            prompt.contains("rayon"),
            "系统 prompt 应包含 rayon 导入约束 (Session 131)"
        );
        assert!(
            prompt.contains("ndarray"),
            "系统 prompt 应包含 ndarray 导入约束 (Session 131)"
        );
        assert!(
            prompt.contains("tokio_stream"),
            "系统 prompt 应包含 tokio_stream 导入约束 (Session 131)"
        );
        assert!(
            prompt.contains("ParallelIterator"),
            "系统 prompt 应包含 ParallelIterator 导入约束 (Session 131)"
        );
    }

    #[test]
    fn test_build_contains_s131_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".lock()"),
            "系统 prompt 应包含 .lock() trait 方法检测约束 (Session 131)"
        );
        assert!(
            prompt.contains(".gen_range()"),
            "系统 prompt 应包含 .gen_range() trait 方法检测约束 (Session 131)"
        );
        assert!(
            prompt.contains(".par_iter()"),
            "系统 prompt 应包含 .par_iter() trait 方法检测约束 (Session 131)"
        );
    }

    // ===== Session 132: 新增外部 crate + trait 方法检测约束测试 =====

    #[test]
    fn test_build_contains_s132_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("axum"),
            "系统 prompt 应包含 axum 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("tera"),
            "系统 prompt 应包含 tera 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("askama"),
            "系统 prompt 应包含 askama 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("rusqlite"),
            "系统 prompt 应包含 rusqlite 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("redis"),
            "系统 prompt 应包含 redis 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("Router"),
            "系统 prompt 应包含 Router 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("params!"),
            "系统 prompt 应包含 params! 宏导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("AsyncConnection"),
            "系统 prompt 应包含 AsyncConnection 导入约束 (Session 132)"
        );
    }

    #[test]
    fn test_build_contains_s132_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".read()"),
            "系统 prompt 应包含 .read() trait 方法检测约束 (Session 132)"
        );
        assert!(
            prompt.contains(".write()"),
            "系统 prompt 应包含 .write() trait 方法检测约束 (Session 132)"
        );
        assert!(
            prompt.contains(".send()"),
            "系统 prompt 应包含 .send() trait 方法检测约束 (Session 132)"
        );
        assert!(
            prompt.contains(".recv()"),
            "系统 prompt 应包含 .recv() trait 方法检测约束 (Session 132)"
        );
        assert!(
            prompt.contains("RwLock"),
            "系统 prompt 应包含 RwLock 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("mpsc::Sender"),
            "系统 prompt 应包含 mpsc::Sender 导入约束 (Session 132)"
        );
        assert!(
            prompt.contains("mpsc::Receiver"),
            "系统 prompt 应包含 mpsc::Receiver 导入约束 (Session 132)"
        );
    }

    #[test]
    fn test_build_contains_s133_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("sqlx"),
            "系统 prompt 应包含 sqlx 导入约束 (Session 133)"
        );
        assert!(
            prompt.contains("axum_extra"),
            "系统 prompt 应包含 axum_extra 导入约束 (Session 133)"
        );
        assert!(
            prompt.contains("lettre"),
            "系统 prompt 应包含 lettre 导入约束 (Session 133)"
        );
        assert!(
            prompt.contains("config"),
            "系统 prompt 应包含 config 导入约束 (Session 133)"
        );
        assert!(
            prompt.contains("TypedHeader"),
            "系统 prompt 应包含 TypedHeader 导入约束 (Session 133)"
        );
        assert!(
            prompt.contains("query!"),
            "系统 prompt 应包含 query! 宏导入约束 (Session 133)"
        );
    }

    #[test]
    fn test_build_contains_s133_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".try_read()"),
            "系统 prompt 应包含 .try_read() trait 方法检测约束 (Session 133)"
        );
        assert!(
            prompt.contains(".try_write()"),
            "系统 prompt 应包含 .try_write() trait 方法检测约束 (Session 133)"
        );
        assert!(
            prompt.contains(".try_send()"),
            "系统 prompt 应包含 .try_send() trait 方法检测约束 (Session 133)"
        );
    }

    #[test]
    fn test_build_contains_s134_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("hyper"),
            "系统 prompt 应包含 hyper 导入约束 (Session 134)"
        );
        assert!(
            prompt.contains("tower"),
            "系统 prompt 应包含 tower 导入约束 (Session 134)"
        );
        assert!(
            prompt.contains("tonic"),
            "系统 prompt 应包含 tonic 导入约束 (Session 134)"
        );
        assert!(
            prompt.contains("proc_macro2"),
            "系统 prompt 应包含 proc_macro2 导入约束 (Session 134)"
        );
        assert!(
            prompt.contains("syn"),
            "系统 prompt 应包含 syn 导入约束 (Session 134)"
        );
        assert!(
            prompt.contains("quote"),
            "系统 prompt 应包含 quote 导入约束 (Session 134)"
        );
    }

    #[test]
    fn test_build_contains_s134_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".await?"),
            "系统 prompt 应包含 .await? trait 方法检测约束 (Session 134)"
        );
        assert!(
            prompt.contains(".spawn()"),
            "系统 prompt 应包含 .spawn() trait 方法检测约束 (Session 134)"
        );
    }

    // ===== Session 135: 新增外部 crate + trait 方法检测约束测试 =====

    #[test]
    fn test_build_contains_s135_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("warp"),
            "系统 prompt 应包含 warp 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("actix_web"),
            "系统 prompt 应包含 actix_web 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("sea_orm"),
            "系统 prompt 应包含 sea_orm 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("diesel"),
            "系统 prompt 应包含 diesel 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("Filter"),
            "系统 prompt 应包含 Filter 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("HttpResponse"),
            "系统 prompt 应包含 HttpResponse 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("EntityTrait"),
            "系统 prompt 应包含 EntityTrait 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("PgConnection"),
            "系统 prompt 应包含 PgConnection 导入约束 (Session 135)"
        );
    }

    #[test]
    fn test_build_contains_s135_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".collect()"),
            "系统 prompt 应包含 .collect() trait 方法检测约束 (Session 135)"
        );
        assert!(
            prompt.contains(".into_iter()"),
            "系统 prompt 应包含 .into_iter() trait 方法检测约束 (Session 135)"
        );
        assert!(
            prompt.contains("FromIterator"),
            "系统 prompt 应包含 FromIterator 导入约束 (Session 135)"
        );
        assert!(
            prompt.contains("IntoIterator"),
            "系统 prompt 应包含 IntoIterator 导入约束 (Session 135)"
        );
    }

    // ===== Session 136: 新增外部 crate + trait 方法检测约束测试 =====

    #[test]
    fn test_build_contains_s136_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("mailgun"),
            "系统 prompt 应包含 mailgun 导入约束 (Session 136)"
        );
        assert!(
            prompt.contains("stripe"),
            "系统 prompt 应包含 stripe 导入约束 (Session 136)"
        );
        assert!(
            prompt.contains("aws_sdk_s3"),
            "系统 prompt 应包含 aws_sdk_s3 导入约束 (Session 136)"
        );
        assert!(
            prompt.contains("aws_config"),
            "系统 prompt 应包含 aws_config 导入约束 (Session 136)"
        );
        assert!(
            prompt.contains("Mailgun"),
            "系统 prompt 应包含 Mailgun 导入约束 (Session 136)"
        );
        assert!(
            prompt.contains("PaymentIntent"),
            "系统 prompt 应包含 PaymentIntent 导入约束 (Session 136)"
        );
        assert!(
            prompt.contains("PutObjectOutput"),
            "系统 prompt 应包含 PutObjectOutput 导入约束 (Session 136)"
        );
        assert!(
            prompt.contains("SdkConfig"),
            "系统 prompt 应包含 SdkConfig 导入约束 (Session 136)"
        );
    }

    #[test]
    fn test_build_contains_s136_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".map()"),
            "系统 prompt 应包含 .map() trait 方法检测约束 (Session 136)"
        );
        assert!(
            prompt.contains(".filter()"),
            "系统 prompt 应包含 .filter() trait 方法检测约束 (Session 136)"
        );
        assert!(
            prompt.contains("Iterator"),
            "系统 prompt 应包含 Iterator 导入约束 (Session 136)"
        );
    }

    // ===== Session 137: 新增外部 crate + trait 方法检测约束测试 =====

    #[test]
    fn test_build_contains_s137_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("oauth2"),
            "系统 prompt 应包含 oauth2 导入约束 (Session 137)"
        );
        assert!(
            prompt.contains("glob"),
            "系统 prompt 应包含 glob 导入约束 (Session 137)"
        );
        assert!(
            prompt.contains("cookie"),
            "系统 prompt 应包含 cookie 导入约束 (Session 137)"
        );
        assert!(
            prompt.contains("BasicClient"),
            "系统 prompt 应包含 BasicClient 导入约束 (Session 137)"
        );
        assert!(
            prompt.contains("AccessToken"),
            "系统 prompt 应包含 AccessToken 导入约束 (Session 137)"
        );
        assert!(
            prompt.contains("Pattern"),
            "系统 prompt 应包含 Pattern 导入约束 (Session 137)"
        );
        assert!(
            prompt.contains("Cookie"),
            "系统 prompt 应包含 Cookie 导入约束 (Session 137)"
        );
    }

    #[test]
    fn test_build_contains_s137_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".zip()"),
            "系统 prompt 应包含 .zip() trait 方法检测约束 (Session 137)"
        );
        assert!(
            prompt.contains(".chain()"),
            "系统 prompt 应包含 .chain() trait 方法检测约束 (Session 137)"
        );
        assert!(
            prompt.contains(".enumerate()"),
            "系统 prompt 应包含 .enumerate() trait 方法检测约束 (Session 137)"
        );
    }

    // ===== Session 138: 新增外部 crate + trait 方法检测约束测试 =====

    #[test]
    fn test_build_contains_s138_new_crate_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("dotenv"),
            "系统 prompt 应包含 dotenv 导入约束 (Session 138)"
        );
        assert!(
            prompt.contains("tauri"),
            "系统 prompt 应包含 tauri 导入约束 (Session 138)"
        );
        assert!(
            prompt.contains("wgpu"),
            "系统 prompt 应包含 wgpu 导入约束 (Session 138)"
        );
        assert!(
            prompt.contains("EnvError"),
            "系统 prompt 应包含 EnvError 导入约束 (Session 138)"
        );
        assert!(
            prompt.contains("AppBuilder"),
            "系统 prompt 应包含 AppBuilder 导入约束 (Session 138)"
        );
        assert!(
            prompt.contains("Device"),
            "系统 prompt 应包含 Device 导入约束 (Session 138)"
        );
        assert!(
            prompt.contains("SurfaceConfiguration"),
            "系统 prompt 应包含 SurfaceConfiguration 导入约束 (Session 138)"
        );
        assert!(
            prompt.contains("ShaderModule"),
            "系统 prompt 应包含 ShaderModule 导入约束 (Session 138)"
        );
    }

    #[test]
    fn test_build_contains_s138_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".flat_map()"),
            "系统 prompt 应包含 .flat_map() trait 方法检测约束 (Session 138)"
        );
        assert!(
            prompt.contains(".peekable()"),
            "系统 prompt 应包含 .peekable() trait 方法检测约束 (Session 138)"
        );
        assert!(
            prompt.contains(".skip()"),
            "系统 prompt 应包含 .skip() trait 方法检测约束 (Session 138)"
        );
    }

    #[test]
    fn test_build_contains_s139_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("env_logger"),
            "系统 prompt 应包含 env_logger 导入约束 (Session 139)"
        );
        assert!(
            prompt.contains("notify"),
            "系统 prompt 应包含 notify 导入约束 (Session 139)"
        );
        assert!(
            prompt.contains("shadow_rs"),
            "系统 prompt 应包含 shadow_rs 导入约束 (Session 139)"
        );
        assert!(
            prompt.contains("Builder"),
            "系统 prompt 应包含 Builder 导入约束 (Session 139)"
        );
        assert!(
            prompt.contains("Watcher"),
            "系统 prompt 应包含 Watcher 导入约束 (Session 139)"
        );
        assert!(
            prompt.contains("ShadowBuilder"),
            "系统 prompt 应包含 ShadowBuilder 导入约束 (Session 139)"
        );
    }

    #[test]
    fn test_build_contains_s139_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".take()"),
            "系统 prompt 应包含 .take() trait 方法检测约束 (Session 139)"
        );
        assert!(
            prompt.contains(".rev()"),
            "系统 prompt 应包含 .rev() trait 方法检测约束 (Session 139)"
        );
        assert!(
            prompt.contains(".step_by()"),
            "系统 prompt 应包含 .step_by() trait 方法检测约束 (Session 139)"
        );
    }

    // ===== Session 141: 新增外部 crate + trait 方法检测约束测试 =====

    #[test]
    fn test_build_contains_s141_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("sysinfo"),
            "系统 prompt 应包含 sysinfo 导入约束 (Session 141)"
        );
        assert!(
            prompt.contains("serialport"),
            "系统 prompt 应包含 serialport 导入约束 (Session 141)"
        );
        assert!(
            prompt.contains("machine_uid"),
            "系统 prompt 应包含 machine_uid 导入约束 (Session 141)"
        );
        assert!(
            prompt.contains("System"),
            "系统 prompt 应包含 System 导入约束 (Session 141)"
        );
        assert!(
            prompt.contains("SerialPort"),
            "系统 prompt 应包含 SerialPort 导入约束 (Session 141)"
        );
    }

    #[test]
    fn test_build_contains_s141_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".cloned()"),
            "系统 prompt 应包含 .cloned() trait 方法检测约束 (Session 141)"
        );
        assert!(
            prompt.contains(".copied()"),
            "系统 prompt 应包含 .copied() trait 方法检测约束 (Session 141)"
        );
        assert!(
            prompt.contains(".fuse()"),
            "系统 prompt 应包含 .fuse() trait 方法检测约束 (Session 141)"
        );
    }

    // ===== Session 142: 新增外部 crate + trait 方法检测约束测试 =====

    #[test]
    fn test_build_contains_s142_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("dotenvy"),
            "系统 prompt 应包含 dotenvy 导入约束 (Session 142)"
        );
        assert!(
            prompt.contains("fd_lock"),
            "系统 prompt 应包含 fd_lock 导入约束 (Session 142)"
        );
        assert!(
            prompt.contains("nix"),
            "系统 prompt 应包含 nix 导入约束 (Session 142)"
        );
        assert!(
            prompt.contains("camino"),
            "系统 prompt 应包含 camino 导入约束 (Session 142)"
        );
        assert!(
            prompt.contains("EnvLoader"),
            "系统 prompt 应包含 EnvLoader 导入约束 (Session 142)"
        );
        assert!(
            prompt.contains("FdLock"),
            "系统 prompt 应包含 FdLock 导入约束 (Session 142)"
        );
        assert!(
            prompt.contains("Utf8PathBuf"),
            "系统 prompt 应包含 Utf8PathBuf 导入约束 (Session 142)"
        );
    }

    #[test]
    fn test_build_contains_s142_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".flatten()"),
            "系统 prompt 应包含 .flatten() trait 方法检测约束 (Session 142)"
        );
        assert!(
            prompt.contains(".max()"),
            "系统 prompt 应包含 .max() trait 方法检测约束 (Session 142)"
        );
        assert!(
            prompt.contains(".min()"),
            "系统 prompt 应包含 .min() trait 方法检测约束 (Session 142)"
        );
        assert!(
            prompt.contains(".sum()"),
            "系统 prompt 应包含 .sum() trait 方法检测约束 (Session 142)"
        );
        assert!(
            prompt.contains(".product()"),
            "系统 prompt 应包含 .product() trait 方法检测约束 (Session 142)"
        );
    }

    // ===== Session 140: 代码完整性约束测试 =====

    #[test]
    fn test_build_contains_s140_completeness_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("不完整的文件"),
            "系统 prompt 应包含不完整文件禁止约束 (Session 140)"
        );
        assert!(
            prompt.contains("完整输出"),
            "系统 prompt 应包含完整输出要求 (Session 140)"
        );
        assert!(
            prompt.contains("不要省略任何中间代码"),
            "系统 prompt 应包含不省略中间代码约束 (Session 140)"
        );
    }

    // ===== Session 144: 导入约束测试 =====

    #[test]
    fn test_build_contains_s144_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("enigo"),
            "系统 prompt 应包含 enigo 导入约束 (Session 144)"
        );
        assert!(
            prompt.contains("x11"),
            "系统 prompt 应包含 x11 导入约束 (Session 144)"
        );
        assert!(
            prompt.contains("winapi"),
            "系统 prompt 应包含 winapi 导入约束 (Session 144)"
        );
        assert!(
            prompt.contains("core_graphics"),
            "系统 prompt 应包含 core_graphics 导入约束 (Session 144)"
        );
    }

    #[test]
    fn test_build_contains_s144_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".any()"),
            "系统 prompt 应包含 .any() trait 方法检测约束 (Session 144)"
        );
        assert!(
            prompt.contains(".fold()"),
            "系统 prompt 应包含 .fold() trait 方法检测约束 (Session 144)"
        );
        assert!(
            prompt.contains(".for_each()"),
            "系统 prompt 应包含 .for_each() trait 方法检测约束 (Session 144)"
        );
    }

    // ===== Session 146: 导入约束测试 =====

    #[test]
    fn test_build_contains_s146_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("ratatui"),
            "系统 prompt 应包含 ratatui 导入约束 (Session 146)"
        );
        assert!(
            prompt.contains("crossterm"),
            "系统 prompt 应包含 crossterm 导入约束 (Session 146)"
        );
        assert!(
            prompt.contains("tui"),
            "系统 prompt 应包含 tui 导入约束 (Session 146)"
        );
        assert!(
            prompt.contains("glium"),
            "系统 prompt 应包含 glium 导入约束 (Session 146)"
        );
        assert!(
            prompt.contains("vulkano"),
            "系统 prompt 应包含 vulkano 导入约束 (Session 146)"
        );
        assert!(
            prompt.contains("ndarray_npy"),
            "系统 prompt 应包含 ndarray_npy 导入约束 (Session 146)"
        );
    }

    #[test]
    fn test_build_contains_s146_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".chunks()"),
            "系统 prompt 应包含 .chunks() trait 方法检测约束 (Session 146)"
        );
        assert!(
            prompt.contains(".windows()"),
            "系统 prompt 应包含 .windows() trait 方法检测约束 (Session 146)"
        );
        assert!(
            prompt.contains(".rchunks()"),
            "系统 prompt 应包含 .rchunks() trait 方法检测约束 (Session 146)"
        );
        assert!(
            prompt.contains(".as_chunks()"),
            "系统 prompt 应包含 .as_chunks() trait 方法检测约束 (Session 146)"
        );
        assert!(
            prompt.contains(".array_chunks()"),
            "系统 prompt 应包含 .array_chunks() trait 方法检测约束 (Session 146)"
        );
    }

    // ===== Session 147: 导入约束测试 =====

    #[test]
    fn test_build_contains_s147_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("eframe"),
            "系统 prompt 应包含 eframe 导入约束 (Session 147)"
        );
        assert!(
            prompt.contains("egui"),
            "系统 prompt 应包含 egui 导入约束 (Session 147)"
        );
        assert!(
            prompt.contains("iced"),
            "系统 prompt 应包含 iced 导入约束 (Session 147)"
        );
        assert!(
            prompt.contains("druid"),
            "系统 prompt 应包含 druid 导入约束 (Session 147)"
        );
        assert!(
            prompt.contains("slint"),
            "系统 prompt 应包含 slint 导入约束 (Session 147)"
        );
    }

    #[test]
    fn test_build_contains_s147_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".first()"),
            "系统 prompt 应包含 .first() trait 方法检测约束 (Session 147)"
        );
        assert!(
            prompt.contains(".last()"),
            "系统 prompt 应包含 .last() trait 方法检测约束 (Session 147)"
        );
        assert!(
            prompt.contains(".nth()"),
            "系统 prompt 应包含 .nth() trait 方法检测约束 (Session 147)"
        );
        assert!(
            prompt.contains(".next_back()"),
            "系统 prompt 应包含 .next_back() trait 方法检测约束 (Session 147)"
        );
        assert!(
            prompt.contains(".rposition()"),
            "系统 prompt 应包含 .rposition() trait 方法检测约束 (Session 147)"
        );
        assert!(
            prompt.contains(".rfold()"),
            "系统 prompt 应包含 .rfold() trait 方法检测约束 (Session 147)"
        );
        assert!(
            prompt.contains(".rfind()"),
            "系统 prompt 应包含 .rfind() trait 方法检测约束 (Session 147)"
        );
    }

    // ===== Session 148: 导入约束测试 =====

    #[test]
    fn test_build_contains_s148_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("num_cpus"),
            "系统 prompt 应包含 num_cpus 导入约束 (Session 148)"
        );
        assert!(
            prompt.contains("once_cell"),
            "系统 prompt 应包含 once_cell 导入约束 (Session 148)"
        );
        assert!(
            prompt.contains("flate2"),
            "系统 prompt 应包含 flate2 导入约束 (Session 148)"
        );
        assert!(
            prompt.contains("walkdir"),
            "系统 prompt 应包含 walkdir 导入约束 (Session 148)"
        );
        assert!(
            prompt.contains("tempfile"),
            "系统 prompt 应包含 tempfile 导入约束 (Session 148)"
        );
    }

    #[test]
    fn test_build_contains_s148_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".iter_mut()"),
            "系统 prompt 应包含 .iter_mut() trait 方法检测约束 (Session 148)"
        );
        assert!(
            prompt.contains(".split()"),
            "系统 prompt 应包含 .split() trait 方法检测约束 (Session 148)"
        );
        assert!(
            prompt.contains(".lines()"),
            "系统 prompt 应包含 .lines() trait 方法检测约束 (Session 148)"
        );
        assert!(
            prompt.contains(".chars()"),
            "系统 prompt 应包含 .chars() trait 方法检测约束 (Session 148)"
        );
    }

    // ===== Session 145: 导入约束测试 =====

    #[test]
    fn test_build_contains_s145_external_crate_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("image"),
            "系统 prompt 应包含 image 导入约束 (Session 145)"
        );
        assert!(
            prompt.contains("imageproc"),
            "系统 prompt 应包含 imageproc 导入约束 (Session 145)"
        );
        assert!(
            prompt.contains("rusttype"),
            "系统 prompt 应包含 rusttype 导入约束 (Session 145)"
        );
        assert!(
            prompt.contains("plotters"),
            "系统 prompt 应包含 plotters 导入约束 (Session 145)"
        );
    }

    #[test]
    fn test_build_contains_s145_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".scan()"),
            "系统 prompt 应包含 .scan() trait 方法检测约束 (Session 145)"
        );
        assert!(
            prompt.contains(".unzip()"),
            "系统 prompt 应包含 .unzip() trait 方法检测约束 (Session 145)"
        );
        assert!(
            prompt.contains(".cycle()"),
            "系统 prompt 应包含 .cycle() trait 方法检测约束 (Session 145)"
        );
    }

    // ===== Session 149: 导入约束测试 =====

    #[test]
    fn test_build_contains_s149_trait_method_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains(".iter()"),
            "系统 prompt 应包含 .iter() trait 方法检测约束 (Session 149)"
        );
        assert!(
            prompt.contains(".try_fold()"),
            "系统 prompt 应包含 .try_fold() trait 方法检测约束 (Session 149)"
        );
        assert!(
            prompt.contains(".try_for_each()"),
            "系统 prompt 应包含 .try_for_each() trait 方法检测约束 (Session 149)"
        );
        assert!(
            prompt.contains(".is_sorted()"),
            "系统 prompt 应包含 .is_sorted() trait 方法检测约束 (Session 149)"
        );
        assert!(
            prompt.contains(".collect_into()"),
            "系统 prompt 应包含 .collect_into() trait 方法检测约束 (Session 149)"
        );
    }

    // ===== Session 150: 测试代码质量约束测试 =====

    #[test]
    fn test_build_contains_s150_option_question_mark_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("? 操作符操作 Option"),
            "系统 prompt 应包含 ? 操作符操作 Option 类型的禁止约束 (Session 150)"
        );
    }

    #[test]
    fn test_build_contains_s150_test_bracket_pairing_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("测试代码中生成不配对的括号"),
            "系统 prompt 应包含测试代码括号配对约束 (Session 150)"
        );
    }

    #[test]
    fn test_build_contains_s150_mixed_return_type_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("混合返回类型"),
            "系统 prompt 应包含测试函数混合返回类型禁止约束 (Session 150)"
        );
    }

    #[test]
    fn test_build_contains_s150_std_trait_imports_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("FromStr"),
            "系统 prompt 应包含 FromStr 导入约束 (Session 150)"
        );
        assert!(
            prompt.contains("Deref"),
            "系统 prompt 应包含 Deref 导入约束 (Session 150)"
        );
        assert!(
            prompt.contains("IndexMut"),
            "系统 prompt 应包含 IndexMut 导入约束 (Session 150)"
        );
        assert!(
            prompt.contains("FusedIterator"),
            "系统 prompt 应包含 FusedIterator 导入约束 (Session 150)"
        );
    }
}
