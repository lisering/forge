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
