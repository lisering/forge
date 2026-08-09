//! Forge — 自主软件开发 Agent
//! 驱动聊天网页进行多阶段自主开发: 需求拆解→代码生成→编译测试→反馈修复→直到完成

use anyhow::Result;
use clap::{Parser, Subcommand};
use forge::cdp;
use forge::chat::TimeoutConfig;
use forge::clarify::HeuristicClarificationChecker;
use forge::error_diagnosis::{ErrorDiagnoser, HeuristicErrorDiagnoser, HybridErrorDiagnoser};
use forge::extract::{extract_files, DefaultExtractor};
use forge::failover_chat::FailoverChatClient;
use forge::interaction::{AutoApprove, CliInteraction};
use forge::language::MultiLanguageTestRunner;
use forge::llm_clarify::{HybridClarificationChecker, LlmClient, OllamaClient};
use forge::package::package;
use forge::site_health::SiteHealthChecker;
use forge::testrunner;
use forge::traits::{
    ChatClient, ClarificationChecker, FileExtractor, HumanInteraction, TestRunner,
};
use forge::workspace::Workspace;
use forge::BrowserManager;
use forge::Orchestrator;
use std::path::PathBuf;
use tracing::{error, warn};

#[derive(Parser)]
#[command(name = "forge")]
#[command(about = "自主软件开发 Agent — 给终极目标,自动拆解、开发、测试、修复")]
struct Cli {
    /// Chrome 调试端口
    #[arg(short, long, default_value = "9222")]
    port: u16,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 发现并列出聊天标签页
    List,

    /// 检查所有聊天标签页的健康状态 (登录/限流/维护)
    Health {
        /// 使用指定索引的标签页 (不指定则检查所有标签页)
        #[arg(long)]
        tab: Option<usize>,
    },

    /// 在 Chrome 中打开一个聊天网页
    Open {
        /// 要打开的 URL
        url: String,
    },

    /// 向聊天标签页发送一条消息并显示回复
    Ask {
        message: String,
        #[arg(short, long, default_value = "120")]
        timeout: u64,
        /// 使用指定索引的标签页 (0=第一个, 默认=0)
        #[arg(long, default_value = "0")]
        tab: usize,
    },

    /// 📎 上传截图/文件到聊天页面并发送分析指令
    ///
    /// 主要用途: 上传 UI 截图让 Z.ai 分析 UI 设计和交互设计
    /// 支持 PNG/JPEG/WebP 等图片格式, 也支持 PDF/TXT 等文档
    Upload {
        /// 要上传的文件路径 (支持多个文件, 用空格分隔)
        files: Vec<String>,
        /// 上传后发送给 AI 的分析指令
        #[arg(short, long)]
        message: Option<String>,
        /// 使用指定索引的标签页 (0=第一个, 默认=0)
        #[arg(long, default_value = "0")]
        tab: usize,
        /// AI 回复超时 (秒, 默认 120)
        #[arg(short, long, default_value = "120")]
        timeout: u64,
    },

    /// 单次生成代码并打包 zip
    Generate {
        message: String,
        #[arg(short, long, default_value = "output.zip")]
        output: PathBuf,
        #[arg(short, long, default_value = "120")]
        timeout: u64,
        /// 使用指定索引的标签页 (0=第一个, 默认=0)
        #[arg(long, default_value = "0")]
        tab: usize,
    },

    /// 单任务 TDD: 生成→编译→测试→修复
    Develop {
        message: String,
        /// 工作区目录 (默认: ./projects/develop)
        #[arg(short, long, default_value = "./projects/develop")]
        workspace: PathBuf,
        #[arg(short, long, default_value = "3")]
        max_rounds: u32,
        #[arg(long, default_value = "180")]
        timeout: u64,
        /// 使用指定索引的标签页 (0=第一个, 默认=0)
        #[arg(long, default_value = "0")]
        tab: usize,
    },

    /// 🚀 自主多阶段开发: 给终极目标,自动拆解→开发→测试→修复→直到完成
    Run {
        /// 终极目标描述 (resume 模式下可省略,从 memory.json 恢复)
        goal: String,
        /// 工作区目录 (默认: ./projects/forge-project — AI 生成的源代码存放于此)
        #[arg(short, long, default_value = "./projects/forge-project")]
        workspace: PathBuf,
        /// 每个任务最大修复轮次
        #[arg(short, long, default_value = "3")]
        max_rounds: u32,
        /// 每次 AI 对话超时 (秒)
        #[arg(long, default_value = "180")]
        timeout: u64,
        /// 从断点恢复 (加载 .forge/memory.json,跳过已完成阶段/任务)
        #[arg(long)]
        resume: bool,
        /// 启用本地 LLM 增强自主追问 (需要 Ollama 运行中)
        #[arg(long)]
        llm_clarify: bool,
        /// Ollama API 端点 (默认: http://localhost:11434)
        #[arg(long, default_value = "http://localhost:11434")]
        ollama_endpoint: String,
        /// Ollama 模型名称 (默认: qwen2.5:3b)
        #[arg(long, default_value = "qwen2.5:3b")]
        ollama_model: String,
        /// 需求变更文件路径 (每行一个需求变更, 运行中自动加载)
        #[arg(long)]
        requirement_file: Option<PathBuf>,
        /// 启用人工干预模式 (CLI 交互式确认: 计划/任务/修复/需求变更)
        #[arg(long)]
        interactive: bool,
        /// 显示多语言支持信息 (默认已启用, 自动检测项目语言: Rust/Python/Go/Node)
        #[arg(long)]
        multi_language: bool,
        /// 启用并行任务执行 (方向 C: TaskGraph 依赖分析 + 并行分组执行)
        #[arg(long)]
        parallel: bool,
        /// 启用智能错误诊断 (方向 F: LLM 分析编译错误根因 + 历史学习)
        #[arg(long)]
        error_diagnosis: bool,
        /// 上下文衔接最大对话轮数 (借鉴方向 1: 对话过长时自动新开对话并交接上下文)
        ///
        /// 0 表示禁用 (默认 30)。启用后, 对话轮数超过此值时,
        /// 自动新开对话并发送包含完整状态信息的交接 prompt。
        #[arg(long, default_value = "30")]
        max_context_turns: usize,
        /// 转向提醒间隔 (借鉴方向 2: 每隔 N 轮对话注入提醒, 防止 AI 跑偏)
        ///
        /// 0 表示禁用 (默认 10)。启用后, 每隔此轮数在发送的消息前注入
        /// "转向提醒", 重新锚定 AI 注意力。推荐小于 max_context_turns。
        #[arg(long, default_value = "10")]
        steer_interval: usize,
        /// 循环终止检测 (借鉴方向 3: 检测修复死循环并改变策略)
        ///
        /// 0 表示禁用 (默认 3)。启用后, 同一编译错误连续出现此次数后
        /// 判定为死循环, 自动在修复 prompt 中追加"换方法"提示。
        /// 策略改变后仍死循环则建议跳过任务。
        #[arg(long, default_value = "3")]
        loop_detection: usize,
        /// 启用结构化开发追踪 (借鉴方向 4: 记录每轮 AI 交互的详细 trace 到 .forge/devtrace.jsonl)
        ///
        /// 默认启用。传 false 禁用。24 小时运行后可通过 trace 文件了解运行详情。
        #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
        dev_trace: bool,
        /// 启用 AI 自主指令 (借鉴方向 5: AI 可通过 /compact /skip /refocus /retry /escalate 指令影响 Forge 行为)
        ///
        /// 默认启用。传 false 禁用。启用后, AI 回复中的 slash commands 会被
        /// 自动检测并执行对应操作 (压缩上下文/跳过任务/重新聚焦/换方法重试/请求人工干预)。
        #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
        slash_commands: bool,
        /// 启用自动恢复 (24h 可靠性: Chrome 断连后自动重连)
        ///
        /// 默认启用。传 false 禁用。启用后, Chrome 崩溃或断连时
        /// 自动使用指数退避策略重试连接, 恢复后从断点续传。
        #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
        auto_recovery: bool,
        /// 自动恢复最大重试次数
        ///
        /// 检测到断连后最多重试此次数 (默认 10)。
        /// 每次重试间隔按指数退避 (2^n 秒, 上限 60s)。
        #[arg(long, default_value = "10")]
        recovery_retries: u32,
        /// Phase 1 超时: 等待新 AI 消息出现 (秒, 默认 30)
        ///
        /// 24h 可靠性强化: 流式响应检测的第一阶段超时。
        /// 超过后判定为新消息未出现。
        ///
        /// 端到端验证 (Session 67): 默认值从 15s 提升到 30s,
        /// 因为 Z.ai 处理复杂 prompt (2000+ 字符) 时新消息出现可能 >15s。
        #[arg(long, default_value = "30")]
        phase1_timeout: u64,
        /// Phase 2 超时: 等待实际回答内容出现 (秒, 默认 60)
        ///
        /// 24h 可靠性强化: 流式响应检测的第二阶段超时。
        /// 超过后直接进入稳定性检测。
        #[arg(long, default_value = "60")]
        phase2_timeout: u64,
        /// Phase 3 超时: 等待文本稳定 (秒, 默认 45)
        ///
        /// 24h 可靠性强化: 流式响应检测的第三阶段超时。
        /// 超过后读取当前文本并返回。
        #[arg(long, default_value = "45")]
        phase3_timeout: u64,
        /// 卡死检测阈值 (秒, 默认 180, 0=禁用)
        ///
        /// 24h 可靠性强化: 如果 Phase 1 中连续 N 秒无任何变化
        /// (无新消息、无文本变化), 判定为页面卡死, 返回错误触发自动恢复。
        #[arg(long, default_value = "180")]
        stuck_threshold: u64,
        /// 使用指定索引的标签页 (0=第一个, 默认=0)
        ///
        /// 多网站支持: 可指定不同标签页使用不同的 AI 网站
        /// (如 tab 0 = Z.ai, tab 1 = DeepSeek)
        #[arg(long, default_value = "0")]
        tab: usize,
        /// 启用多网站自动切换 (24h 可靠性: 主网站不健康时自动切换到备用标签页)
        ///
        /// 启用后, 在发送消息前自动检查当前网站的健康状态 (登录/限流/维护),
        /// 不健康时自动切换到其他可用标签页。需要打开多个聊天标签页。
        #[arg(long)]
        auto_failover: bool,
        /// 健康检查间隔 (每 N 轮对话检查一次网站健康, 默认 5)
        ///
        /// 0 = 每次发送消息都检查。仅在 --auto-failover 启用时有效。
        #[arg(long, default_value = "5")]
        health_check_interval: usize,
        /// 多网站切换最大连续失败次数 (超过后放弃切换, 默认 3)
        ///
        /// 仅在 --auto-failover 启用时有效。
        #[arg(long, default_value = "3")]
        failover_max_failures: usize,
        /// 多网站切换冷却时间 (秒, 避免频繁切换, 默认 30)
        ///
        /// 仅在 --auto-failover 启用时有效。
        #[arg(long, default_value = "30")]
        failover_cooldown: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "forge=info,warn".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::List => {
            let mut manager = BrowserManager::new(cli.port);
            match manager.discover_and_connect().await {
                Ok(_) => {
                    println!("\n找到 {} 个聊天标签页:", manager.tabs.len());
                    for (i, tab) in manager.tabs.iter().enumerate() {
                        println!("  [{}] {} ({}) [{}]", i, tab.title, tab.url, tab.site_type);
                    }
                }
                Err(e) => {
                    error!("{}", e);
                    println!("\n{}", e);
                    println!("\n请按以下步骤操作:");
                    println!("  1. 关闭所有 Chrome");
                    println!(
                        "  2. 启动: {} --remote-debugging-port={} --user-data-dir={}",
                        chrome_path(),
                        cli.port,
                        chrome_user_data_dir()
                    );
                    println!("  3. 在 Chrome 中打开聊天网页并登录");
                    println!("  4. 再次运行: forge list");
                    println!("\n提示: 使用持久化 user-data-dir ({}) 保存登录 cookie, 重启后无需重新登录。", chrome_user_data_dir());
                }
            }
        }

        Commands::Health { tab } => {
            let mut manager = BrowserManager::new(cli.port);
            manager.discover_and_connect().await?;

            if manager.tabs.is_empty() {
                println!("没有发现聊天标签页");
                return Ok(());
            }

            let tabs_to_check: Vec<usize> = match tab {
                Some(idx) => {
                    if idx >= manager.tabs.len() {
                        anyhow::bail!(
                            "标签页索引 {} 超出范围 (共 {} 个标签页)",
                            idx,
                            manager.tabs.len()
                        );
                    }
                    vec![idx]
                }
                None => (0..manager.tabs.len()).collect(),
            };

            println!("\n检查 {} 个标签页的健康状态...", tabs_to_check.len());
            println!("──────────────────────────────────────────────");

            for &idx in &tabs_to_check {
                let chat_tab = &manager.tabs[idx];
                print!(
                    "  [{}] {} ({}) [{}] ... ",
                    idx, chat_tab.title, chat_tab.url, chat_tab.site_type
                );

                match SiteHealthChecker::check(&chat_tab.session, chat_tab.site_type).await {
                    Ok(result) => {
                        if result.is_healthy() {
                            println!("✅ 健康");
                        } else {
                            println!("⚠ {}", result.status);
                            if let Some(msg) = &result.message {
                                println!("     详情: {}", msg);
                            }
                            if let Some(url) = &result.current_url {
                                println!("     当前 URL: {}", url);
                            }
                        }
                    }
                    Err(e) => {
                        println!("❌ 检查失败: {}", e);
                    }
                }
            }
            println!("──────────────────────────────────────────────");
        }

        Commands::Open { url } => {
            println!("正在打开: {}", url);
            match cdp::create_tab(cli.port, &url).await {
                Ok(tab) => println!("✅ 已打开: {} ({})", tab.title, tab.url),
                Err(e) => error!("打开失败: {}", e),
            }
        }

        Commands::Ask {
            message,
            timeout,
            tab,
        } => {
            let mut manager = BrowserManager::new(cli.port);
            manager.discover_and_connect().await?;
            if tab >= manager.tabs.len() {
                anyhow::bail!(
                    "标签页索引 {} 超出范围 (共 {} 个标签页)",
                    tab,
                    manager.tabs.len()
                );
            }
            println!(
                "使用标签页 [{}]: {} ({})",
                tab, manager.tabs[tab].title, manager.tabs[tab].url
            );
            let chat_tab = &manager.tabs[tab];
            println!("发送: {}", message);
            match chat_tab.send_and_wait(&message, timeout).await {
                Ok(result) => {
                    println!("\n回复 ({:.1}s):", result.elapsed.as_secs_f64());
                    if result.timed_out {
                        warn!("⚠ 可能未完成");
                    }
                    println!("{}", sep());
                    println!("{}", result.text);
                    println!("{}", sep());
                }
                Err(e) => error!("失败: {}", e),
            }
        }

        Commands::Upload {
            files,
            message,
            tab,
            timeout,
        } => {
            let mut manager = BrowserManager::new(cli.port);
            manager.discover_and_connect().await?;
            if tab >= manager.tabs.len() {
                anyhow::bail!(
                    "标签页索引 {} 超出范围 (共 {} 个标签页)",
                    tab,
                    manager.tabs.len()
                );
            }

            let chat_tab = &manager.tabs[tab];
            println!(
                "使用标签页 [{}]: {} ({}) [{}]",
                tab, chat_tab.title, chat_tab.url, chat_tab.site_type
            );

            // 验证文件路径存在
            let valid_files: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
            for f in &valid_files {
                if !std::path::Path::new(f).exists() {
                    anyhow::bail!("文件不存在: {}", f);
                }
            }

            println!("📎 上传 {} 个文件...", valid_files.len());
            for (i, f) in valid_files.iter().enumerate() {
                println!("  [{}] {}", i + 1, f);
            }

            // 上传文件
            if let Err(e) = chat_tab.upload_files(&valid_files).await {
                error!("文件上传失败: {}", e);
                println!("❌ 文件上传失败: {}", e);
                return Ok(());
            }
            println!("✅ 文件上传完成");

            // 如果有分析指令, 上传后发送消息
            if let Some(msg) = message {
                println!("\n📨 发送分析指令: {}", msg);
                match chat_tab.send_and_wait(&msg, timeout).await {
                    Ok(result) => {
                        println!("\nAI 回复 ({:.1}s):", result.elapsed.as_secs_f64());
                        if result.timed_out {
                            warn!("⚠ 可能未完成");
                        }
                        println!("{}", sep());
                        println!("{}", result.text);
                        println!("{}", sep());
                    }
                    Err(e) => error!("发送消息失败: {}", e),
                }
            } else {
                println!("\n💡 提示: 使用 -m 参数指定分析指令, 如:");
                println!("  forge upload screenshot.png -m \"分析这个UI设计的优缺点\"");
            }
        }

        Commands::Generate {
            message,
            output,
            timeout,
            tab,
        } => {
            let mut manager = BrowserManager::new(cli.port);
            manager.discover_and_connect().await?;
            if tab >= manager.tabs.len() {
                anyhow::bail!(
                    "标签页索引 {} 超出范围 (共 {} 个标签页)",
                    tab,
                    manager.tabs.len()
                );
            }
            println!(
                "使用标签页 [{}]: {} ({}) [{}]",
                tab, manager.tabs[tab].title, manager.tabs[tab].url, manager.tabs[tab].site_type
            );
            let tab = &manager.tabs[tab];
            println!("指令: {}", message);
            match tab.send_and_wait(&message, timeout).await {
                Ok(result) => {
                    println!(
                        "AI 回复 ({:.1}s, {}字符)",
                        result.elapsed.as_secs_f64(),
                        result.text.len()
                    );
                    let files = extract_files(&result.text);
                    if files.is_empty() {
                        warn!("未找到代码文件");
                        println!("\n{}", result.text);
                    } else {
                        println!("\n提取 {} 个文件:", files.len());
                        for f in &files {
                            println!("  {} ({}字符)", f.path, f.content.len());
                        }
                        match package(&files, &output) {
                            Ok(_) => println!("\n✅ 已打包: {}", output.display()),
                            Err(e) => error!("打包失败: {}", e),
                        }
                    }
                }
                Err(e) => error!("失败: {}", e),
            }
        }

        Commands::Develop {
            message,
            workspace,
            max_rounds,
            timeout,
            tab,
        } => {
            let mut manager = BrowserManager::new(cli.port);
            manager.discover_and_connect().await?;
            if tab >= manager.tabs.len() {
                anyhow::bail!(
                    "标签页索引 {} 超出范围 (共 {} 个标签页)",
                    tab,
                    manager.tabs.len()
                );
            }
            println!(
                "使用标签页 [{}]: {} ({}) [{}]",
                tab, manager.tabs[tab].title, manager.tabs[tab].url, manager.tabs[tab].site_type
            );
            let tab = &manager.tabs[tab];

            let ws = Workspace::new(&workspace);
            ws.init()?;

            println!("══════════════════════════════════════════");
            println!("  TDD 开发循环 (最多 {} 轮)", max_rounds);
            println!("  工作区: {}", workspace.display());
            println!("══════════════════════════════════════════\n");

            let prompt = format!(
                "{}\n\n请用 ```file:路径``` 格式输出每个文件。必须包含 Cargo.toml 和 src/main.rs。",
                message
            );

            println!("▶ 请求 AI 生成代码...");
            let result = tab.send_and_wait(&prompt, timeout).await?;
            println!(
                "AI 回复 ({:.1}s, {}字符)",
                result.elapsed.as_secs_f64(),
                result.text.len()
            );

            let files = extract_files(&result.text);
            if files.is_empty() {
                error!("没有代码文件");
                println!("\n{}", result.text);
                return Ok(());
            }

            println!("\n提取 {} 个文件", files.len());
            ws.write_files(&files)?;
            println!("文件已写入: {}", workspace.display());

            println!("\n▶ cargo check...");
            let mut test_result = testrunner::cargo_check(&ws.root)?;
            let mut round = 1;

            while !test_result.success && round < max_rounds {
                round += 1;
                println!("\n▶ 修复第 {} 轮...", round);
                let feedback = test_result.to_feedback();
                let current_code = ws
                    .list_files()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|f| !f.starts_with("target/"))
                    .filter_map(|p| {
                        ws.read_file(&p)
                            .ok()
                            .map(|c| format!("```file:{}\n{}\n```\n", p, c))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let fix_prompt = format!(
                    "编译错误:\n{}\n当前代码:\n{}\n请修复。用 ```file:路径``` 格式输出完整文件。",
                    feedback, current_code
                );

                let result = tab.send_and_wait(&fix_prompt, timeout).await?;
                let files = extract_files(&result.text);
                if files.is_empty() {
                    continue;
                }
                ws.write_files(&files)?;

                println!("▶ 重新 cargo check...");
                test_result = testrunner::cargo_check(&ws.root)?;
            }

            if test_result.success {
                println!("\n✅ 编译成功! 运行 cargo test...");
                let test_result = testrunner::cargo_test(&ws.root)?;
                println!("{}", test_result.to_feedback());
            } else {
                error!("修复 {} 轮后仍失败", max_rounds);
                println!("{}", test_result.to_feedback());
            }

            println!("\n{}", ws.tree()?);
        }

        Commands::Run {
            goal,
            workspace,
            max_rounds,
            timeout,
            resume,
            llm_clarify,
            ollama_endpoint,
            ollama_model,
            requirement_file,
            interactive,
            multi_language,
            parallel,
            error_diagnosis,
            max_context_turns,
            steer_interval,
            loop_detection,
            dev_trace,
            slash_commands,
            auto_recovery,
            recovery_retries,
            phase1_timeout,
            phase2_timeout,
            phase3_timeout,
            stuck_threshold,
            tab,
            auto_failover,
            health_check_interval,
            failover_max_failures,
            failover_cooldown,
        } => {
            let mut manager = BrowserManager::new(cli.port);
            manager.discover_and_connect().await?;
            if tab >= manager.tabs.len() {
                anyhow::bail!(
                    "标签页索引 {} 超出范围 (共 {} 个标签页)",
                    tab,
                    manager.tabs.len()
                );
            }
            println!(
                "使用标签页 [{}]: {} ({}) [{}]",
                tab, manager.tabs[tab].title, manager.tabs[tab].url, manager.tabs[tab].site_type
            );

            // 设置超时配置 (24h 可靠性强化) — 对每个标签页按网站类型设置
            // DeepSeek 等网站可能需要更长的 Phase 1 超时 (响应较慢/需要登录)
            for chat_tab in manager.tabs.iter_mut() {
                chat_tab.timeout_config =
                    TimeoutConfig::new(phase1_timeout, phase2_timeout, phase3_timeout)
                        .with_stuck_threshold(stuck_threshold)
                        .for_site_type(chat_tab.site_type);
            }

            // 需求变更文件: 启动时加载初始变更
            if let Some(ref req_file) = requirement_file {
                println!("📋 需求变更文件: {}", req_file.display());
            }

            // 人工干预模式
            let interaction: Box<dyn HumanInteraction> = if interactive {
                println!("👋 启用人工干预模式 (CLI 交互式确认)");
                Box::new(CliInteraction::new())
            } else {
                Box::new(AutoApprove)
            };

            // 测试运行器: 默认使用多语言运行器 (自动检测 Rust/Python/Go/Node)
            if multi_language {
                println!("🌐 多语言支持已启用 (自动检测项目语言)");
            }
            let test_runner = MultiLanguageTestRunner::new();

            if parallel {
                println!("🔄 并行任务执行已启用 (TaskGraph 依赖分析 + 并行分组)");
            }

            // 智能错误诊断 (方向 F)
            let error_diagnoser: Option<Box<dyn ErrorDiagnoser>> = if error_diagnosis {
                if llm_clarify {
                    // LLM 增强模式: 复用 Ollama 客户端
                    let ollama = OllamaClient::new(&ollama_endpoint, &ollama_model);
                    println!("🔍 智能错误诊断已启用 (Hybrid: 启发式 + LLM + 历史学习)");
                    Some(Box::new(HybridErrorDiagnoser::new(ollama)))
                } else {
                    println!(
                        "🔍 智能错误诊断已启用 (启发式模式, 使用 --llm-clarify 启用 LLM 增强)"
                    );
                    Some(Box::new(HeuristicErrorDiagnoser::new()))
                }
            } else {
                None
            };

            // === 多网站自动切换 (--auto-failover) ===
            if auto_failover {
                if manager.tabs.len() < 2 {
                    warn!("⚠ --auto-failover 已启用但只有 1 个标签页, 无法切换");
                    println!("⚠ --auto-failover: 只有 1 个标签页, 无法切换 (建议打开多个聊天网页)");
                } else {
                    println!("🔄 多网站自动切换已启用 ({} 个标签页)", manager.tabs.len());
                    println!("   健康检查间隔: {} 轮", health_check_interval);
                    println!("   最大失败次数: {}", failover_max_failures);
                    println!("   切换冷却时间: {}s", failover_cooldown);
                }
            }

            // 构建聊天客户端: 单标签页或多网站自动切换
            if auto_failover && manager.tabs.len() >= 2 {
                // === 多网站自动切换模式 ===
                let tabs: Vec<&forge::browser::ChatTab> = manager.tabs.iter().collect();
                let failover_client = FailoverChatClient::new(
                    tabs,
                    tab,
                    failover_max_failures,
                    failover_cooldown,
                    health_check_interval,
                );

                if llm_clarify {
                    // LLM 增强模式
                    let ollama = OllamaClient::new(&ollama_endpoint, &ollama_model);
                    if ollama.is_available().await {
                        println!(
                            "✅ Ollama 可用 (模型: {}, 端点: {})",
                            ollama_model, ollama_endpoint
                        );
                    } else {
                        warn!("⚠ Ollama 不可用 ({}), 将回退到启发式检查", ollama_endpoint);
                    }
                    println!("🧠 启用 LLM 增强自主追问 (Hybrid)");
                    let checker = HybridClarificationChecker::new(ollama);

                    run_with_clarifier(
                        &failover_client,
                        checker,
                        test_runner,
                        &workspace,
                        &goal,
                        max_rounds,
                        timeout,
                        resume,
                        interaction,
                        parallel,
                        error_diagnoser,
                        max_context_turns,
                        steer_interval,
                        loop_detection,
                        dev_trace,
                        slash_commands,
                        auto_recovery,
                        cli.port,
                        recovery_retries,
                        requirement_file.as_deref(),
                    )
                    .await?;

                    // 打印性能统计
                    failover_client.print_stats().await;
                } else {
                    // 启发式模式
                    run_with_clarifier(
                        &failover_client,
                        HeuristicClarificationChecker::new(),
                        test_runner,
                        &workspace,
                        &goal,
                        max_rounds,
                        timeout,
                        resume,
                        interaction,
                        parallel,
                        error_diagnoser,
                        max_context_turns,
                        steer_interval,
                        loop_detection,
                        dev_trace,
                        slash_commands,
                        auto_recovery,
                        cli.port,
                        recovery_retries,
                        requirement_file.as_deref(),
                    )
                    .await?;

                    // 打印性能统计
                    failover_client.print_stats().await;
                }
            } else {
                // === 单标签页模式 (原有逻辑) ===
                let tab_ref = &manager.tabs[tab];

                if llm_clarify {
                    // LLM 增强模式
                    let ollama = OllamaClient::new(&ollama_endpoint, &ollama_model);
                    if ollama.is_available().await {
                        println!(
                            "✅ Ollama 可用 (模型: {}, 端点: {})",
                            ollama_model, ollama_endpoint
                        );
                    } else {
                        warn!("⚠ Ollama 不可用 ({}), 将回退到启发式检查", ollama_endpoint);
                    }
                    println!("🧠 启用 LLM 增强自主追问 (Hybrid)");
                    let checker = HybridClarificationChecker::new(ollama);

                    run_with_clarifier(
                        tab_ref,
                        checker,
                        test_runner,
                        &workspace,
                        &goal,
                        max_rounds,
                        timeout,
                        resume,
                        interaction,
                        parallel,
                        error_diagnoser,
                        max_context_turns,
                        steer_interval,
                        loop_detection,
                        dev_trace,
                        slash_commands,
                        auto_recovery,
                        cli.port,
                        recovery_retries,
                        requirement_file.as_deref(),
                    )
                    .await?;
                } else {
                    // 启发式模式
                    run_with_clarifier(
                        tab_ref,
                        HeuristicClarificationChecker::new(),
                        test_runner,
                        &workspace,
                        &goal,
                        max_rounds,
                        timeout,
                        resume,
                        interaction,
                        parallel,
                        error_diagnoser,
                        max_context_turns,
                        steer_interval,
                        loop_detection,
                        dev_trace,
                        slash_commands,
                        auto_recovery,
                        cli.port,
                        recovery_retries,
                        requirement_file.as_deref(),
                    )
                    .await?;
                }
            }
        }
    }

    Ok(())
}

fn sep() -> &'static str {
    "──────────────────────────────────────────────"
}

fn chrome_path() -> &'static str {
    if cfg!(target_os = "macos") {
        "/Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome"
    } else if cfg!(target_os = "linux") {
        "google-chrome"
    } else {
        "chrome.exe"
    }
}

/// Chrome 用户数据目录 — 持久化路径, 保存登录 cookie
///
/// 使用 `~/.forge-chrome` 而非 `/tmp/forge-chrome`,
/// 因为 `/tmp` 在系统重启后会被清除, 导致登录状态丢失。
/// `~/.forge-chrome` 是用户目录下的持久化路径, 重启后仍然保留 cookie。
fn chrome_user_data_dir() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        format!("{}/.forge-chrome", home.to_string_lossy())
    } else {
        // 回退: 如果 HOME 不可用, 使用 /tmp
        "/tmp/forge-chrome".to_string()
    }
}

/// 运行 Orchestrator 并打包最终产物 (泛型: 支持 Heuristic 和 Hybrid 检查器)
async fn run_and_package<C, T, E, Q>(
    orch: &mut Orchestrator<'_, C, T, E, Q>,
    workspace: &std::path::Path,
) -> Result<()>
where
    C: ChatClient,
    T: TestRunner,
    E: FileExtractor,
    Q: ClarificationChecker,
{
    orch.run().await?;

    // 最终打包
    let files: Vec<_> = orch
        .workspace
        .list_files()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.starts_with("target/"))
        .collect();

    if !files.is_empty() {
        let zip_path = workspace.join("forge-output.zip");
        let extracted: Vec<forge::extract::ExtractedFile> = files
            .iter()
            .filter_map(|f| {
                orch.workspace
                    .read_file(f)
                    .ok()
                    .map(|content| forge::extract::ExtractedFile {
                        path: f.clone(),
                        content,
                        language: String::new(),
                    })
            })
            .collect();
        match package(&extracted, &zip_path) {
            Ok(_) => println!("\n📦 最终产物已打包: {}", zip_path.display()),
            Err(e) => error!("打包失败: {}", e),
        }
    }
    Ok(())
}

/// 通用辅助函数: 创建 Orchestrator + 配置 + 运行 + 打包
///
/// 泛型参数:
/// - `C`: ChatClient (ChatTab 或 FailoverChatClient)
/// - `Q`: ClarificationChecker (HeuristicClarificationChecker 或 HybridClarificationChecker)
///
/// 通过此函数避免 llm_clarify / auto_failover 维度的代码重复。
#[allow(clippy::too_many_arguments)]
async fn run_with_clarifier<C, Q>(
    chat: &C,
    clarifier: Q,
    test_runner: MultiLanguageTestRunner,
    workspace: &std::path::Path,
    goal: &str,
    max_rounds: u32,
    timeout: u64,
    resume: bool,
    interaction: Box<dyn HumanInteraction>,
    parallel: bool,
    error_diagnoser: Option<Box<dyn ErrorDiagnoser>>,
    max_context_turns: usize,
    steer_interval: usize,
    loop_detection: usize,
    dev_trace: bool,
    slash_commands: bool,
    auto_recovery: bool,
    port: u16,
    recovery_retries: u32,
    requirement_file: Option<&std::path::Path>,
) -> Result<()>
where
    C: ChatClient,
    Q: ClarificationChecker,
{
    let workspace_str = workspace.to_str().unwrap_or("./forge-project");

    let mut orch = Orchestrator::new(
        chat,
        test_runner,
        DefaultExtractor,
        workspace_str,
        goal,
        max_rounds,
        timeout,
    )
    .with_resume(resume)
    .with_clarification(clarifier)
    .with_interaction(interaction)
    .with_parallel(parallel)
    .with_error_diagnosis_opt(error_diagnoser)
    .with_context_handoff(max_context_turns)
    .with_steer_reminder(steer_interval)
    .with_loop_detection(loop_detection)
    .with_dev_trace(dev_trace)
    .with_slash_commands(slash_commands);

    if auto_recovery {
        println!(
            "🔧 自动恢复已启用 (Chrome 断连后自动重连, 最多 {} 次重试)",
            recovery_retries
        );
        orch = orch.with_auto_recovery(port, recovery_retries);
    }

    // 共享 DevTraceWriter 给 ChatClient (FailoverChatClient 用于记录健康检查/切换事件)
    // ChatTab 等普通客户端的 set_dev_trace 是空操作 (DIP: 默认实现)
    if let Some(ref trace_writer) = orch.dev_trace {
        chat.set_dev_trace(trace_writer.clone());
    }

    // 加载需求变更文件
    if let Some(req_file) = requirement_file {
        orch.memory.load_changes_from_file(req_file);
    }

    run_and_package(&mut orch, workspace).await?;

    // 写入最终性能统计到 DevTrace (如 FailoverChatClient 的网站性能统计)
    chat.write_final_trace().await;

    Ok(())
}
