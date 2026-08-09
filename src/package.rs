//! 打包项目为 zip

use crate::extract::ExtractedFile;
use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use std::io::Write;
use tracing::info;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[derive(Serialize)]
struct ProjectMetadata {
    generated_at: String,
    total_files: usize,
    total_chars: usize,
    files: Vec<FileMeta>,
}

#[derive(Serialize)]
struct FileMeta {
    path: String,
    size: usize,
    language: String,
}

/// 将提取的文件打包成 zip
pub fn package(files: &[ExtractedFile], output_path: &std::path::Path) -> Result<()> {
    let file = std::fs::File::create(output_path)?;
    let mut zip = ZipWriter::new(file);

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    let mut total_chars = 0;
    let mut file_metas = Vec::new();

    // 写入所有文件
    for f in files {
        // 确保 path 不以 / 开头
        let path = f.path.trim_start_matches('/');
        zip.start_file(path, options)?;
        zip.write_all(f.content.as_bytes())?;

        total_chars += f.content.len();
        file_metas.push(FileMeta {
            path: path.to_string(),
            size: f.content.len(),
            language: f.language.clone(),
        });
    }

    // 生成元数据
    let metadata = ProjectMetadata {
        generated_at: Utc::now().to_rfc3339(),
        total_files: files.len(),
        total_chars,
        files: file_metas,
    };

    zip.start_file(".forge-metadata.json", options)?;
    zip.write_all(serde_json::to_string_pretty(&metadata)?.as_bytes())?;

    zip.finish()?;

    info!(
        "项目已打包: {} ({} 个文件, {} 字符)",
        output_path.display(),
        files.len(),
        total_chars
    );

    Ok(())
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::tempdir;
    use zip::ZipArchive;

    fn make_file(path: &str, content: &str, language: &str) -> ExtractedFile {
        ExtractedFile {
            path: path.to_string(),
            content: content.to_string(),
            language: language.to_string(),
        }
    }

    /// 从 ZIP 中读取所有文件条目 (名称 → 内容)
    fn read_zip(path: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            entries.push((name, buf));
        }
        entries
    }

    #[test]
    fn test_package_single_file() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("output.zip");
        let files = vec![make_file("src/main.rs", "fn main() {}", "rust")];

        package(&files, &zip_path).unwrap();

        assert!(zip_path.exists());
        let entries = read_zip(&zip_path);
        assert!(entries.iter().any(|(n, _)| n == "src/main.rs"));
        assert!(entries.iter().any(|(n, _)| n == ".forge-metadata.json"));
    }

    #[test]
    fn test_package_multiple_files() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("multi.zip");
        let files = vec![
            make_file("src/main.rs", "fn main() {}", "rust"),
            make_file("src/lib.rs", "pub fn hello() {}", "rust"),
            make_file("Cargo.toml", "[package]\nname = \"test\"", "toml"),
        ];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        assert_eq!(entries.len(), 4); // 3 files + metadata
        assert!(entries.iter().any(|(n, _)| n == "src/main.rs"));
        assert!(entries.iter().any(|(n, _)| n == "src/lib.rs"));
        assert!(entries.iter().any(|(n, _)| n == "Cargo.toml"));
    }

    #[test]
    fn test_package_file_content_preserved() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("content.zip");
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        let files = vec![make_file("src/main.rs", content, "rust")];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        let main_entry = entries.iter().find(|(n, _)| n == "src/main.rs").unwrap();
        assert_eq!(String::from_utf8_lossy(&main_entry.1), content);
    }

    #[test]
    fn test_package_strips_leading_slash() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("slash.zip");
        let files = vec![make_file("/src/main.rs", "fn main() {}", "rust")];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        // 应该是 "src/main.rs" 而不是 "/src/main.rs"
        assert!(entries.iter().any(|(n, _)| n == "src/main.rs"));
        assert!(!entries.iter().any(|(n, _)| n.starts_with("/")));
    }

    #[test]
    fn test_package_empty_file_list() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("empty.zip");
        let files: Vec<ExtractedFile> = vec![];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        assert_eq!(entries.len(), 1); // 只有 metadata
        assert!(entries.iter().any(|(n, _)| n == ".forge-metadata.json"));
    }

    #[test]
    fn test_package_metadata_content() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("meta.zip");
        let files = vec![
            make_file("src/main.rs", "fn main() {}", "rust"),
            make_file("Cargo.toml", "[package]", "toml"),
        ];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        let meta_entry = entries
            .iter()
            .find(|(n, _)| n == ".forge-metadata.json")
            .unwrap();
        let meta: serde_json::Value = serde_json::from_slice(&meta_entry.1).unwrap();
        assert_eq!(meta["total_files"], 2);
        assert!(meta["total_chars"].as_u64().unwrap() > 0);
        assert!(meta["generated_at"].as_str().unwrap().contains("T")); // RFC3339
        let files_arr = meta["files"].as_array().unwrap();
        assert_eq!(files_arr.len(), 2);
    }

    #[test]
    fn test_package_unicode_content() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("unicode.zip");
        let content = "// 这是中文注释\nfn main() {\n    println!(\"你好世界\");\n}\n";
        let files = vec![make_file("src/main.rs", content, "rust")];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        let main_entry = entries.iter().find(|(n, _)| n == "src/main.rs").unwrap();
        assert_eq!(String::from_utf8_lossy(&main_entry.1), content);
    }

    #[test]
    fn test_package_metadata_file_sizes() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("sizes.zip");
        let content_a = "fn main() {}";
        let content_b = "pub fn hello() {}";
        let files = vec![
            make_file("a.rs", content_a, "rust"),
            make_file("b.rs", content_b, "rust"),
        ];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        let meta_entry = entries
            .iter()
            .find(|(n, _)| n == ".forge-metadata.json")
            .unwrap();
        let meta: serde_json::Value = serde_json::from_slice(&meta_entry.1).unwrap();
        let files_arr = meta["files"].as_array().unwrap();
        assert_eq!(files_arr[0]["size"], content_a.len());
        assert_eq!(files_arr[1]["size"], content_b.len());
        assert_eq!(
            meta["total_chars"].as_u64().unwrap(),
            (content_a.len() + content_b.len()) as u64
        );
    }

    #[test]
    fn test_package_creates_valid_zip() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("valid.zip");
        let files = vec![make_file("test.rs", "fn test() {}", "rust")];

        package(&files, &zip_path).unwrap();

        // 验证是有效的 ZIP 文件
        let file = std::fs::File::open(&zip_path).unwrap();
        let archive = ZipArchive::new(file);
        assert!(archive.is_ok());
        assert_eq!(archive.unwrap().len(), 2); // test.rs + metadata
    }

    #[test]
    fn test_package_file_language_preserved() {
        let dir = tempdir().unwrap();
        let zip_path = dir.path().join("lang.zip");
        let files = vec![
            make_file("main.rs", "fn main() {}", "rust"),
            make_file("main.py", "print('hello')", "python"),
        ];

        package(&files, &zip_path).unwrap();

        let entries = read_zip(&zip_path);
        let meta_entry = entries
            .iter()
            .find(|(n, _)| n == ".forge-metadata.json")
            .unwrap();
        let meta: serde_json::Value = serde_json::from_slice(&meta_entry.1).unwrap();
        let files_arr = meta["files"].as_array().unwrap();
        assert_eq!(files_arr[0]["language"], "rust");
        assert_eq!(files_arr[1]["language"], "python");
    }
}
