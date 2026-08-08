//! Workspace — 管理本地项目文件系统
//!
//! 将 AI 生成的代码写入本地目录,提供文件读写和项目结构管理
//!
//! 此外,提供文件版本快照机制:
//! - 每次 write_files 之前保存将被覆盖文件的快照
//! - cargo_check 通过后可标记 "known good" 快照点
//! - 修复失败时自动回滚到最近的 known good 版本

use crate::extract::ExtractedFile;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info};

/// 文件快照元信息
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    /// 快照序号
    pub id: u32,
    /// 目录名 (如 "0001_pre_write_20260807_120000")
    pub name: String,
    /// 标签 (如 "pre_write", "known_good")
    pub label: String,
    /// 时间戳
    pub timestamp: String,
    /// 快照中的文件数
    pub file_count: usize,
    /// 快照目录的完整路径
    pub path: PathBuf,
}

/// 项目工作区
pub struct Workspace {
    pub root: PathBuf,
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 初始化工作区目录 (含版本管理目录)
    pub fn init(&self) -> Result<()> {
        if !self.root.exists() {
            fs::create_dir_all(&self.root)
                .context(format!("创建目录失败: {}", self.root.display()))?;
            info!("已创建工作区: {}", self.root.display());
        }
        // 初始化版本管理目录
        self.init_versions()?;
        Ok(())
    }

    // ========================================================================
    //  基础文件操作
    // ========================================================================

    /// 将提取的文件写入工作区
    pub fn write_files(&self, files: &[ExtractedFile]) -> Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        for file in files {
            let path = self.root.join(&file.path);

            // 创建父目录
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .context(format!("创建目录失败: {}", parent.display()))?;
            }

            fs::write(&path, &file.content).context(format!("写入文件失败: {}", path.display()))?;

            debug!("已写入: {} ({} 字节)", path.display(), file.content.len());
            written.push(path);
        }
        info!("已写入 {} 个文件到 {}", written.len(), self.root.display());
        Ok(written)
    }

    /// 带版本快照的写入: 先保存将被覆盖的文件,再写入新文件
    ///
    /// 返回 (快照ID, 写入路径列表)。
    /// 如果没有已存在的文件需要快照, 快照ID为 None。
    pub fn write_files_with_snapshot(
        &self,
        files: &[ExtractedFile],
        snapshot_label: &str,
    ) -> Result<(Option<u32>, Vec<PathBuf>)> {
        // 找出已存在、即将被覆盖的文件
        let existing: Vec<String> = files
            .iter()
            .map(|f| f.path.clone())
            .filter(|p| self.root.join(p).exists())
            .collect();

        let snap_id = if !existing.is_empty() {
            let id = self
                .snapshot_files(&existing, snapshot_label)
                .context("保存写入前快照失败")?;
            info!(
                "已保存写入前快照 #{} ({} 个文件, 标签: {})",
                id,
                existing.len(),
                snapshot_label
            );
            Some(id)
        } else {
            None
        };

        let written = self.write_files(files)?;
        Ok((snap_id, written))
    }

    /// 读取文件内容
    pub fn read_file(&self, relative_path: &str) -> Result<String> {
        let path = self.root.join(relative_path);
        fs::read_to_string(&path).context(format!("读取文件失败: {}", path.display()))
    }

    /// 写入单个文件
    pub fn write_file(&self, relative_path: &str, content: &str) -> Result<()> {
        let path = self.root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        debug!("已写入: {}", path.display());
        Ok(())
    }

    /// 列出所有文件 (排除 target/ 和 .forge/)
    pub fn list_files(&self) -> Result<Vec<String>> {
        let mut files = Vec::new();
        if !self.root.exists() {
            return Ok(files);
        }
        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                // 跳过 target/ 和 .forge/ 目录
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy();
                    if name == "target" || name == ".forge" {
                        return false;
                    }
                }
                true
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&self.root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                files.push(rel);
            }
        }
        files.sort();
        Ok(files)
    }

    /// 检查是否是 Rust 项目 (有 Cargo.toml)
    pub fn is_rust_project(&self) -> bool {
        self.root.join("Cargo.toml").exists()
    }

    /// 获取文件树 (缩进格式)
    pub fn tree(&self) -> Result<String> {
        let files = self.list_files()?;
        if files.is_empty() {
            return Ok("(空)".to_string());
        }
        let mut result = String::new();
        for f in &files {
            result.push_str(&format!("  {}\n", f));
        }
        Ok(result)
    }

    // ========================================================================
    //  文件版本管理
    // ========================================================================

    /// 版本快照根目录: <workspace>/.forge/versions/
    fn versions_dir(&self) -> PathBuf {
        self.root.join(".forge").join("versions")
    }

    /// .forge 目录 (存放 known good 指针等)
    fn forge_dir(&self) -> PathBuf {
        self.root.join(".forge")
    }

    /// 初始化版本管理目录
    fn init_versions(&self) -> Result<()> {
        let dir = self.versions_dir();
        fs::create_dir_all(&dir).context(format!("创建版本目录失败: {}", dir.display()))?;
        Ok(())
    }

    /// 保存指定文件的当前内容为快照
    ///
    /// 只保存已存在的文件。用于 write_files 前保存"即将被覆盖的文件"。
    /// 返回快照序号。
    pub fn snapshot_files(&self, file_paths: &[String], label: &str) -> Result<u32> {
        let dir = self.versions_dir();
        fs::create_dir_all(&dir)?;

        let next_id = self.next_snapshot_id();
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let snap_name = format!("{:04}_{}_{}", next_id, label, timestamp);
        let snap_dir = dir.join(&snap_name);
        fs::create_dir_all(&snap_dir)?;

        let mut saved_files = Vec::new();
        for rel_path in file_paths {
            let src = self.root.join(rel_path);
            if src.exists() && src.is_file() {
                let dst = snap_dir.join(rel_path);
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&src, &dst)?;
                saved_files.push(rel_path.clone());
            }
        }

        // 写元数据
        let meta = serde_json::json!({
            "id": next_id,
            "label": label,
            "timestamp": timestamp.to_string(),
            "files": saved_files,
            "file_count": saved_files.len(),
        });
        fs::write(
            snap_dir.join("_meta.json"),
            serde_json::to_string_pretty(&meta)?,
        )?;

        debug!(
            "快照 #{}: {} ({} 个文件, 标签: {})",
            next_id,
            snap_name,
            saved_files.len(),
            label
        );

        Ok(next_id)
    }

    /// 保存当前所有项目文件为快照 (用于 known good)
    ///
    /// 排除 target/ 和 .forge/ 目录。返回快照序号。
    pub fn snapshot_all(&self, label: &str) -> Result<u32> {
        let all_files = self.list_files()?;
        if all_files.is_empty() {
            // 空项目也保存一个空快照 (标记用)
            let dir = self.versions_dir();
            fs::create_dir_all(&dir)?;
            let next_id = self.next_snapshot_id();
            let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let snap_name = format!("{:04}_{}_{}", next_id, label, timestamp);
            let snap_dir = dir.join(&snap_name);
            fs::create_dir_all(&snap_dir)?;
            let meta = serde_json::json!({
                "id": next_id,
                "label": label,
                "timestamp": timestamp.to_string(),
                "files": Vec::<String>::new(),
                "file_count": 0,
            });
            fs::write(
                snap_dir.join("_meta.json"),
                serde_json::to_string_pretty(&meta)?,
            )?;
            return Ok(next_id);
        }
        self.snapshot_files(&all_files, label)
    }

    /// 将指定快照标记为 known good
    ///
    /// 之后修复失败时可回滚到这个快照。
    pub fn save_known_good(&self, snapshot_id: u32) -> Result<()> {
        let dir = self.forge_dir();
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("known_good.txt"), snapshot_id.to_string())?;
        info!("已标记快照 #{} 为 known good", snapshot_id);
        Ok(())
    }

    /// 回滚到最近的 known good 快照
    ///
    /// 如果没有 known good, 返回 None。
    pub fn rollback_to_known_good(&self) -> Result<Option<u32>> {
        let kg_path = self.forge_dir().join("known_good.txt");
        if !kg_path.exists() {
            return Ok(None);
        }
        let id_str = fs::read_to_string(&kg_path).context("读取 known_good.txt 失败")?;
        let id: u32 = id_str
            .trim()
            .parse()
            .context("解析 known good 快照ID失败")?;
        self.rollback_to_snapshot(id)?;
        Ok(Some(id))
    }

    /// 回滚到指定快照
    ///
    /// 将快照中的文件恢复到工作区。快照中没有的文件保持不变。
    pub fn rollback_to_snapshot(&self, snapshot_id: u32) -> Result<()> {
        let snapshots = self.list_snapshots();
        let snap = snapshots
            .iter()
            .find(|s| s.id == snapshot_id)
            .ok_or_else(|| anyhow::anyhow!("快照 #{} 不存在", snapshot_id))?;

        let snap_dir = &snap.path;
        let meta_path = snap_dir.join("_meta.json");
        let meta_str = fs::read_to_string(&meta_path).context("读取快照元数据失败")?;
        let meta: serde_json::Value = serde_json::from_str(&meta_str)?;
        let files: Vec<String> = meta
            .get("files")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let mut restored = 0;
        for rel_path in &files {
            let src = snap_dir.join(rel_path);
            let dst = self.root.join(rel_path);
            if src.exists() && src.is_file() {
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&src, &dst)?;
                restored += 1;
            }
        }

        info!(
            "已回滚到快照 #{} ({}): 恢复 {} 个文件",
            snapshot_id, snap.label, restored
        );

        Ok(())
    }

    /// 列出所有快照 (按序号排序)
    pub fn list_snapshots(&self) -> Vec<SnapshotInfo> {
        let dir = self.versions_dir();
        if !dir.exists() {
            return vec![];
        }

        let mut snapshots = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return vec![],
        };

        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                let id = name
                    .split('_')
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);

                // 读取元数据
                let meta_path = entry.path().join("_meta.json");
                let (label, timestamp, file_count) = if meta_path.exists() {
                    match fs::read_to_string(&meta_path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    {
                        Some(v) => (
                            v.get("label")
                                .and_then(|l| l.as_str())
                                .unwrap_or("")
                                .to_string(),
                            v.get("timestamp")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string(),
                            v.get("file_count").and_then(|c| c.as_u64()).unwrap_or(0) as usize,
                        ),
                        None => (String::new(), String::new(), 0),
                    }
                } else {
                    (String::new(), String::new(), 0)
                };

                snapshots.push(SnapshotInfo {
                    id,
                    name,
                    label,
                    timestamp,
                    file_count,
                    path: entry.path(),
                });
            }
        }

        snapshots.sort_by_key(|s| s.id);
        snapshots
    }

    /// 计算下一个快照序号
    fn next_snapshot_id(&self) -> u32 {
        self.list_snapshots()
            .iter()
            .map(|s| s.id)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// 获取最近的 known good 快照ID
    pub fn get_known_good_id(&self) -> Option<u32> {
        let kg_path = self.forge_dir().join("known_good.txt");
        if !kg_path.exists() {
            return None;
        }
        fs::read_to_string(&kg_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
    }

    /// 清除 known good 标记 (用于新任务开始时重置)
    pub fn clear_known_good(&self) -> Result<()> {
        let kg_path = self.forge_dir().join("known_good.txt");
        if kg_path.exists() {
            fs::remove_file(&kg_path)?;
            debug!("已清除 known good 标记");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractedFile;
    use tempfile::tempdir;

    /// 创建临时工作区并初始化
    fn make_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        ws.init().unwrap();
        (dir, ws)
    }

    /// 创建带 3 个文件的临时工作区
    fn make_ws_with_files() -> (tempfile::TempDir, Workspace) {
        let (dir, ws) = make_ws();
        ws.write_file("src/main.rs", "fn main() {}").unwrap();
        ws.write_file("Cargo.toml", "[package]\nname = \"test\"")
            .unwrap();
        ws.write_file("src/lib.rs", "pub fn hello() {}").unwrap();
        (dir, ws)
    }

    fn ef(path: &str, content: &str) -> ExtractedFile {
        ExtractedFile {
            path: path.to_string(),
            content: content.to_string(),
            language: String::new(),
        }
    }

    // ===== 基础文件操作 =====

    #[test]
    fn test_init_creates_versions_dir() {
        let (_dir, ws) = make_ws();
        assert!(ws.versions_dir().exists(), ".forge/versions/ 应存在");
    }

    #[test]
    fn test_write_and_read_file() {
        let (_dir, ws) = make_ws();
        ws.write_file("src/main.rs", "fn main() {}").unwrap();
        assert_eq!(ws.read_file("src/main.rs").unwrap(), "fn main() {}");
    }

    #[test]
    fn test_list_files_excludes_forge_and_target() {
        let (_dir, ws) = make_ws_with_files();
        ws.write_file("target/debug/output", "binary").unwrap();
        let files = ws.list_files().unwrap();
        assert!(files.iter().all(|f| !f.starts_with("target/")));
        assert!(files.iter().all(|f| !f.starts_with(".forge/")));
        assert!(files.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn test_is_rust_project() {
        let (_dir, ws) = make_ws_with_files();
        assert!(ws.is_rust_project());
    }

    #[test]
    fn test_tree_output() {
        let (_dir, ws) = make_ws_with_files();
        let tree = ws.tree().unwrap();
        assert!(tree.contains("src/main.rs"));
        assert!(tree.contains("Cargo.toml"));
    }

    // ===== 版本快照 =====

    #[test]
    fn test_snapshot_files_saves_content() {
        let (_dir, ws) = make_ws_with_files();
        let id = ws.snapshot_files(&["src/main.rs".into()], "test").unwrap();
        assert_eq!(id, 1);

        let snaps = ws.list_snapshots();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].id, 1);
        assert_eq!(snaps[0].label, "test");
        assert_eq!(snaps[0].file_count, 1);

        let saved = std::fs::read_to_string(snaps[0].path.join("src/main.rs")).unwrap();
        assert_eq!(saved, "fn main() {}");
    }

    #[test]
    fn test_snapshot_files_skips_nonexistent() {
        let (_dir, ws) = make_ws_with_files();
        ws.snapshot_files(&["src/main.rs".into(), "nope.rs".into()], "t")
            .unwrap();
        let snaps = ws.list_snapshots();
        assert_eq!(snaps[0].file_count, 1, "不存在的文件被跳过");
    }

    #[test]
    fn test_snapshot_all() {
        let (_dir, ws) = make_ws_with_files();
        ws.snapshot_all("full").unwrap();
        let snaps = ws.list_snapshots();
        assert_eq!(snaps[0].file_count, 3, "全量快照应包含 3 个文件");
    }

    #[test]
    fn test_snapshot_all_empty() {
        let (_dir, ws) = make_ws();
        ws.snapshot_all("empty").unwrap();
        let snaps = ws.list_snapshots();
        assert_eq!(snaps[0].file_count, 0);
    }

    #[test]
    fn test_list_snapshots_ordered() {
        let (_dir, ws) = make_ws_with_files();
        ws.snapshot_all("first").unwrap();
        ws.snapshot_all("second").unwrap();
        ws.snapshot_all("third").unwrap();
        let snaps = ws.list_snapshots();
        assert_eq!(snaps.len(), 3);
        assert_eq!(snaps[0].id, 1);
        assert_eq!(snaps[2].id, 3);
        assert_eq!(snaps[0].label, "first");
        assert_eq!(snaps[2].label, "third");
    }

    #[test]
    fn test_list_snapshots_empty() {
        let (_dir, ws) = make_ws();
        assert!(ws.list_snapshots().is_empty());
    }

    #[test]
    fn test_next_snapshot_id_increments() {
        let (_dir, ws) = make_ws_with_files();
        assert_eq!(ws.next_snapshot_id(), 1);
        ws.snapshot_all("a").unwrap();
        assert_eq!(ws.next_snapshot_id(), 2);
    }

    // ===== Known Good =====

    #[test]
    fn test_save_and_get_known_good() {
        let (_dir, ws) = make_ws_with_files();
        let id = ws.snapshot_all("kg").unwrap();
        ws.save_known_good(id).unwrap();
        assert_eq!(ws.get_known_good_id(), Some(id));
    }

    #[test]
    fn test_get_known_good_none() {
        let (_dir, ws) = make_ws();
        assert_eq!(ws.get_known_good_id(), None);
    }

    #[test]
    fn test_clear_known_good() {
        let (_dir, ws) = make_ws_with_files();
        let id = ws.snapshot_all("kg").unwrap();
        ws.save_known_good(id).unwrap();
        assert!(ws.get_known_good_id().is_some());
        ws.clear_known_good().unwrap();
        assert!(ws.get_known_good_id().is_none());
    }

    // ===== 回滚 =====

    #[test]
    fn test_rollback_to_snapshot() {
        let (_dir, ws) = make_ws_with_files();
        let sid = ws.snapshot_all("orig").unwrap();
        ws.write_file("src/main.rs", "modified").unwrap();
        assert_eq!(ws.read_file("src/main.rs").unwrap(), "modified");
        ws.rollback_to_snapshot(sid).unwrap();
        assert_eq!(ws.read_file("src/main.rs").unwrap(), "fn main() {}");
    }

    #[test]
    fn test_rollback_nonexistent_snapshot() {
        let (_dir, ws) = make_ws();
        assert!(ws.rollback_to_snapshot(999).is_err());
    }

    #[test]
    fn test_rollback_to_known_good() {
        let (_dir, ws) = make_ws_with_files();
        let gid = ws.snapshot_all("kg").unwrap();
        ws.save_known_good(gid).unwrap();
        ws.write_file("src/main.rs", "broken {{{").unwrap();
        let r = ws.rollback_to_known_good().unwrap();
        assert_eq!(r, Some(gid));
        assert_eq!(ws.read_file("src/main.rs").unwrap(), "fn main() {}");
    }

    #[test]
    fn test_rollback_to_known_good_none() {
        let (_dir, ws) = make_ws_with_files();
        assert_eq!(ws.rollback_to_known_good().unwrap(), None);
    }

    #[test]
    fn test_rollback_restores_deleted_file() {
        let (_dir, ws) = make_ws_with_files();
        let sid = ws.snapshot_all("before_del").unwrap();
        std::fs::remove_file(ws.root.join("src/lib.rs")).unwrap();
        assert!(ws.read_file("src/lib.rs").is_err());
        ws.rollback_to_snapshot(sid).unwrap();
        assert_eq!(ws.read_file("src/lib.rs").unwrap(), "pub fn hello() {}");
    }

    // ===== write_files_with_snapshot =====

    #[test]
    fn test_write_files_with_snapshot_existing() {
        let (_dir, ws) = make_ws_with_files();
        let files = vec![
            ef("src/main.rs", "fn main() { new }"),
            ef("src/new.rs", "pub fn new() {}"),
        ];
        let (sid, written) = ws.write_files_with_snapshot(&files, "pw").unwrap();
        assert!(sid.is_some(), "覆盖已有文件时应保存快照");
        assert_eq!(written.len(), 2);

        // 快照保存了原始内容
        let snaps = ws.list_snapshots();
        let saved = std::fs::read_to_string(snaps[0].path.join("src/main.rs")).unwrap();
        assert_eq!(saved, "fn main() {}", "快照应保存原始内容");

        // 新内容已写入
        assert_eq!(ws.read_file("src/main.rs").unwrap(), "fn main() { new }");
    }

    #[test]
    fn test_write_files_with_snapshot_new_only() {
        let (_dir, ws) = make_ws();
        let files = vec![
            ef("src/main.rs", "fn main() {}"),
            ef("Cargo.toml", "[package]"),
        ];
        let (sid, written) = ws.write_files_with_snapshot(&files, "pw").unwrap();
        assert!(sid.is_none(), "全是新文件，无需快照");
        assert_eq!(written.len(), 2);
    }

    #[test]
    fn test_write_files_basic() {
        let (_dir, ws) = make_ws();
        let files = vec![
            ef("src/main.rs", "fn main() {}"),
            ef("Cargo.toml", "[package]"),
        ];
        let written = ws.write_files(&files).unwrap();
        assert_eq!(written.len(), 2);
        assert!(ws.root.join("src/main.rs").exists());
    }

    // ===== 完整工作流 =====

    #[test]
    fn test_full_workflow_snapshot_rollback() {
        let (_dir, ws) = make_ws_with_files();

        // 1. known good
        let gid = ws.snapshot_all("kg").unwrap();
        ws.save_known_good(gid).unwrap();

        // 2. AI 破坏文件
        let files = vec![ef("src/main.rs", "broken {{{"), ef("src/extra.rs", "extra")];
        ws.write_files_with_snapshot(&files, "pw_a2").unwrap();

        // 3. 回滚
        let rb = ws.rollback_to_known_good().unwrap();
        assert_eq!(rb, Some(gid));

        // 4. 验证恢复
        assert_eq!(ws.read_file("src/main.rs").unwrap(), "fn main() {}");
        assert_eq!(ws.read_file("src/lib.rs").unwrap(), "pub fn hello() {}");
    }

    #[test]
    fn test_multiple_known_good_progression() {
        let (_dir, ws) = make_ws_with_files();

        let id1 = ws.snapshot_all("kg1").unwrap();
        ws.save_known_good(id1).unwrap();

        ws.write_file("src/main.rs", "v2").unwrap();
        let id2 = ws.snapshot_all("kg2").unwrap();
        ws.save_known_good(id2).unwrap();

        ws.write_file("src/main.rs", "broken").unwrap();
        ws.rollback_to_known_good().unwrap();
        assert_eq!(
            ws.read_file("src/main.rs").unwrap(),
            "v2",
            "回滚到最新 known good"
        );
    }
}
