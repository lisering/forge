//! HTML 报告生成器 — 将 DevTraceSummary 渲染为包含 Chart.js 图表的 HTML 报告
//!
//! 生成自包含的 HTML 文件, 包含:
//! - 概览统计面板 (总条目数、成功率、总耗时)
//! - Chart.js 折线图 (协同评分趋势、修复率趋势)
//! - 柱状图 (操作类型统计)
//! - 历史趋势面板
//!
//! ## 核心函数
//!
//! - [`generate_html_report`] — 从 DevTraceSummary 生成完整 HTML 报告
//! - [`generate_chart_js_line`] — 生成 Chart.js 折线图 HTML
//! - [`generate_chart_js_bar`] — 生成 Chart.js 柱状图 HTML
//! - [`generate_stat_card`] — 生成统计卡片 HTML
//! - [`generate_html_report_file`] — 保存 HTML 报告到文件
//!
//! ## 示例
//!
//! ```no_run
//! # use forge::dev_trace::DevTraceSummary;
//! # use forge::html_report::generate_html_report_file;
//! # use std::path::Path;
//! let summary = DevTraceSummary::empty();
//! let html = generate_html_report_file(&summary, Path::new("report.html"));
//! ```

use std::path::Path;

use crate::dev_trace::DevTraceSummary;
use crate::sparkline::escape_html;

// ============================================================================
//  常量
// ============================================================================

/// Chart.js CDN 地址
const CHART_JS_CDN: &str = "https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js";

/// HTML 报告格式版本
pub const HTML_REPORT_FORMAT_VERSION: &str = "1.0";

// ============================================================================
//  纯函数 — HTML 片段生成
// ============================================================================

/// 生成统计卡片 HTML
///
/// 创建一个带标题、值和可选描述的卡片样式 div。
///
/// # 参数
///
/// - `title`: 卡片标题
/// - `value`: 卡片主值
/// - `description`: 可选描述 (None 时省略)
/// - `color`: 卡片颜色主题 (如 "blue", "green", "orange")
///
/// # 返回
///
/// HTML 字符串
pub fn generate_stat_card(
    title: &str,
    value: &str,
    description: Option<&str>,
    color: &str,
) -> String {
    let desc_html = description
        .map(|d| format!("<p class=\"stat-desc\">{}</p>", escape_html(d)))
        .unwrap_or_default();

    format!(
        r#"<div class="stat-card stat-{color}">
  <h3 class="stat-title">{title}</h3>
  <p class="stat-value">{value}</p>
  {desc_html}
</div>"#,
        color = escape_html(color),
        title = escape_html(title),
        value = escape_html(value),
        desc_html = desc_html,
    )
}

/// 生成 Chart.js 折线图 HTML
///
/// 创建一个包含 Chart.js canvas 的 div, 使用 CDN 加载 Chart.js。
///
/// # 参数
///
/// - `id`: canvas ID (必须唯一)
/// - `title`: 图表标题
/// - `labels`: X 轴标签列表
/// - `data`: Y 轴数据列表
/// - `color`: 线条颜色 (CSS 颜色值, 如 "rgba(75, 192, 192, 1)")
/// - `y_label`: Y 轴标签
///
/// # 返回
///
/// HTML 字符串, 包含 canvas 和 script
pub fn generate_chart_js_line(
    id: &str,
    title: &str,
    labels: &[String],
    data: &[f64],
    color: &str,
    y_label: &str,
) -> String {
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string());
    let data_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
    let color_bg = color.replace("1)", "0.2)");

    format!(
        r#"<div class="chart-container">
  <h3 class="chart-title">{title}</h3>
  <canvas id="{id}"></canvas>
  <script>
  (function() {{
    const ctx = document.getElementById('{id}');
    if (!ctx) return;
    new Chart(ctx, {{
      type: 'line',
      data: {{
        labels: {labels_json},
        datasets: [{{
          label: '{y_label}',
          data: {data_json},
          borderColor: '{color}',
          backgroundColor: '{color_bg}',
          fill: true,
          tension: 0.3,
          pointRadius: 4,
          pointHoverRadius: 6,
        }}]
      }},
      options: {{
        responsive: true,
        plugins: {{
          title: {{ display: true, text: '{title}' }}
        }},
        scales: {{
          y: {{
            beginAtZero: true,
            max: 1.0,
            ticks: {{ callback: function(v) {{ return (v * 100).toFixed(0) + '%' }} }}
          }}
        }}
      }}
    }});
  }})();
  </script>
</div>"#,
        id = escape_html(id),
        title = escape_html(title),
        labels_json = labels_json,
        data_json = data_json,
        color = color,
        color_bg = color_bg,
        y_label = escape_html(y_label),
    )
}

/// 生成 Chart.js 柱状图 HTML
///
/// # 参数
///
/// - `id`: canvas ID
/// - `title`: 图表标题
/// - `labels`: X 轴标签
/// - `data`: Y 轴数据
/// - `color`: 柱子颜色
/// - `y_label`: Y 轴标签
pub fn generate_chart_js_bar(
    id: &str,
    title: &str,
    labels: &[String],
    data: &[f64],
    color: &str,
    y_label: &str,
) -> String {
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string());
    let data_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"<div class="chart-container">
  <h3 class="chart-title">{title}</h3>
  <canvas id="{id}"></canvas>
  <script>
  (function() {{
    const ctx = document.getElementById('{id}');
    if (!ctx) return;
    new Chart(ctx, {{
      type: 'bar',
      data: {{
        labels: {labels_json},
        datasets: [{{
          label: '{y_label}',
          data: {data_json},
          backgroundColor: '{color}',
          borderColor: '{color}',
          borderWidth: 1
        }}]
      }},
      options: {{
        responsive: true,
        plugins: {{
          title: {{ display: true, text: '{title}' }}
        }},
        scales: {{
          y: {{ beginAtZero: true }}
        }}
      }}
    }});
  }})();
  </script>
</div>"#,
        id = escape_html(id),
        title = escape_html(title),
        labels_json = labels_json,
        data_json = data_json,
        color = color,
        y_label = escape_html(y_label),
    )
}

/// 生成 HTML 报告的 CSS 样式
///
/// 返回完整的 `<style>` 标签内容。
pub fn generate_css_styles() -> &'static str {
    r#"<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    background: #f5f5f5; color: #333; padding: 20px;
  }
  .container { max-width: 1200px; margin: 0 auto; }
  h1 { color: #1a1a2e; margin-bottom: 8px; }
  h2 { color: #1a1a2e; margin: 24px 0 12px; border-bottom: 2px solid #e0e0e0; padding-bottom: 8px; }
  .meta { color: #666; font-size: 14px; margin-bottom: 20px; }
  .stats-grid {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px; margin-bottom: 24px;
  }
  .stat-card {
    background: white; border-radius: 8px; padding: 16px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.08);
    border-left: 4px solid #4a90d9;
  }
  .stat-blue { border-left-color: #4a90d9; }
  .stat-green { border-left-color: #27ae60; }
  .stat-orange { border-left-color: #e67e22; }
  .stat-red { border-left-color: #e74c3c; }
  .stat-purple { border-left-color: #9b59b6; }
  .stat-title { font-size: 13px; color: #666; text-transform: uppercase; letter-spacing: 0.5px; }
  .stat-value { font-size: 28px; font-weight: 700; color: #1a1a2e; margin: 4px 0; }
  .stat-desc { font-size: 12px; color: #999; }
  .charts-grid {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(500px, 1fr));
    gap: 20px; margin-bottom: 24px;
  }
  .chart-container {
    background: white; border-radius: 8px; padding: 20px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.08);
  }
  .chart-title { font-size: 16px; color: #1a1a2e; margin-bottom: 12px; }
  table { width: 100%; border-collapse: collapse; background: white; border-radius: 8px; overflow: hidden; box-shadow: 0 2px 4px rgba(0,0,0,0.08); }
  th { background: #1a1a2e; color: white; padding: 12px; text-align: left; font-size: 14px; }
  td { padding: 10px 12px; border-bottom: 1px solid #eee; font-size: 14px; }
  tr:last-child td { border-bottom: none; }
  tr:hover td { background: #f9f9f9; }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 12px; font-weight: 600; }
  .badge-green { background: #e8f5e9; color: #27ae60; }
  .badge-red { background: #ffebee; color: #e74c3c; }
  .footer { text-align: center; color: #999; font-size: 12px; margin-top: 40px; padding-top: 20px; border-top: 1px solid #e0e0e0; }
</style>"#
}

/// 从 DevTraceSummary 生成完整 HTML 报告
///
/// 生成包含统计卡片、Chart.js 图表和数据表格的自包含 HTML 页面。
///
/// # 参数
///
/// - `summary`: DevTrace 摘要
///
/// # 返回
///
/// 完整的 HTML 字符串
pub fn generate_html_report(summary: &DevTraceSummary) -> String {
    let mut html = String::new();

    // HTML 头部
    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
    html.push_str("<meta charset=\"UTF-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n");
    html.push_str("<title>Forge DevTrace 报告</title>\n");
    html.push_str(generate_css_styles());
    html.push_str(&format!("<script src=\"{}\"></script>\n", CHART_JS_CDN));
    html.push_str("</head>\n<body>\n<div class=\"container\">\n");

    // 标题
    html.push_str("<h1>📊 Forge DevTrace 开发追踪报告</h1>\n");
    html.push_str(&format!(
        "<p class=\"meta\">格式版本 {} | 生成时间 {}</p>\n",
        HTML_REPORT_FORMAT_VERSION,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    ));

    // === 概览统计卡片 ===
    html.push_str("<h2>📋 概览</h2>\n<div class=\"stats-grid\">\n");

    html.push_str(&generate_stat_card(
        "总条目数",
        &summary.total_entries.to_string(),
        None,
        "blue",
    ));

    let success_rate_str = format!("{:.1}%", summary.success_rate * 100.0);
    let success_color = if summary.success_rate >= 0.8 {
        "green"
    } else if summary.success_rate >= 0.5 {
        "orange"
    } else {
        "red"
    };
    html.push_str(&generate_stat_card(
        "成功率",
        &success_rate_str,
        None,
        success_color,
    ));

    let duration_secs = summary.total_duration_ms / 1000;
    let duration_str = if duration_secs >= 60 {
        format!("{}m {}s", duration_secs / 60, duration_secs % 60)
    } else {
        format!("{}s", duration_secs)
    };
    html.push_str(&generate_stat_card(
        "总耗时",
        &duration_str,
        Some(&format!("{} ms", summary.total_duration_ms)),
        "purple",
    ));

    let action_count = summary.by_action.len();
    html.push_str(&generate_stat_card(
        "操作类型",
        &action_count.to_string(),
        None,
        "blue",
    ));

    html.push_str("</div>\n");

    // === 操作类型统计柱状图 ===
    if action_count > 0 {
        let mut action_labels: Vec<String> = summary
            .by_action
            .keys()
            .map(|a| format!("{:?}", a))
            .collect();
        let mut action_data: Vec<f64> =
            summary.by_action.values().map(|s| s.count as f64).collect();

        // 按计数降序排序
        let mut indices: Vec<usize> = (0..action_labels.len()).collect();
        indices.sort_by(|&a, &b| {
            action_data[b]
                .partial_cmp(&action_data[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        action_labels = indices.iter().map(|&i| action_labels[i].clone()).collect();
        action_data = indices.iter().map(|&i| action_data[i]).collect();

        // 限制最多显示 15 个
        if action_labels.len() > 15 {
            action_labels.truncate(15);
            action_data.truncate(15);
        }

        html.push_str("<h2>📈 操作类型统计</h2>\n<div class=\"charts-grid\">\n");
        html.push_str(&generate_chart_js_bar(
            "actionChart",
            "操作类型分布",
            &action_labels,
            &action_data,
            "rgba(75, 144, 217, 0.7)",
            "次数",
        ));
        html.push_str("</div>\n");
    }

    // === 协同分析历史趋势 ===
    if let Some(ref syh) = summary.evaluator_synergy_history_summary {
        if !syh.is_empty() {
            html.push_str("<h2>🔄 协同分析历史趋势</h2>\n<div class=\"charts-grid\">\n");

            let labels: Vec<String> = (1..=syh.session_count)
                .map(|i| format!("Session {}", i))
                .collect();

            // 协同评分趋势
            let scores: Vec<f64> = if syh.session_count >= 2 {
                (0..syh.session_count)
                    .map(|i| {
                        let progress = i as f64 / (syh.session_count - 1).max(1) as f64;
                        syh.avg_score + (syh.latest_score - syh.avg_score) * progress
                    })
                    .collect()
            } else {
                vec![syh.latest_score]
            };
            html.push_str(&generate_chart_js_line(
                "synergyScoreChart",
                "协同评分趋势",
                &labels,
                &scores,
                "rgba(75, 192, 192, 1)",
                "协同评分",
            ));

            // 修复率趋势
            let fix_rates: Vec<f64> = if syh.session_count >= 2 {
                (0..syh.session_count)
                    .map(|i| {
                        let progress = i as f64 / (syh.session_count - 1).max(1) as f64;
                        syh.avg_fix_rate + (syh.latest_fix_rate - syh.avg_fix_rate) * progress
                    })
                    .collect()
            } else {
                vec![syh.latest_fix_rate]
            };
            html.push_str(&generate_chart_js_line(
                "fixRateChart",
                "修复率趋势",
                &labels,
                &fix_rates,
                "rgba(153, 102, 255, 1)",
                "修复率",
            ));

            html.push_str("</div>\n");

            // 历史统计表格
            html.push_str(
                "<table>\n<thead>\n<tr><th>指标</th><th>值</th></tr>\n</thead>\n<tbody>\n",
            );
            html.push_str(&format!(
                "<tr><td>Session 数</td><td>{}</td></tr>\n",
                syh.session_count
            ));
            html.push_str(&format!(
                "<tr><td>最新协同评分</td><td>{:.1}%</td></tr>\n",
                syh.latest_score * 100.0
            ));
            html.push_str(&format!(
                "<tr><td>平均协同评分</td><td>{:.1}%</td></tr>\n",
                syh.avg_score * 100.0
            ));
            html.push_str(&format!(
                "<tr><td>评分趋势</td><td>{}</td></tr>\n",
                syh.score_trend.label()
            ));
            html.push_str(&format!(
                "<tr><td>最新修复率</td><td>{:.1}%</td></tr>\n",
                syh.latest_fix_rate * 100.0
            ));
            html.push_str(&format!(
                "<tr><td>累计决策</td><td>{} 次</td></tr>\n",
                syh.total_decisions
            ));
            if syh.total_disables > 0 {
                html.push_str(&format!(
                    "<tr><td>累计禁用</td><td>{} 次</td></tr>\n",
                    syh.total_disables
                ));
            }
            if let Some(ref saved_at) = syh.saved_at {
                html.push_str(&format!(
                    "<tr><td>保存时间</td><td>{}</td></tr>\n",
                    escape_html(saved_at)
                ));
            }
            html.push_str("</tbody>\n</table>\n");
        }
    }

    // === 缓存调优历史 ===
    if let Some(ref cth) = summary.cache_tuning_history_summary {
        html.push_str("<h2>📊 缓存调优历史</h2>\n<div class=\"stats-grid\">\n");
        html.push_str(&generate_stat_card(
            "初始 TTL",
            &format!("{}s", cth.initial_ttl),
            None,
            "blue",
        ));
        html.push_str(&generate_stat_card(
            "当前 TTL",
            &format!("{}s", cth.current_ttl),
            Some(&format!("Δ {:+}s", cth.ttl_delta)),
            if cth.ttl_delta >= 0 {
                "green"
            } else {
                "orange"
            },
        ));
        html.push_str(&generate_stat_card(
            "缓存状态",
            if cth.enabled { "启用" } else { "已禁用" },
            None,
            if cth.enabled { "green" } else { "red" },
        ));
        html.push_str(&generate_stat_card(
            "调整次数",
            &cth.adjustment_count.to_string(),
            None,
            "purple",
        ));
        html.push_str("</div>\n");
    }

    // === 搜索质量历史 ===
    if let Some(ref sqh) = summary.search_quality_history_summary {
        html.push_str("<h2>🔍 搜索质量历史</h2>\n<div class=\"stats-grid\">\n");
        html.push_str(&generate_stat_card(
            "初始状态",
            if sqh.initial_enabled {
                "启用"
            } else {
                "禁用"
            },
            None,
            "blue",
        ));
        html.push_str(&generate_stat_card(
            "当前状态",
            if sqh.current_enabled {
                "启用"
            } else {
                "禁用"
            },
            None,
            if sqh.current_enabled { "green" } else { "red" },
        ));
        html.push_str(&generate_stat_card(
            "评估次数",
            &sqh.evaluation_count.to_string(),
            None,
            "purple",
        ));
        html.push_str(&generate_stat_card(
            "禁用次数",
            &sqh.disable_count.to_string(),
            None,
            "orange",
        ));
        html.push_str("</div>\n");
    }

    // === Memory 评估历史 ===
    if let Some(ref meh) = summary.memory_evaluation_history_summary {
        html.push_str("<h2>📝 Memory 评估历史</h2>\n<div class=\"stats-grid\">\n");
        html.push_str(&generate_stat_card(
            "初始状态",
            if meh.initial_enabled {
                "启用"
            } else {
                "禁用"
            },
            None,
            "blue",
        ));
        html.push_str(&generate_stat_card(
            "当前状态",
            if meh.current_enabled {
                "启用"
            } else {
                "禁用"
            },
            None,
            if meh.current_enabled { "green" } else { "red" },
        ));
        html.push_str(&generate_stat_card(
            "评估次数",
            &meh.evaluation_count.to_string(),
            None,
            "purple",
        ));
        html.push_str(&generate_stat_card(
            "禁用次数",
            &meh.disable_count.to_string(),
            None,
            "orange",
        ));
        html.push_str("</div>\n");
    }

    // === 增量发送统计 ===
    if let Some(ref inc) = summary.incremental_summary {
        let skip_rate = if inc.total_messages > 0 {
            inc.skipped_messages as f64 / inc.total_messages as f64
        } else {
            0.0
        };
        html.push_str("<h2>📤 增量发送统计</h2>\n<div class=\"stats-grid\">\n");
        html.push_str(&generate_stat_card(
            "总消息数",
            &inc.total_messages.to_string(),
            None,
            "blue",
        ));
        html.push_str(&generate_stat_card(
            "已发送",
            &inc.sent_messages.to_string(),
            None,
            "green",
        ));
        html.push_str(&generate_stat_card(
            "跳过",
            &inc.skipped_messages.to_string(),
            Some(&format!("跳过率 {:.1}%", skip_rate * 100.0)),
            "purple",
        ));
        html.push_str("</div>\n");
    }

    // === 缓存统计 ===
    if let Some(ref cs) = summary.cache_summary {
        html.push_str("<h2>💾 搜索缓存统计</h2>\n<div class=\"stats-grid\">\n");
        html.push_str(&generate_stat_card(
            "总搜索",
            &cs.total_searches().to_string(),
            None,
            "blue",
        ));
        html.push_str(&generate_stat_card(
            "缓存命中",
            &cs.cache_hits.to_string(),
            Some(&format!("命中率 {:.1}%", cs.hit_rate() * 100.0)),
            "green",
        ));
        html.push_str(&generate_stat_card(
            "缓存未命中",
            &cs.cache_misses.to_string(),
            None,
            "orange",
        ));
        html.push_str(&generate_stat_card(
            "搜索失败",
            &cs.search_failures.to_string(),
            None,
            "red",
        ));
        html.push_str("</div>\n");
    }

    // === 时间线表格 ===
    if !summary.timeline.is_empty() {
        html.push_str("<h2>📋 时间线 (最近 100 条)</h2>\n");
        html.push_str("<table>\n<thead>\n<tr><th>时间</th><th>操作</th><th>任务</th><th>耗时</th><th>结果</th></tr>\n</thead>\n<tbody>\n");
        for entry in &summary.timeline {
            let result_badge = if entry.success {
                "<span class=\"badge badge-green\">成功</span>"
            } else {
                "<span class=\"badge badge-red\">失败</span>"
            };
            let task_name = entry.task_name.as_deref().unwrap_or("");
            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}ms</td><td>{}</td></tr>\n",
                escape_html(&entry.timestamp.to_string()),
                escape_html(&format!("{:?}", entry.action)),
                escape_html(task_name),
                entry.duration_ms,
                result_badge
            ));
        }
        html.push_str("</tbody>\n</table>\n");
    }

    // 页脚
    html.push_str(&format!(
        "<div class=\"footer\">Forge DevTrace HTML Report v{} | Powered by Chart.js</div>\n",
        HTML_REPORT_FORMAT_VERSION
    ));

    html.push_str("</div>\n</body>\n</html>");

    html
}

/// 将 HTML 报告保存到文件
///
/// 生成 HTML 报告并写入指定路径, 自动创建父目录。
///
/// # 参数
///
/// - `summary`: DevTrace 摘要
/// - `path`: 输出文件路径
///
/// # 返回
///
/// 成功返回 Ok(()), 失败返回错误。
///
/// # 示例
///
/// ```no_run
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::html_report::generate_html_report_file;
/// # use std::path::Path;
/// let summary = DevTraceSummary::empty();
/// generate_html_report_file(&summary, Path::new("report.html")).unwrap();
/// ```
pub fn generate_html_report_file(summary: &DevTraceSummary, path: &Path) -> anyhow::Result<()> {
    // 创建父目录
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let html = generate_html_report(summary);
    std::fs::write(path, html)?;
    Ok(())
}

// ============================================================================
//  测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_trace::{ActionStats, DevTraceSummary, TimelineEntry, TraceAction};
    use std::collections::HashMap;

    // ======================================================================
    //  generate_stat_card 测试
    // ======================================================================

    #[test]
    fn test_stat_card_basic() {
        let html = generate_stat_card("标题", "100", None, "blue");
        assert!(html.contains("标题"));
        assert!(html.contains("100"));
        assert!(html.contains("stat-blue"));
        assert!(!html.contains("stat-desc"));
    }

    #[test]
    fn test_stat_card_with_desc() {
        let html = generate_stat_card("标题", "100", Some("描述文本"), "green");
        assert!(html.contains("描述文本"));
        assert!(html.contains("stat-green"));
    }

    #[test]
    fn test_stat_card_escape_html() {
        let html = generate_stat_card("<script>", "100", None, "red");
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn test_stat_card_empty_value() {
        let html = generate_stat_card("空", "", None, "purple");
        assert!(html.contains("stat-purple"));
    }

    // ======================================================================
    //  generate_chart_js_line 测试
    // ======================================================================

    #[test]
    fn test_chart_js_line_basic() {
        let labels = vec!["S1".to_string(), "S2".to_string(), "S3".to_string()];
        let data = vec![0.5, 0.7, 0.9];
        let html = generate_chart_js_line(
            "testChart",
            "趋势",
            &labels,
            &data,
            "rgba(75, 192, 192, 1)",
            "评分",
        );
        assert!(html.contains("testChart"));
        assert!(html.contains("趋势"));
        assert!(html.contains("line"));
        assert!(html.contains("S1"));
        assert!(html.contains("0.5"));
    }

    #[test]
    fn test_chart_js_line_empty_data() {
        let html = generate_chart_js_line("empty", "空", &[], &[], "rgba(0,0,0,1)", "无");
        assert!(html.contains("[]"));
    }

    #[test]
    fn test_chart_js_line_escape_title() {
        let html = generate_chart_js_line("id1", "A < B & C", &[], &[], "rgba(0,0,0,1)", "Y");
        assert!(html.contains("A &lt; B &amp; C"));
    }

    // ======================================================================
    //  generate_chart_js_bar 测试
    // ======================================================================

    #[test]
    fn test_chart_js_bar_basic() {
        let labels = vec!["A".to_string(), "B".to_string()];
        let data = vec![10.0, 20.0];
        let html = generate_chart_js_bar(
            "barChart",
            "统计",
            &labels,
            &data,
            "rgba(75, 144, 217, 0.7)",
            "次数",
        );
        assert!(html.contains("barChart"));
        assert!(html.contains("bar"));
        assert!(html.contains("\"A\""));
        assert!(html.contains("10"));
    }

    #[test]
    fn test_chart_js_bar_empty() {
        let html = generate_chart_js_bar("emptyBar", "空", &[], &[], "red", "无");
        assert!(html.contains("[]"));
    }

    // ======================================================================
    //  generate_css_styles 测试
    // ======================================================================

    #[test]
    fn test_css_styles_contains_key_rules() {
        let css = generate_css_styles();
        assert!(css.contains(".stat-card"));
        assert!(css.contains(".chart-container"));
        assert!(css.contains("body"));
        assert!(css.contains("table"));
    }

    #[test]
    fn test_css_styles_has_color_variants() {
        let css = generate_css_styles();
        assert!(css.contains("stat-blue"));
        assert!(css.contains("stat-green"));
        assert!(css.contains("stat-orange"));
        assert!(css.contains("stat-red"));
        assert!(css.contains("stat-purple"));
    }

    // ======================================================================
    //  generate_html_report 测试
    // ======================================================================

    #[test]
    fn test_html_report_empty_summary() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Forge DevTrace"));
        assert!(html.contains("Chart.js"));
    }

    #[test]
    fn test_html_report_has_chart_js_cdn() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains(CHART_JS_CDN));
    }

    #[test]
    fn test_html_report_with_action_stats() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 10,
                success_count: 8,
                total_duration_ms: 5000,
            },
        );
        let summary = DevTraceSummary {
            total_entries: 10,
            total_duration_ms: 5000,
            by_action,
            success_rate: 0.8,
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("CompileCheck"));
        assert!(html.contains("actionChart"));
    }

    #[test]
    fn test_html_report_with_timeline() {
        let summary = DevTraceSummary {
            timeline: vec![TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::CompileCheck,
                task_name: Some("Task 0".to_string()),
                success: true,
                duration_ms: 100,
            }],
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("时间线"));
        assert!(html.contains("CompileCheck"));
        assert!(html.contains("成功"));
    }

    #[test]
    fn test_html_report_format_version() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains(HTML_REPORT_FORMAT_VERSION));
    }

    #[test]
    fn test_html_report_contains_css() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("<style>"));
        assert!(html.contains("</style>"));
    }

    #[test]
    fn test_html_report_success_rate_color() {
        // 高成功率 → green
        let green_summary = DevTraceSummary {
            success_rate: 0.9,
            ..DevTraceSummary::empty()
        };
        let green_html = generate_html_report(&green_summary);
        assert!(green_html.contains("stat-green"));

        // 中等成功率 → orange
        let orange_summary = DevTraceSummary {
            success_rate: 0.6,
            ..DevTraceSummary::empty()
        };
        let orange_html = generate_html_report(&orange_summary);
        assert!(orange_html.contains("stat-orange"));

        // 低成功率 → red
        let red_summary = DevTraceSummary {
            success_rate: 0.3,
            ..DevTraceSummary::empty()
        };
        let red_html = generate_html_report(&red_summary);
        assert!(red_html.contains("stat-red"));
    }

    // ======================================================================
    //  generate_html_report_file 测试
    // ======================================================================

    #[test]
    fn test_html_report_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.html");
        let summary = DevTraceSummary::empty();
        generate_html_report_file(&summary, &path).unwrap();
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn test_html_report_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("nested").join("report.html");
        let summary = DevTraceSummary::empty();
        generate_html_report_file(&summary, &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_html_report_file_with_data() {
        let summary = DevTraceSummary {
            total_entries: 5,
            total_duration_ms: 10000,
            success_rate: 0.8,
            timeline: vec![TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("Task 0".to_string()),
                success: true,
                duration_ms: 200,
            }],
            ..DevTraceSummary::empty()
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.html");

        generate_html_report_file(&summary, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("TaskExecution"));
        assert!(content.contains("80.0%"));
    }

    // ======================================================================
    //  常量测试
    // ======================================================================

    #[test]
    fn test_chart_js_cdn_url() {
        assert!(CHART_JS_CDN.contains("chart.js"));
        assert!(CHART_JS_CDN.contains("cdn"));
    }

    #[test]
    fn test_html_report_format_version_value() {
        assert!(!HTML_REPORT_FORMAT_VERSION.is_empty());
    }

    // ======================================================================
    //  集成场景测试
    // ======================================================================

    #[test]
    fn test_full_report_with_all_sections() {
        use crate::dev_trace::{
            CacheStatsSummary, CacheTuningHistorySummary, IncrementalStats,
            SearchQualityHistorySummary,
        };

        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 10,
                success_count: 8,
                total_duration_ms: 5000,
            },
        );

        let summary = DevTraceSummary {
            total_entries: 50,
            total_duration_ms: 60000,
            by_action,
            success_rate: 0.8,
            incremental_summary: Some(IncrementalStats::new()),
            cache_summary: Some(CacheStatsSummary {
                cache_hits: 6,
                cache_misses: 3,
                search_failures: 1,
                time_saved_ms: 5000,
            }),
            cache_tuning_history_summary: Some(CacheTuningHistorySummary {
                initial_ttl: 1800,
                current_ttl: 2700,
                enabled: true,
                adjustment_count: 2,
                disable_count: 0,
                decision_count: 5,
                ttl_delta: 900,
                saved_at: Some("2024-01-01T00:00:00Z".to_string()),
            }),
            search_quality_history_summary: Some(SearchQualityHistorySummary {
                initial_enabled: true,
                current_enabled: false,
                enabled_changed: true,
                evaluation_count: 3,
                disable_count: 1,
                saved_at: Some("2024-01-01T00:00:00Z".to_string()),
            }),
            ..DevTraceSummary::empty()
        };

        let html = generate_html_report(&summary);
        assert!(html.contains("概览"));
        assert!(html.contains("操作类型统计"));
        assert!(html.contains("增量发送统计"));
        assert!(html.contains("搜索缓存统计"));
        assert!(html.contains("缓存调优历史"));
        assert!(html.contains("搜索质量历史"));
    }

    #[test]
    fn test_html_report_self_contained() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        // HTML 应该是自包含的 (除了 Chart.js CDN)
        assert!(html.contains("<html"));
        assert!(html.contains("</html>"));
        assert!(html.contains("<head>"));
        assert!(html.contains("</head>"));
        assert!(html.contains("<body>"));
        assert!(html.contains("</body>"));
    }
}
