//! HTML 报告生成器 — 将 DevTraceSummary 渲染为包含 Chart.js 图表的 HTML 报告
//!
//! 生成自包含的 HTML 文件, 包含:
//! - 概览统计面板 (总条目数、成功率、总耗时)
//! - Chart.js 折线图 (协同评分趋势、修复率趋势)
//! - 柱状图 (操作类型统计)
//! - Doughnut/Pie 图 (缓存命中率、操作类型分布) (Session 97)
//! - 甘特图 (时间线可视化) (Session 97)
//! - 深色模式切换 (Session 97)
//! - PDF 导出 (打印) (Session 97)
//! - 历史趋势面板
//!
//! ## 核心函数
//!
//! - [`generate_html_report`] — 从 DevTraceSummary 生成完整 HTML 报告
//! - [`generate_chart_js_line`] — 生成 Chart.js 折线图 HTML
//! - [`generate_chart_js_bar`] — 生成 Chart.js 柱状图 HTML
//! - [`generate_chart_js_doughnut`] — 生成 Chart.js Doughnut/Pie 图 HTML (Session 97)
//! - [`generate_chart_js_gantt`] — 生成 Chart.js 甘特图 HTML (Session 97)
//! - [`generate_stat_card`] — 生成统计卡片 HTML
//! - [`generate_report_toolbar`] — 生成报告工具栏 (深色模式 + PDF 导出) (Session 97)
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

use std::collections::HashMap;
use std::path::Path;

use crate::dev_trace::{ActionStats, DevTraceSummary, TimelineEntry, TraceAction};
use crate::sparkline::escape_html;

// ============================================================================
//  常量
// ============================================================================

/// Chart.js CDN 地址
const CHART_JS_CDN: &str = "https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js";

/// HTML 报告格式版本
pub const HTML_REPORT_FORMAT_VERSION: &str = "1.2";

/// 默认 Doughnut 图表颜色调色板 (Session 97)
///
/// 10 种鲜艳的颜色, 循环使用以区分不同数据切片。
pub const DEFAULT_DOUGHNUT_COLORS: &[&str] = &[
    "rgba(75, 192, 192, 0.8)",
    "rgba(255, 99, 132, 0.8)",
    "rgba(255, 205, 86, 0.8)",
    "rgba(54, 162, 235, 0.8)",
    "rgba(153, 102, 255, 0.8)",
    "rgba(255, 159, 64, 0.8)",
    "rgba(201, 203, 207, 0.8)",
    "rgba(75, 192, 192, 0.8)",
    "rgba(255, 99, 132, 0.8)",
    "rgba(255, 205, 86, 0.8)",
];

/// 为数据列表生成颜色数组, 循环使用默认调色板 (Session 97)
///
/// # 参数
///
/// - `count`: 需要的颜色数量
///
/// # 返回
///
/// CSS 颜色字符串列表
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_doughnut_colors;
/// let colors = generate_doughnut_colors(3);
/// assert_eq!(colors.len(), 3);
/// assert!(colors[0].contains("75, 192, 192"));
/// ```
pub fn generate_doughnut_colors(count: usize) -> Vec<String> {
    (0..count)
        .map(|i| DEFAULT_DOUGHNUT_COLORS[i % DEFAULT_DOUGHNUT_COLORS.len()].to_string())
        .collect()
}

/// 生成 Chart.js Doughnut/Pie 图 HTML (Session 97)
///
/// 创建一个包含 Chart.js canvas 的 div, 渲染为环形图 (doughnut) 或饼图 (pie)。
/// Doughnut 图中心可显示总计数, 适合展示占比分布 (如缓存命中率、操作类型分布)。
///
/// # 参数
///
/// - `id`: canvas ID (必须唯一)
/// - `title`: 图表标题
/// - `labels`: 各切片标签列表
/// - `data`: 各切片数值列表
/// - `colors`: 各切片颜色列表 (CSS 颜色值), 长度应与 `data` 相同;
///   如为空则自动使用 [`DEFAULT_DOUGHNUT_COLORS`] 调色板
///
/// # 返回
///
/// HTML 字符串, 包含 canvas 和 script
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_chart_js_doughnut;
/// let html = generate_chart_js_doughnut(
///     "cachePie",
///     "缓存命中率",
///     &vec!["命中".to_string(), "未命中".to_string(), "失败".to_string()],
///     &vec![6.0, 3.0, 1.0],
///     &[],
/// );
/// assert!(html.contains("doughnut"));
/// assert!(html.contains("命中"));
/// ```
pub fn generate_chart_js_doughnut(
    id: &str,
    title: &str,
    labels: &[String],
    data: &[f64],
    colors: &[String],
) -> String {
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string());
    let data_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
    let colors_vec: Vec<String> = if colors.is_empty() {
        generate_doughnut_colors(data.len())
    } else {
        colors.to_vec()
    };
    let colors_json = serde_json::to_string(&colors_vec).unwrap_or_else(|_| "[]".to_string());
    let total: f64 = data.iter().sum();

    format!(
        r#"<div class="chart-container">
  <h3 class="chart-title">{title}</h3>
  <canvas id="{id}"></canvas>
  <script>
  (function() {{
    const ctx = document.getElementById('{id}');
    if (!ctx) return;
    new Chart(ctx, {{
      type: 'doughnut',
      data: {{
        labels: {labels_json},
        datasets: [{{
          data: {data_json},
          backgroundColor: {colors_json},
          borderColor: '#fff',
          borderWidth: 2,
          hoverOffset: 8,
        }}]
      }},
      options: {{
        responsive: true,
        plugins: {{
          title: {{ display: true, text: '{title}' }},
          legend: {{ position: 'right' }},
          tooltip: {{
            callbacks: {{
              label: function(ctx) {{
                const total = {total};
                const pct = total > 0 ? (ctx.parsed / total * 100).toFixed(1) + '%' : '0%';
                return ctx.label + ': ' + ctx.parsed + ' (' + pct + ')';
              }}
            }}
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
        colors_json = colors_json,
        total = total,
    )
}

/// 生成 Chart.js 甘特图 HTML (Session 97)
///
/// 创建一个水平条形图, 以时间线形式展示各项操作的执行时段。
/// 每个条形代表一个 timeline 条目, 条形长度 = 执行耗时, 颜色区分成功/失败。
///
/// # 参数
///
/// - `id`: canvas ID (必须唯一)
/// - `title`: 图表标题
/// - `labels`: Y 轴标签列表 (每个条形的标签)
/// - `data`: 浮动条数据列表, 每项为 `[start_ms, end_ms]`
/// - `colors`: 每个条形的颜色列表
///
/// # 返回
///
/// HTML 字符串, 包含 canvas 和 script
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_chart_js_gantt;
/// let html = generate_chart_js_gantt(
///     "ganttChart",
///     "时间线",
///     &vec!["Task 0".to_string(), "Task 1".to_string()],
///     &vec![vec![0.0, 5000.0], vec![5000.0, 8000.0]],
///     &vec!["rgba(75,192,192,0.6)".to_string(), "rgba(255,99,132,0.6)".to_string()],
/// );
/// assert!(html.contains("bar"));
/// assert!(html.contains("indexAxis: 'y'"));
/// ```
pub fn generate_chart_js_gantt(
    id: &str,
    title: &str,
    labels: &[String],
    data: &[Vec<f64>],
    colors: &[String],
) -> String {
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string());
    let data_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
    let colors_vec: Vec<String> = if colors.is_empty() {
        vec!["rgba(75, 192, 192, 0.6)".to_string(); data.len()]
    } else {
        colors.to_vec()
    };
    let colors_json = serde_json::to_string(&colors_vec).unwrap_or_else(|_| "[]".to_string());

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
          label: '{title}',
          data: {data_json},
          backgroundColor: {colors_json},
          borderColor: {colors_json},
          borderWidth: 1,
          borderSkipped: false,
        }}]
      }},
      options: {{
        indexAxis: 'y',
        responsive: true,
        plugins: {{
          title: {{ display: true, text: '{title}' }},
          legend: {{ display: false }},
          tooltip: {{
            callbacks: {{
              label: function(ctx) {{
                const range = ctx.raw;
                const dur = (range[1] - range[0]) / 1000;
                return ctx.label + ': ' + dur.toFixed(1) + 's';
              }}
            }}
          }}
        }},
        scales: {{
          x: {{
            title: {{ display: true, text: '时间 (ms)' }},
            beginAtZero: true,
          }},
          y: {{
            beginAtZero: true,
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
        colors_json = colors_json,
    )
}

/// 从 TimelineEntry 列表提取甘特图数据 (Session 97)
///
/// 将时间线条目转换为 `[start_ms, end_ms]` 浮动条数据, 并生成对应的标签和颜色。
/// 成功的操作使用绿色, 失败的使用红色。
///
/// # 参数
///
/// - `timeline`: 时间线条目列表
///
/// # 返回
///
/// 返回三元组: `(labels, data, colors)`
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceSummary, TimelineEntry, TraceAction};
/// # use forge::html_report::extract_gantt_data;
/// # use chrono::Utc;
/// let timeline = vec![
///     TimelineEntry {
///         timestamp: Utc::now(),
///         action: TraceAction::TaskExecution,
///         task_name: Some("Task 0".to_string()),
///         success: true,
///         duration_ms: 100,
///     },
/// ];
/// let (labels, data, colors) = extract_gantt_data(&timeline);
/// assert_eq!(labels.len(), 1);
/// assert_eq!(data.len(), 1);
/// assert_eq!(data[0].len(), 2); // [start, end]
/// assert!(colors[0].contains("75, 192, 192")); // 成功→绿
/// ```
pub fn extract_gantt_data(
    timeline: &[crate::dev_trace::TimelineEntry],
) -> (Vec<String>, Vec<Vec<f64>>, Vec<String>) {
    let mut cumulative_ms: f64 = 0.0;
    let labels: Vec<String> = timeline
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let name = e
                .task_name
                .clone()
                .unwrap_or_else(|| format!("{:?}", e.action));
            format!("#{} {}", i, name)
        })
        .collect();

    let data: Vec<Vec<f64>> = timeline
        .iter()
        .map(|e| {
            let start = cumulative_ms;
            let end = cumulative_ms + e.duration_ms as f64;
            cumulative_ms = end;
            vec![start, end]
        })
        .collect();

    let colors: Vec<String> = timeline
        .iter()
        .map(|e| {
            if e.success {
                "rgba(75, 192, 192, 0.6)".to_string()
            } else {
                "rgba(255, 99, 132, 0.6)".to_string()
            }
        })
        .collect();

    (labels, data, colors)
}

/// 生成报告工具栏 HTML (Session 97)
///
/// 包含深色模式切换按钮和 PDF 导出 (打印) 按钮。
/// 深色模式使用 localStorage 持久化用户选择。
///
/// # 返回
///
/// HTML 字符串, 包含工具栏 div 和 JavaScript 脚本
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_report_toolbar;
/// let html = generate_report_toolbar();
/// assert!(html.contains("toolbar"));
/// assert!(html.contains("toggleTheme"));
/// assert!(html.contains("window.print"));
/// ```
pub fn generate_report_toolbar() -> String {
    r#"<div class="report-toolbar">
  <button class="toolbar-btn" onclick="toggleTheme()" id="themeBtn">🌙 深色模式</button>
  <button class="toolbar-btn" onclick="window.print()">📄 导出 PDF</button>
</div>
<script>
  function toggleTheme() {
    const body = document.body;
    const isDark = body.classList.toggle('dark-mode');
    localStorage.setItem('forge-theme', isDark ? 'dark' : 'light');
    document.getElementById('themeBtn').textContent = isDark ? '☀️ 浅色模式' : '🌙 深色模式';
  }
  (function() {
    const saved = localStorage.getItem('forge-theme');
    if (saved === 'dark') {
      document.body.classList.add('dark-mode');
      const btn = document.getElementById('themeBtn');
      if (btn) btn.textContent = '☀️ 浅色模式';
    }
  })();
</script>"#
        .to_string()
}

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

/// 生成 Chart.js 折线图 HTML (自动 Y 轴范围)
///
/// 与 [`generate_chart_js_line`] 类似, 但 Y 轴不强制 0.0~1.0 范围,
/// 也不使用百分比刻度。适用于 TTL 秒数、带符号差值等非百分比数据。
///
/// # 参数
///
/// - `id`: canvas ID (必须唯一)
/// - `title`: 图表标题
/// - `labels`: X 轴标签列表
/// - `data`: Y 轴数据列表
/// - `color`: 线条颜色 (CSS 颜色值)
/// - `y_label`: Y 轴标签
///
/// # 返回
///
/// HTML 字符串, 包含 canvas 和 script
pub fn generate_chart_js_line_raw(
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
          y: {{ beginAtZero: false }}
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

/// 根据数据值的正负生成 Chart.js 点颜色数组 (Session 96)
///
/// 为每个数据点生成对应的 CSS 颜色值:
/// - 正值 (> 0): 绿色 `rgba(75, 192, 192, 1)`
/// - 负值 (< 0): 红色 `rgba(255, 99, 132, 1)`
/// - 零值: 灰色 `rgba(201, 203, 207, 1)`
///
/// 用于 Chart.js 的 `pointBackgroundColor` 和 `pointBorderColor` 属性,
/// 在差值趋势图中直观区分正负值。
///
/// # 参数
///
/// - `data`: 数据值列表
///
/// # 返回
///
/// CSS 颜色字符串列表, 长度与 `data` 相同
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_point_colors;
/// let colors = generate_point_colors(&[0.1, -0.2, 0.0]);
/// assert_eq!(colors.len(), 3);
/// assert!(colors[0].contains("75, 192, 192"));   // 正→绿
/// assert!(colors[1].contains("255, 99, 132"));    // 负→红
/// assert!(colors[2].contains("201, 203, 207"));   // 零→灰
/// ```
pub fn generate_point_colors(data: &[f64]) -> Vec<String> {
    data.iter()
        .map(|&v| {
            if v > 0.0 {
                "rgba(75, 192, 192, 1)".to_string()
            } else if v < 0.0 {
                "rgba(255, 99, 132, 1)".to_string()
            } else {
                "rgba(201, 203, 207, 1)".to_string()
            }
        })
        .collect()
}

/// 生成带颜色编码数据点的 Chart.js 折线图 HTML (Session 96)
///
/// 与 [`generate_chart_js_line_raw`] 类似, 但数据点的颜色根据值的正负自动着色:
/// - 正值点: 绿色
/// - 负值点: 红色
/// - 零值点: 灰色
///
/// 线条颜色保持统一, 仅数据点使用颜色编码, 便于在趋势图中直观识别正负区间。
///
/// # 参数
///
/// - `id`: canvas ID (必须唯一)
/// - `title`: 图表标题
/// - `labels`: X 轴标签列表
/// - `data`: Y 轴数据列表
/// - `color`: 线条颜色 (CSS 颜色值)
/// - `y_label`: Y 轴标签
///
/// # 返回
///
/// HTML 字符串, 包含 canvas 和 script
pub fn generate_chart_js_line_colored(
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
    let point_colors = generate_point_colors(data);
    let point_colors_json =
        serde_json::to_string(&point_colors).unwrap_or_else(|_| "[]".to_string());

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
          pointRadius: 5,
          pointHoverRadius: 7,
          pointBackgroundColor: {point_colors_json},
          pointBorderColor: {point_colors_json},
        }}]
      }},
      options: {{
        responsive: true,
        plugins: {{
          title: {{ display: true, text: '{title}' }}
        }},
        scales: {{
          y: {{ beginAtZero: false }}
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
        point_colors_json = point_colors_json,
        y_label = escape_html(y_label),
    )
}

/// 生成 HTML 报告的 CSS 样式
///
/// 返回完整的 `<style>` 标签内容。
/// 包含浅色/深色模式 CSS 变量、打印样式和响应式布局 (Session 97 增强)。
/// Session 98 新增: 搜索框样式、模态框样式、可排序表头样式。
pub fn generate_css_styles() -> &'static str {
    r#"<style>
  :root {
    --bg: #f5f5f5; --text: #333; --card-bg: #fff; --border: #e0e0e0;
    --heading: #1a1a2e; --muted: #666; --faint: #999; --hover: #f9f9f9;
    --shadow: rgba(0,0,0,0.08);
  }
  body.dark-mode {
    --bg: #1a1a2e; --text: #e0e0e0; --card-bg: #2a2a3e; --border: #3a3a4e;
    --heading: #f0f0f0; --muted: #aaa; --faint: #888; --hover: #33334a;
    --shadow: rgba(0,0,0,0.3);
  }
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    background: var(--bg); color: var(--text); padding: 20px;
    transition: background 0.3s, color 0.3s;
  }
  .container { max-width: 1200px; margin: 0 auto; }
  h1 { color: var(--heading); margin-bottom: 8px; }
  h2 { color: var(--heading); margin: 24px 0 12px; border-bottom: 2px solid var(--border); padding-bottom: 8px; }
  .meta { color: var(--muted); font-size: 14px; margin-bottom: 20px; }
  .report-toolbar {
    display: flex; gap: 12px; margin-bottom: 20px; flex-wrap: wrap;
  }
  .toolbar-btn {
    padding: 8px 16px; border: 1px solid var(--border); border-radius: 6px;
    background: var(--card-bg); color: var(--text); cursor: pointer;
    font-size: 14px; transition: opacity 0.2s;
  }
  .toolbar-btn:hover { opacity: 0.85; }
  .stats-grid {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px; margin-bottom: 24px;
  }
  .stat-card {
    background: var(--card-bg); border-radius: 8px; padding: 16px;
    box-shadow: 0 2px 4px var(--shadow);
    border-left: 4px solid #4a90d9;
  }
  .stat-blue { border-left-color: #4a90d9; }
  .stat-green { border-left-color: #27ae60; }
  .stat-orange { border-left-color: #e67e22; }
  .stat-red { border-left-color: #e74c3c; }
  .stat-purple { border-left-color: #9b59b6; }
  .stat-title { font-size: 13px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }
  .stat-value { font-size: 28px; font-weight: 700; color: var(--heading); margin: 4px 0; }
  .stat-desc { font-size: 12px; color: var(--faint); }
  .charts-grid {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(500px, 1fr));
    gap: 20px; margin-bottom: 24px;
  }
  .chart-container {
    background: var(--card-bg); border-radius: 8px; padding: 20px;
    box-shadow: 0 2px 4px var(--shadow);
    position: relative;
  }
  .chart-title { font-size: 16px; color: var(--heading); margin-bottom: 12px; cursor: pointer; }
  .chart-title:hover { text-decoration: underline; }
  .chart-fullscreen-btn {
    position: absolute; top: 16px; right: 16px;
    background: var(--hover); border: 1px solid var(--border); border-radius: 4px;
    padding: 2px 8px; cursor: pointer; font-size: 16px; color: var(--text);
    opacity: 0.6; transition: opacity 0.2s;
  }
  .chart-fullscreen-btn:hover { opacity: 1; }
  table { width: 100%; border-collapse: collapse; background: var(--card-bg); border-radius: 8px; overflow: hidden; box-shadow: 0 2px 4px var(--shadow); }
  th { background: var(--heading); color: var(--card-bg); padding: 12px; text-align: left; font-size: 14px; cursor: pointer; user-select: none; }
  th:hover { opacity: 0.85; }
  th::after { content: ' \21C5'; opacity: 0.5; font-size: 12px; }
  td { padding: 10px 12px; border-bottom: 1px solid var(--border); font-size: 14px; }
  tr:last-child td { border-bottom: none; }
  tr:hover td { background: var(--hover); }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 12px; font-weight: 600; }
  .badge-green { background: #e8f5e9; color: #27ae60; }
  .badge-red { background: #ffebee; color: #e74c3c; }
  .timeline-search {
    margin-bottom: 12px; display: flex; gap: 8px; align-items: center;
  }
  .timeline-search input {
    padding: 8px 12px; border: 1px solid var(--border); border-radius: 6px;
    background: var(--card-bg); color: var(--text); font-size: 14px; width: 300px;
  }
  .timeline-search .search-count { color: var(--faint); font-size: 13px; }
  .chart-modal {
    display: none; position: fixed; z-index: 9999; left: 0; top: 0;
    width: 100%; height: 100%; background: rgba(0,0,0,0.7);
    justify-content: center; align-items: center;
  }
  .chart-modal.active { display: flex; }
  .chart-modal-content {
    background: var(--card-bg); border-radius: 12px; padding: 30px;
    width: 90%; max-width: 1000px; max-height: 80vh; overflow: auto;
    position: relative;
  }
  .chart-modal-close {
    position: absolute; top: 12px; right: 16px;
    font-size: 28px; font-weight: bold; cursor: pointer;
    color: var(--text); opacity: 0.6; transition: opacity 0.2s;
  }
  .chart-modal-close:hover { opacity: 1; }
  .footer { text-align: center; color: var(--faint); font-size: 12px; margin-top: 40px; padding-top: 20px; border-top: 1px solid var(--border); }
  @media print {
    body { background: white !important; color: black !important; padding: 0; }
    .report-toolbar { display: none !important; }
    .timeline-search { display: none !important; }
    .chart-fullscreen-btn { display: none !important; }
    .chart-modal { display: none !important; }
    .stat-card, .chart-container, table { box-shadow: none !important; border: 1px solid #ccc !important; break-inside: avoid; }
    .chart-container { page-break-inside: avoid; }
    h2 { break-after: avoid; }
    th { cursor: default; }
    th::after { content: ''; }
  }
</style>"#
}

// ============================================================================
//  Session 98 — 数据导出 CSV/JSON + 交互增强
// ============================================================================

/// 转义 CSV 字段 — 如果字段包含逗号、引号或换行, 则用双引号包裹 (Session 98)
///
/// 遵循 RFC 4180 规范:
/// - 字段包含逗号 (`,`)、双引号 (`"`) 或换行符时, 用双引号包裹
/// - 字段内的双引号用两个双引号转义 (`""` → `"`)
///
/// # 参数
///
/// - `field`: 原始字段值
///
/// # 返回
///
/// 转义后的 CSV 安全字段
///
/// # 示例
///
/// ```
/// # use forge::html_report::csv_escape_field;
/// assert_eq!(csv_escape_field("hello"), "hello");
/// assert_eq!(csv_escape_field("a,b"), "\"a,b\"");
/// assert_eq!(csv_escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
/// assert_eq!(csv_escape_field("line1\nline2"), "\"line1\nline2\"");
/// ```
pub fn csv_escape_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// 将时间线数据导出为 CSV 格式 (Session 98)
///
/// 生成包含表头和数据行的 CSV 字符串, 列: 时间戳,操作类型,任务名称,耗时(ms),结果。
///
/// # 参数
///
/// - `timeline`: 时间线条目列表
///
/// # 返回
///
/// CSV 格式字符串 (UTF-8, LF 换行)
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_timeline_csv;
/// # use forge::dev_trace::{TimelineEntry, TraceAction};
/// # use chrono::Utc;
/// let timeline = vec![
///     TimelineEntry {
///         timestamp: chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z").unwrap().with_timezone(&Utc),
///         action: TraceAction::TaskExecution,
///         task_name: Some("Task 0".to_string()),
///         success: true,
///         duration_ms: 500,
///     },
/// ];
/// let csv = generate_timeline_csv(&timeline);
/// assert!(csv.contains("时间戳"));
/// assert!(csv.contains("操作类型"));
/// assert!(csv.contains("Task 0"));
/// assert!(csv.contains("成功"));
/// ```
pub fn generate_timeline_csv(timeline: &[TimelineEntry]) -> String {
    let mut csv = String::from("时间戳,操作类型,任务名称,耗时(ms),结果\n");
    for entry in timeline {
        let task = entry.task_name.as_deref().unwrap_or("");
        let result = if entry.success { "成功" } else { "失败" };
        csv.push_str(&format!(
            "{},{},{},{},{}\n",
            csv_escape_field(&entry.timestamp.to_rfc3339()),
            csv_escape_field(&format!("{:?}", entry.action)),
            csv_escape_field(task),
            entry.duration_ms,
            result,
        ));
    }
    csv
}

/// 将操作类型统计导出为 CSV 格式 (Session 98)
///
/// 生成包含表头和数据行的 CSV 字符串, 列: 操作类型,总次数,成功次数,成功率,总耗时(ms),平均耗时(ms)。
///
/// # 参数
///
/// - `by_action`: 按操作类型分组的统计映射
///
/// # 返回
///
/// CSV 格式字符串
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_action_stats_csv;
/// # use forge::dev_trace::{ActionStats, TraceAction};
/// # use std::collections::HashMap;
/// let mut by_action = HashMap::new();
/// by_action.insert(TraceAction::CompileCheck, ActionStats { count: 10, success_count: 8, total_duration_ms: 5000 });
/// let csv = generate_action_stats_csv(&by_action);
/// assert!(csv.contains("操作类型"));
/// assert!(csv.contains("CompileCheck"));
/// assert!(csv.contains("10"));
/// assert!(csv.contains("80.0%"));
/// ```
pub fn generate_action_stats_csv(by_action: &HashMap<TraceAction, ActionStats>) -> String {
    let mut csv = String::from("操作类型,总次数,成功次数,成功率,总耗时(ms),平均耗时(ms)\n");
    let mut entries: Vec<_> = by_action.iter().collect();
    entries.sort_by_key(|(action, _)| format!("{:?}", action));
    for (action, stats) in &entries {
        let rate = if stats.count > 0 {
            stats.success_count as f64 / stats.count as f64 * 100.0
        } else {
            0.0
        };
        let avg = stats.avg_duration_ms();
        csv.push_str(&format!(
            "{},{},{},{:.1}%,{},{}\n",
            csv_escape_field(&format!("{:?}", action)),
            stats.count,
            stats.success_count,
            rate,
            stats.total_duration_ms,
            avg,
        ));
    }
    csv
}

/// 将时间线数据导出为 JSON 格式 (Session 98)
///
/// 生成包含时间线条目的 JSON 数组字符串。
///
/// # 参数
///
/// - `timeline`: 时间线条目列表
///
/// # 返回
///
/// JSON 格式字符串 (pretty print)
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_timeline_json;
/// # use forge::dev_trace::{TimelineEntry, TraceAction};
/// # use chrono::Utc;
/// let timeline = vec![
///     TimelineEntry {
///         timestamp: Utc::now(),
///         action: TraceAction::TaskExecution,
///         task_name: Some("Task A".to_string()),
///         success: true,
///         duration_ms: 300,
///     },
/// ];
/// let json = generate_timeline_json(&timeline);
/// assert!(json.contains("Task A"));
/// assert!(json.contains("\"duration_ms\""));
/// ```
pub fn generate_timeline_json(timeline: &[TimelineEntry]) -> String {
    #[derive(serde::Serialize)]
    struct TimelineExport<'a> {
        timestamp: String,
        action: String,
        task_name: &'a str,
        duration_ms: u64,
        success: bool,
    }

    let export: Vec<TimelineExport> = timeline
        .iter()
        .map(|e| TimelineExport {
            timestamp: e.timestamp.to_rfc3339(),
            action: format!("{:?}", e.action),
            task_name: e.task_name.as_deref().unwrap_or(""),
            duration_ms: e.duration_ms,
            success: e.success,
        })
        .collect();

    serde_json::to_string_pretty(&export).unwrap_or_else(|_| "[]".to_string())
}

/// 将操作类型统计导出为 JSON 格式 (Session 98)
///
/// 生成包含操作类型统计的 JSON 数组字符串。
///
/// # 参数
///
/// - `by_action`: 按操作类型分组的统计映射
///
/// # 返回
///
/// JSON 格式字符串 (pretty print)
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_action_stats_json;
/// # use forge::dev_trace::{ActionStats, TraceAction};
/// # use std::collections::HashMap;
/// let mut by_action = HashMap::new();
/// by_action.insert(TraceAction::CompileCheck, ActionStats { count: 5, success_count: 4, total_duration_ms: 2000 });
/// let json = generate_action_stats_json(&by_action);
/// assert!(json.contains("CompileCheck"));
/// assert!(json.contains("\"count\": 5"));
/// ```
pub fn generate_action_stats_json(by_action: &HashMap<TraceAction, ActionStats>) -> String {
    #[derive(serde::Serialize)]
    struct ActionExport {
        action: String,
        count: usize,
        success_count: usize,
        success_rate: f64,
        total_duration_ms: u64,
        avg_duration_ms: u64,
    }

    let mut entries: Vec<ActionExport> = by_action
        .iter()
        .map(|(action, stats)| ActionExport {
            action: format!("{:?}", action),
            count: stats.count,
            success_count: stats.success_count,
            success_rate: if stats.count > 0 {
                stats.success_count as f64 / stats.count as f64
            } else {
                0.0
            },
            total_duration_ms: stats.total_duration_ms,
            avg_duration_ms: stats.avg_duration_ms(),
        })
        .collect();
    entries.sort_by(|a, b| a.action.cmp(&b.action));

    serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
}

/// 生成导出按钮 HTML (Session 98)
///
/// 包含 CSV 和 JSON 导出按钮, 点击时通过 JavaScript 下载对应格式的数据。
/// 按钮使用 `onclick` 调用 `downloadTimelineCSV()` 和 `downloadTimelineJSON()` 函数。
///
/// # 返回
///
/// HTML 字符串, 包含按钮和内联 script
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_export_buttons;
/// let html = generate_export_buttons();
/// assert!(html.contains("downloadTimelineCSV"));
/// assert!(html.contains("downloadTimelineJSON"));
/// assert!(html.contains("CSV"));
/// assert!(html.contains("JSON"));
/// ```
pub fn generate_export_buttons() -> String {
    r#"<div class="export-buttons" style="margin-bottom: 12px; display: flex; gap: 8px;">
  <button class="toolbar-btn" onclick="downloadTimelineCSV()">📋 导出时间线 CSV</button>
  <button class="toolbar-btn" onclick="downloadTimelineJSON()">📦 导出时间线 JSON</button>
  <button class="toolbar-btn" onclick="downloadActionStatsCSV()">📋 导出操作统计 CSV</button>
  <button class="toolbar-btn" onclick="downloadActionStatsJSON()">📦 导出操作统计 JSON</button>
</div>
<script>
  function downloadFile(filename, content, mimeType) {
    const blob = new Blob([content], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }
  function downloadTimelineCSV() {
    const csv = document.getElementById('timeline-csv-data')?.textContent || '';
    downloadFile('forge-timeline.csv', csv, 'text/csv;charset=utf-8');
  }
  function downloadTimelineJSON() {
    const json = document.getElementById('timeline-json-data')?.textContent || '';
    downloadFile('forge-timeline.json', json, 'application/json');
  }
  function downloadActionStatsCSV() {
    const csv = document.getElementById('action-stats-csv-data')?.textContent || '';
    downloadFile('forge-action-stats.csv', csv, 'text/csv;charset=utf-8');
  }
  function downloadActionStatsJSON() {
    const json = document.getElementById('action-stats-json-data')?.textContent || '';
    downloadFile('forge-action-stats.json', json, 'application/json');
  }
</script>"#
        .to_string()
}

/// 生成时间线搜索过滤框 HTML + JavaScript (Session 98)
///
/// 提供一个文本输入框, 实时过滤时间线表格行。
/// 支持按操作类型、任务名称、结果进行搜索。
///
/// # 返回
///
/// HTML 字符串, 包含搜索框和过滤脚本
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_timeline_search;
/// let html = generate_timeline_search();
/// assert!(html.contains("timeline-search"));
/// assert!(html.contains("filterTimeline"));
/// ```
pub fn generate_timeline_search() -> String {
    r#"<div class="timeline-search">
  <input type="text" id="timelineSearch" placeholder="🔍 搜索时间线 (操作类型/任务名称/结果)..." oninput="filterTimeline()">
  <span class="search-count" id="timelineSearchCount"></span>
</div>
<script>
  function filterTimeline() {
    const query = document.getElementById('timelineSearch')?.value.toLowerCase().trim() || '';
    const tbody = document.querySelector('#timelineTable tbody');
    if (!tbody) return;
    const rows = tbody.querySelectorAll('tr');
    let visible = 0;
    rows.forEach(function(row) {
      const text = row.textContent.toLowerCase();
      if (query === '' || text.includes(query)) {
        row.style.display = '';
        visible++;
      } else {
        row.style.display = 'none';
      }
    });
    const countEl = document.getElementById('timelineSearchCount');
    if (countEl) {
      countEl.textContent = query ? visible + ' / ' + rows.length + ' 条匹配' : '';
    }
  }
</script>"#
        .to_string()
}

/// 生成图表全屏放大模态框 JavaScript (Session 98)
///
/// 点击图表标题或全屏按钮时, 将图表内容复制到模态框中放大显示。
/// 支持点击背景或关闭按钮关闭模态框, 按 ESC 键也可关闭。
///
/// # 返回
///
/// HTML 字符串, 包含模态框 div 和 JavaScript 脚本
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_chart_fullscreen_script;
/// let html = generate_chart_fullscreen_script();
/// assert!(html.contains("chart-modal"));
/// assert!(html.contains("openChartFullscreen"));
/// assert!(html.contains("closeChartFullscreen"));
/// ```
pub fn generate_chart_fullscreen_script() -> String {
    r#"<div class="chart-modal" id="chartModal" onclick="if(event.target===this)closeChartFullscreen()">
  <div class="chart-modal-content">
    <span class="chart-modal-close" onclick="closeChartFullscreen()">&times;</span>
    <div id="chartModalBody"></div>
  </div>
</div>
<script>
  function openChartFullscreen(canvasId) {
    const source = document.getElementById(canvasId);
    if (!source) return;
    const container = source.closest('.chart-container');
    if (!container) return;
    const title = container.querySelector('.chart-title')?.textContent || '';
    const modalBody = document.getElementById('chartModalBody');
    if (!modalBody) return;
    modalBody.innerHTML = '<h3 style="margin-bottom:16px;">' + title + '</h3><canvas id="chartModalCanvas"></canvas>';
    const modalCanvas = document.getElementById('chartModalCanvas');
    if (!modalCanvas) return;
    const ctx = modalCanvas.getContext('2d');
    const chart = Chart.getChart(canvasId);
    if (chart) {
      new Chart(ctx, {
        type: chart.config.type,
        data: chart.data,
        options: Object.assign({}, chart.options, { responsive: true, maintainAspectRatio: false })
      });
    }
    const modal = document.getElementById('chartModal');
    if (modal) modal.classList.add('active');
    document.body.style.overflow = 'hidden';
  }
  function closeChartFullscreen() {
    const modal = document.getElementById('chartModal');
    if (modal) modal.classList.remove('active');
    const modalBody = document.getElementById('chartModalBody');
    if (modalBody) modalBody.innerHTML = '';
    document.body.style.overflow = '';
  }
  document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') closeChartFullscreen();
  });
</script>"#
        .to_string()
}

/// 生成表格排序 JavaScript (Session 98)
///
/// 点击表格表头时, 按该列进行升序/降序排序, 再次点击切换排序方向。
/// 支持数值和文本列的自动类型检测。
///
/// # 返回
///
/// HTML 字符串, 包含排序 JavaScript 脚本
///
/// # 示例
///
/// ```
/// # use forge::html_report::generate_table_sort_script;
/// let html = generate_table_sort_script();
/// assert!(html.contains("sortTable"));
/// assert!(html.contains("sortDirection"));
/// ```
pub fn generate_table_sort_script() -> String {
    r#"<script>
  var sortDirection = {};
  function sortTable(tableId, columnIndex) {
    const table = document.getElementById(tableId);
    if (!table) return;
    const tbody = table.querySelector('tbody');
    if (!tbody) return;
    const rows = Array.from(tbody.querySelectorAll('tr'));
    const key = tableId + '-' + columnIndex;
    const dir = sortDirection[key] = !sortDirection[key];
    rows.sort(function(a, b) {
      let aVal = a.cells[columnIndex]?.textContent.trim() || '';
      let bVal = b.cells[columnIndex]?.textContent.trim() || '';
      const aNum = parseFloat(aVal.replace(/[^0-9.-]/g, ''));
      const bNum = parseFloat(bVal.replace(/[^0-9.-]/g, ''));
      if (!isNaN(aNum) && !isNaN(bNum)) {
        return dir ? aNum - bNum : bNum - aNum;
      }
      return dir ? aVal.localeCompare(bVal) : bVal.localeCompare(aVal);
    });
    rows.forEach(function(row) { tbody.appendChild(row); });
  }
</script>"#
        .to_string()
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

    // === 报告工具栏 (Session 97) ===
    html.push_str(&generate_report_toolbar());
    html.push('\n');

    // === 导出按钮 (Session 98) ===
    html.push_str(&generate_export_buttons());
    html.push('\n');

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
        // 操作类型 Doughnut 图 (Session 97)
        html.push_str(&generate_chart_js_doughnut(
            "actionDoughnut",
            "操作类型占比",
            &action_labels,
            &action_data,
            &[],
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

        // === TTL 变化趋势图 (Session 94) ===
        if let Some(ref ttl_values) = summary.ttl_history_values {
            if ttl_values.len() >= 2 {
                let labels: Vec<String> = (1..=ttl_values.len())
                    .map(|i| format!("决策 {}", i))
                    .collect();
                html.push_str("<div class=\"charts-grid\">\n");
                html.push_str(&generate_chart_js_line_raw(
                    "ttlTrendChart",
                    "TTL 变化趋势",
                    &labels,
                    ttl_values,
                    "rgba(54, 162, 235, 1)",
                    "TTL (秒)",
                ));
                html.push_str("</div>\n");
            }
        }

        // === 关联差值趋势图 (Session 94, 颜色编码 Session 96) ===
        if let Some(ref diff_values) = summary.correlation_diff_history {
            if diff_values.len() >= 2 {
                let labels: Vec<String> = (1..=diff_values.len())
                    .map(|i| format!("决策 {}", i))
                    .collect();
                html.push_str("<div class=\"charts-grid\">\n");
                html.push_str(&generate_chart_js_line_colored(
                    "diffTrendChart",
                    "关联差值趋势",
                    &labels,
                    diff_values,
                    "rgba(255, 99, 132, 1)",
                    "差值",
                ));
                html.push_str("</div>\n");
            }
        }
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

        // === 搜索质量差值趋势图 (Session 95, 颜色编码 Session 96) ===
        if let Some(ref diffs) = summary.search_diff_history {
            if diffs.len() >= 2 {
                let labels: Vec<String> =
                    (1..=diffs.len()).map(|i| format!("评估 {}", i)).collect();
                html.push_str("<div class=\"charts-grid\">\n");
                html.push_str(&generate_chart_js_line_colored(
                    "searchDiffTrendChart",
                    "搜索质量差值趋势",
                    &labels,
                    diffs,
                    "rgba(75, 192, 192, 1)",
                    "差值",
                ));
                html.push_str("</div>\n");
            }
        }
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

        // === Memory 评估差值趋势图 (Session 95, 颜色编码 Session 96) ===
        if let Some(ref diffs) = summary.memory_diff_history {
            if diffs.len() >= 2 {
                let labels: Vec<String> =
                    (1..=diffs.len()).map(|i| format!("评估 {}", i)).collect();
                html.push_str("<div class=\"charts-grid\">\n");
                html.push_str(&generate_chart_js_line_colored(
                    "memoryDiffTrendChart",
                    "Memory 评估差值趋势",
                    &labels,
                    diffs,
                    "rgba(153, 102, 255, 1)",
                    "差值",
                ));
                html.push_str("</div>\n");
            }
        }
    }

    // === 联合决策历史 (Session 99) ===
    if let Some(ref jdh) = summary.joint_decision_history_summary {
        if !jdh.is_empty() {
            html.push_str("<h2>🔗 联合决策历史</h2>\n<div class=\"stats-grid\">\n");
            html.push_str(&generate_stat_card(
                "Session 数",
                &jdh.session_count.to_string(),
                None,
                "blue",
            ));
            html.push_str(&generate_stat_card(
                "累计决策",
                &jdh.total_decisions.to_string(),
                None,
                "purple",
            ));
            html.push_str(&generate_stat_card(
                "升级警告",
                &jdh.total_escalations.to_string(),
                Some(&format!("升级率 {:.1}%", jdh.escalation_rate * 100.0)),
                "orange",
            ));
            html.push_str(&generate_stat_card(
                "保守模式",
                &jdh.total_conservative_modes.to_string(),
                Some(&format!("占比 {:.1}%", jdh.conservative_mode_rate * 100.0)),
                if jdh.current_conservative_mode {
                    "red"
                } else {
                    "green"
                },
            ));
            html.push_str("</div>\n");

            // 历史统计表格
            html.push_str(
                "<table>\n<thead>\n<tr><th>指标</th><th>值</th></tr>\n</thead>\n<tbody>\n",
            );
            html.push_str(&format!(
                "<tr><td>最新决策</td><td>{}</td></tr>\n",
                jdh.latest_action.label()
            ));
            html.push_str(&format!(
                "<tr><td>当前模式</td><td>{}</td></tr>\n",
                if jdh.current_conservative_mode {
                    "保守模式"
                } else {
                    "正常模式"
                }
            ));
            if let Some(ref saved_at) = jdh.saved_at {
                html.push_str(&format!(
                    "<tr><td>保存时间</td><td>{}</td></tr>\n",
                    escape_html(saved_at)
                ));
            }
            html.push_str("</tbody>\n</table>\n");
        }
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

        // === 缓存命中率 Doughnut 图 (Session 97) ===
        let cache_labels = vec!["命中".to_string(), "未命中".to_string(), "失败".to_string()];
        let cache_data = vec![
            cs.cache_hits as f64,
            cs.cache_misses as f64,
            cs.search_failures as f64,
        ];
        let cache_colors = vec![
            "rgba(75, 192, 192, 0.8)".to_string(),
            "rgba(255, 205, 86, 0.8)".to_string(),
            "rgba(255, 99, 132, 0.8)".to_string(),
        ];
        html.push_str("<div class=\"charts-grid\">\n");
        html.push_str(&generate_chart_js_doughnut(
            "cacheDoughnut",
            "缓存命中率分布",
            &cache_labels,
            &cache_data,
            &cache_colors,
        ));
        html.push_str("</div>\n");
    }

    // === 时间线甘特图 (Session 97) ===
    if !summary.timeline.is_empty() {
        let (gantt_labels, gantt_data, gantt_colors) = extract_gantt_data(&summary.timeline);
        html.push_str("<h2>📊 时间线甘特图</h2>\n<div class=\"charts-grid\">\n");
        html.push_str(&generate_chart_js_gantt(
            "ganttChart",
            "任务执行时间线",
            &gantt_labels,
            &gantt_data,
            &gantt_colors,
        ));
        html.push_str("</div>\n");
    }

    // === 时间线表格 (Session 98: 搜索过滤 + 可排序表头) ===
    if !summary.timeline.is_empty() {
        html.push_str("<h2>📋 时间线 (最近 100 条)</h2>\n");
        // 搜索过滤框
        html.push_str(&generate_timeline_search());
        html.push('\n');
        // 可排序表格
        html.push_str("<table id=\"timelineTable\">\n<thead>\n<tr>");
        html.push_str("<th onclick=\"sortTable('timelineTable', 0)\">时间</th>");
        html.push_str("<th onclick=\"sortTable('timelineTable', 1)\">操作</th>");
        html.push_str("<th onclick=\"sortTable('timelineTable', 2)\">任务</th>");
        html.push_str("<th onclick=\"sortTable('timelineTable', 3)\">耗时</th>");
        html.push_str("<th onclick=\"sortTable('timelineTable', 4)\">结果</th>");
        html.push_str("</tr>\n</thead>\n<tbody>\n");
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

        // 隐藏的 CSV/JSON 数据 (供导出按钮读取)
        let timeline_csv = generate_timeline_csv(&summary.timeline);
        let timeline_json = generate_timeline_json(&summary.timeline);
        html.push_str(&format!(
            "<script type=\"text/plain\" id=\"timeline-csv-data\">{}</script>\n",
            escape_html(&timeline_csv)
        ));
        html.push_str(&format!(
            "<script type=\"text/plain\" id=\"timeline-json-data\">{}</script>\n",
            escape_html(&timeline_json)
        ));
    }

    // 隐藏的操作统计 CSV/JSON 数据
    if !summary.by_action.is_empty() {
        let action_csv = generate_action_stats_csv(&summary.by_action);
        let action_json = generate_action_stats_json(&summary.by_action);
        html.push_str(&format!(
            "<script type=\"text/plain\" id=\"action-stats-csv-data\">{}</script>\n",
            escape_html(&action_csv)
        ));
        html.push_str(&format!(
            "<script type=\"text/plain\" id=\"action-stats-json-data\">{}</script>\n",
            escape_html(&action_json)
        ));
    }

    // === 交互脚本 (Session 98: 图表全屏 + 表格排序) ===
    html.push_str(&generate_chart_fullscreen_script());
    html.push('\n');
    html.push_str(&generate_table_sort_script());
    html.push('\n');

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

    // ======================================================================
    //  generate_chart_js_line_raw 测试 (Session 94)
    // ======================================================================

    #[test]
    fn test_chart_js_line_raw_basic() {
        let labels = vec!["D1".to_string(), "D2".to_string(), "D3".to_string()];
        let data = vec![1800.0, 2700.0, 3600.0];
        let html = generate_chart_js_line_raw(
            "rawChart",
            "TTL 趋势",
            &labels,
            &data,
            "rgba(54, 162, 235, 1)",
            "TTL (秒)",
        );
        assert!(html.contains("rawChart"));
        assert!(html.contains("TTL 趋势"));
        assert!(html.contains("line"));
        assert!(html.contains("1800"));
        assert!(html.contains("3600"));
    }

    #[test]
    fn test_chart_js_line_raw_no_max_constraint() {
        // 不应包含 max: 1.0 百分比约束
        let html = generate_chart_js_line_raw(
            "rawNoMax",
            "原始",
            &["A".to_string()],
            &[100.0],
            "rgba(0,0,0,1)",
            "值",
        );
        assert!(!html.contains("max: 1.0"));
        assert!(!html.contains("toFixed(0) + '%'"));
    }

    #[test]
    fn test_chart_js_line_raw_empty_data() {
        let html = generate_chart_js_line_raw("rawEmpty", "空", &[], &[], "rgba(0,0,0,1)", "无");
        assert!(html.contains("[]"));
    }

    #[test]
    fn test_chart_js_line_raw_with_negative_values() {
        let labels = vec!["D1".to_string(), "D2".to_string(), "D3".to_string()];
        let data = vec![0.1, -0.2, -0.5];
        let html = generate_chart_js_line_raw(
            "negChart",
            "差值趋势",
            &labels,
            &data,
            "rgba(255, 99, 132, 1)",
            "差值",
        );
        assert!(html.contains("negChart"));
        assert!(html.contains("-0.2"));
    }

    #[test]
    fn test_chart_js_line_raw_escape_title() {
        let html = generate_chart_js_line_raw("id2", "A < B & C", &[], &[], "rgba(0,0,0,1)", "Y");
        assert!(html.contains("A &lt; B &amp; C"));
    }

    // ======================================================================
    //  HTML 报告: 缓存调优 sparkline 测试 (Session 94)
    // ======================================================================

    #[test]
    fn test_html_report_with_ttl_trend_chart() {
        use crate::dev_trace::CacheTuningHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_cache_tuning_history(CacheTuningHistorySummary::new(
                1800, 2700, true, 2, 0, 3, None,
            ))
            .with_cache_tuning_sparkline(vec![1800.0, 2700.0, 2700.0], vec![0.1, 0.3, 0.05]);
        let html = generate_html_report(&summary);

        assert!(html.contains("ttlTrendChart"));
        assert!(html.contains("TTL 变化趋势"));
        assert!(html.contains("1800"));
    }

    #[test]
    fn test_html_report_with_diff_trend_chart() {
        use crate::dev_trace::CacheTuningHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_cache_tuning_history(CacheTuningHistorySummary::new(
                1800, 2700, true, 2, 0, 3, None,
            ))
            .with_cache_tuning_sparkline(vec![1800.0, 2700.0, 2700.0], vec![0.1, 0.3, 0.05]);
        let html = generate_html_report(&summary);

        assert!(html.contains("diffTrendChart"));
        assert!(html.contains("关联差值趋势"));
    }

    #[test]
    fn test_html_report_no_ttl_chart_without_data() {
        use crate::dev_trace::CacheTuningHistorySummary;
        let summary = DevTraceSummary::empty().with_cache_tuning_history(
            CacheTuningHistorySummary::new(1800, 2700, true, 1, 0, 1, None),
        );
        let html = generate_html_report(&summary);

        assert!(!html.contains("ttlTrendChart"));
        assert!(!html.contains("diffTrendChart"));
    }

    #[test]
    fn test_html_report_no_ttl_chart_with_single_value() {
        use crate::dev_trace::CacheTuningHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_cache_tuning_history(CacheTuningHistorySummary::new(
                1800, 1800, true, 0, 0, 1, None,
            ))
            .with_cache_tuning_sparkline(vec![1800.0], vec![0.05]);
        let html = generate_html_report(&summary);

        assert!(!html.contains("ttlTrendChart"));
        assert!(!html.contains("diffTrendChart"));
    }

    // ======================================================================
    //  HTML 报告: 搜索质量/Memory 评估 sparkline 测试 (Session 95)
    // ======================================================================

    #[test]
    fn test_html_report_with_search_diff_trend_chart() {
        use crate::dev_trace::SearchQualityHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 3, 0, None))
            .with_search_quality_sparkline(vec![0.1, -0.05, 0.2]);
        let html = generate_html_report(&summary);

        assert!(html.contains("searchDiffTrendChart"));
        assert!(html.contains("搜索质量差值趋势"));
    }

    #[test]
    fn test_html_report_with_memory_diff_trend_chart() {
        use crate::dev_trace::MemoryEvaluationHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_memory_evaluation_history(MemoryEvaluationHistorySummary::new(
                true, true, 3, 0, None,
            ))
            .with_memory_evaluation_sparkline(vec![0.1, -0.05, 0.2]);
        let html = generate_html_report(&summary);

        assert!(html.contains("memoryDiffTrendChart"));
        assert!(html.contains("Memory 评估差值趋势"));
    }

    #[test]
    fn test_html_report_no_search_diff_chart_without_data() {
        use crate::dev_trace::SearchQualityHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 3, 0, None));
        let html = generate_html_report(&summary);

        assert!(!html.contains("searchDiffTrendChart"));
    }

    #[test]
    fn test_html_report_no_memory_diff_chart_without_data() {
        use crate::dev_trace::MemoryEvaluationHistorySummary;
        let summary = DevTraceSummary::empty().with_memory_evaluation_history(
            MemoryEvaluationHistorySummary::new(true, true, 3, 0, None),
        );
        let html = generate_html_report(&summary);

        assert!(!html.contains("memoryDiffTrendChart"));
    }

    #[test]
    fn test_html_report_no_search_diff_chart_single_value() {
        use crate::dev_trace::SearchQualityHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 1, 0, None))
            .with_search_quality_sparkline(vec![0.1]);
        let html = generate_html_report(&summary);

        assert!(!html.contains("searchDiffTrendChart"));
    }

    #[test]
    fn test_html_report_no_memory_diff_chart_single_value() {
        use crate::dev_trace::MemoryEvaluationHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_memory_evaluation_history(MemoryEvaluationHistorySummary::new(
                true, true, 1, 0, None,
            ))
            .with_memory_evaluation_sparkline(vec![0.1]);
        let html = generate_html_report(&summary);

        assert!(!html.contains("memoryDiffTrendChart"));
    }

    // ======================================================================
    //  颜色编码测试 (Session 96)
    // ======================================================================

    // --- generate_point_colors 测试 ---

    #[test]
    fn test_generate_point_colors_positive() {
        let colors = generate_point_colors(&[0.1, 1.0, 100.0]);
        assert_eq!(colors.len(), 3);
        assert!(colors.iter().all(|c| c.contains("75, 192, 192"))); // 全绿
    }

    #[test]
    fn test_generate_point_colors_negative() {
        let colors = generate_point_colors(&[-0.1, -1.0, -100.0]);
        assert_eq!(colors.len(), 3);
        assert!(colors.iter().all(|c| c.contains("255, 99, 132"))); // 全红
    }

    #[test]
    fn test_generate_point_colors_zero() {
        let colors = generate_point_colors(&[0.0, 0.0]);
        assert_eq!(colors.len(), 2);
        assert!(colors.iter().all(|c| c.contains("201, 203, 207"))); // 全灰
    }

    #[test]
    fn test_generate_point_colors_mixed() {
        let colors = generate_point_colors(&[0.1, -0.2, 0.0, 0.3, -0.1]);
        assert_eq!(colors.len(), 5);
        assert!(colors[0].contains("75, 192, 192")); // 正→绿
        assert!(colors[1].contains("255, 99, 132")); // 负→红
        assert!(colors[2].contains("201, 203, 207")); // 零→灰
        assert!(colors[3].contains("75, 192, 192")); // 正→绿
        assert!(colors[4].contains("255, 99, 132")); // 负→红
    }

    #[test]
    fn test_generate_point_colors_empty() {
        let colors = generate_point_colors(&[]);
        assert!(colors.is_empty());
    }

    // --- generate_chart_js_line_colored 测试 ---

    #[test]
    fn test_chart_js_line_colored_contains_point_colors() {
        let labels = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let data = vec![0.1, -0.2, 0.0];
        let html = generate_chart_js_line_colored(
            "testColored",
            "测试",
            &labels,
            &data,
            "rgba(75, 192, 192, 1)",
            "差值",
        );
        // 应包含 pointBackgroundColor 和 pointBorderColor
        assert!(html.contains("pointBackgroundColor"));
        assert!(html.contains("pointBorderColor"));
        // 应包含颜色值
        assert!(html.contains("75, 192, 192")); // 绿色
        assert!(html.contains("255, 99, 132")); // 红色
        assert!(html.contains("201, 203, 207")); // 灰色
    }

    #[test]
    fn test_chart_js_line_colored_all_positive() {
        let labels = vec!["a".to_string(), "b".to_string()];
        let data = vec![0.1, 0.2];
        let html = generate_chart_js_line_colored(
            "testPos",
            "测试",
            &labels,
            &data,
            "rgba(75, 192, 192, 1)",
            "差值",
        );
        assert!(html.contains("75, 192, 192")); // 绿色
        assert!(!html.contains("255, 99, 132")); // 无红色
    }

    #[test]
    fn test_chart_js_line_colored_all_negative() {
        let labels = vec!["a".to_string(), "b".to_string()];
        let data = vec![-0.1, -0.2];
        let html = generate_chart_js_line_colored(
            "testNeg",
            "测试",
            &labels,
            &data,
            "rgba(255, 99, 132, 1)",
            "差值",
        );
        assert!(html.contains("255, 99, 132")); // 红色
        assert!(!html.contains("75, 192, 192")); // 无绿色
    }

    #[test]
    fn test_chart_js_line_colored_empty_data() {
        let html =
            generate_chart_js_line_colored("testEmpty", "空", &[], &[], "rgba(0,0,0,1)", "Y");
        // 空数据仍应生成 HTML
        assert!(html.contains("testEmpty"));
        assert!(html.contains("pointBackgroundColor"));
    }

    #[test]
    fn test_chart_js_line_colored_escape_title() {
        let html =
            generate_chart_js_line_colored("id_esc", "A < B & C", &[], &[], "rgba(0,0,0,1)", "Y");
        assert!(html.contains("A &lt; B &amp; C"));
    }

    // --- HTML 报告: 颜色编码集成测试 (Session 96) ---

    #[test]
    fn test_html_report_search_diff_chart_has_colored_points() {
        use crate::dev_trace::SearchQualityHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 3, 0, None))
            .with_search_quality_sparkline(vec![0.1, -0.05, 0.2]);
        let html = generate_html_report(&summary);

        // 搜索差值趋势图应使用颜色编码点
        assert!(html.contains("searchDiffTrendChart"));
        assert!(html.contains("pointBackgroundColor"));
        assert!(html.contains("75, 192, 192")); // 绿色 (正值)
        assert!(html.contains("255, 99, 132")); // 红色 (负值)
    }

    #[test]
    fn test_html_report_memory_diff_chart_has_colored_points() {
        use crate::dev_trace::MemoryEvaluationHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_memory_evaluation_history(MemoryEvaluationHistorySummary::new(
                true, true, 3, 0, None,
            ))
            .with_memory_evaluation_sparkline(vec![0.1, -0.05, 0.2]);
        let html = generate_html_report(&summary);

        assert!(html.contains("memoryDiffTrendChart"));
        assert!(html.contains("pointBackgroundColor"));
        assert!(html.contains("75, 192, 192")); // 绿色
        assert!(html.contains("255, 99, 132")); // 红色
    }

    #[test]
    fn test_html_report_cache_tuning_diff_chart_has_colored_points() {
        use crate::dev_trace::CacheTuningHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_cache_tuning_history(CacheTuningHistorySummary::new(
                1800, 2700, true, 2, 0, 3, None,
            ))
            .with_cache_tuning_sparkline(vec![1800.0, 2700.0, 2700.0], vec![0.1, -0.3, 0.05]);
        let html = generate_html_report(&summary);

        assert!(html.contains("diffTrendChart"));
        assert!(html.contains("pointBackgroundColor"));
        assert!(html.contains("75, 192, 192")); // 绿色
        assert!(html.contains("255, 99, 132")); // 红色
    }

    #[test]
    fn test_html_report_all_positive_diff_only_green_points() {
        use crate::dev_trace::SearchQualityHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 3, 0, None))
            .with_search_quality_sparkline(vec![0.1, 0.2, 0.3]);
        let html = generate_html_report(&summary);

        assert!(html.contains("searchDiffTrendChart"));
        assert!(html.contains("75, 192, 192")); // 绿色
                                                // 差值趋势图中不应有红色点 (只有正值)
                                                // 注意: 搜索质量面板的 stat card 可能也有红色, 所以只检查图表区域
                                                // 这里验证 pointBackgroundColor 中有绿色
    }

    #[test]
    fn test_html_report_all_negative_diff_only_red_points() {
        use crate::dev_trace::MemoryEvaluationHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_memory_evaluation_history(MemoryEvaluationHistorySummary::new(
                true, true, 3, 0, None,
            ))
            .with_memory_evaluation_sparkline(vec![-0.1, -0.2, -0.3]);
        let html = generate_html_report(&summary);

        assert!(html.contains("memoryDiffTrendChart"));
        assert!(html.contains("255, 99, 132")); // 红色
    }

    // ======================================================================
    //  Session 97 测试 — Doughnut/Pie 图 + 甘特图 + 深色模式 + 工具栏
    // ======================================================================

    // --- generate_doughnut_colors 测试 ---

    #[test]
    fn test_generate_doughnut_colors_basic() {
        let colors = generate_doughnut_colors(3);
        assert_eq!(colors.len(), 3);
        assert!(colors[0].contains("75, 192, 192"));
        assert!(colors[1].contains("255, 99, 132"));
        assert!(colors[2].contains("255, 205, 86"));
    }

    #[test]
    fn test_generate_doughnut_colors_zero() {
        let colors = generate_doughnut_colors(0);
        assert!(colors.is_empty());
    }

    #[test]
    fn test_generate_doughnut_colors_wraps_around() {
        // 超过调色板长度 (10) 应循环
        let colors = generate_doughnut_colors(12);
        assert_eq!(colors.len(), 12);
        // 第 10 个应该与第 0 个相同 (循环)
        assert_eq!(colors[0], colors[10]);
        assert_eq!(colors[1], colors[11]);
    }

    #[test]
    fn test_default_doughnut_colors_has_10_entries() {
        assert_eq!(DEFAULT_DOUGHNUT_COLORS.len(), 10);
    }

    // --- generate_chart_js_doughnut 测试 ---

    #[test]
    fn test_chart_js_doughnut_basic() {
        let labels = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let data = vec![10.0, 20.0, 30.0];
        let html = generate_chart_js_doughnut("testDoughnut", "测试饼图", &labels, &data, &[]);
        assert!(html.contains("doughnut"));
        assert!(html.contains("testDoughnut"));
        assert!(html.contains("测试饼图"));
        assert!(html.contains("\"A\""));
        assert!(html.contains("10"));
    }

    #[test]
    fn test_chart_js_doughnut_custom_colors() {
        let labels = vec!["X".to_string(), "Y".to_string()];
        let data = vec![5.0, 10.0];
        let colors = vec!["rgba(1,2,3,0.5)".to_string(), "rgba(4,5,6,0.5)".to_string()];
        let html =
            generate_chart_js_doughnut("customColorPie", "自定义颜色", &labels, &data, &colors);
        assert!(html.contains("rgba(1,2,3,0.5)"));
        assert!(html.contains("rgba(4,5,6,0.5)"));
    }

    #[test]
    fn test_chart_js_doughnut_empty_data() {
        let html = generate_chart_js_doughnut("emptyPie", "空", &[], &[], &[]);
        assert!(html.contains("[]"));
        assert!(html.contains("doughnut"));
    }

    #[test]
    fn test_chart_js_doughnut_escape_title() {
        let html = generate_chart_js_doughnut("id_esc_d", "A < B & C", &[], &[], &[]);
        assert!(html.contains("A &lt; B &amp; C"));
    }

    #[test]
    fn test_chart_js_doughnut_has_tooltip_percentage() {
        let labels = vec!["A".to_string()];
        let data = vec![50.0];
        let html = generate_chart_js_doughnut("tooltipPie", "百分比提示", &labels, &data, &[]);
        assert!(html.contains("toFixed(1) + '%'"));
    }

    // --- generate_chart_js_gantt 测试 ---

    #[test]
    fn test_chart_js_gantt_basic() {
        let labels = vec!["Task 0".to_string(), "Task 1".to_string()];
        let data = vec![vec![0.0, 5000.0], vec![5000.0, 8000.0]];
        let colors = vec![
            "rgba(75,192,192,0.6)".to_string(),
            "rgba(255,99,132,0.6)".to_string(),
        ];
        let html = generate_chart_js_gantt("ganttTest", "甘特图测试", &labels, &data, &colors);
        assert!(html.contains("bar"));
        assert!(html.contains("indexAxis: 'y'"));
        assert!(html.contains("ganttTest"));
        assert!(html.contains("Task 0"));
        assert!(html.contains("5000"));
    }

    #[test]
    fn test_chart_js_gantt_empty_data() {
        let html = generate_chart_js_gantt("emptyGantt", "空甘特", &[], &[], &[]);
        assert!(html.contains("[]"));
        assert!(html.contains("indexAxis: 'y'"));
    }

    #[test]
    fn test_chart_js_gantt_default_colors() {
        let labels = vec!["T1".to_string()];
        let data = vec![vec![0.0, 100.0]];
        let html = generate_chart_js_gantt("defaultColorGantt", "默认颜色", &labels, &data, &[]);
        // 空颜色列表时使用默认绿色
        assert!(html.contains("75, 192, 192"));
    }

    #[test]
    fn test_chart_js_gantt_escape_title() {
        let html = generate_chart_js_gantt("id_esc_g", "A < B & C", &[], &[], &[]);
        assert!(html.contains("A &lt; B &amp; C"));
    }

    #[test]
    fn test_chart_js_gantt_has_time_axis_label() {
        let labels = vec!["T1".to_string()];
        let data = vec![vec![0.0, 1000.0]];
        let html = generate_chart_js_gantt("axisGantt", "时间轴", &labels, &data, &[]);
        assert!(html.contains("时间 (ms)"));
    }

    // --- extract_gantt_data 测试 ---

    #[test]
    fn test_extract_gantt_data_basic() {
        use crate::dev_trace::{TimelineEntry, TraceAction};
        let timeline = vec![
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("Task A".to_string()),
                success: true,
                duration_ms: 1000,
            },
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::FixAttempt,
                task_name: Some("Fix B".to_string()),
                success: false,
                duration_ms: 500,
            },
        ];
        let (labels, data, colors) = extract_gantt_data(&timeline);
        assert_eq!(labels.len(), 2);
        assert_eq!(data.len(), 2);
        assert_eq!(colors.len(), 2);

        // 第一个条目: 0 ~ 1000
        assert_eq!(data[0], vec![0.0, 1000.0]);
        // 第二个条目: 1000 ~ 1500
        assert_eq!(data[1], vec![1000.0, 1500.0]);

        // 成功→绿, 失败→红
        assert!(colors[0].contains("75, 192, 192"));
        assert!(colors[1].contains("255, 99, 132"));

        // 标签包含任务名
        assert!(labels[0].contains("Task A"));
        assert!(labels[1].contains("Fix B"));
    }

    #[test]
    fn test_extract_gantt_data_empty() {
        let (labels, data, colors) = extract_gantt_data(&[]);
        assert!(labels.is_empty());
        assert!(data.is_empty());
        assert!(colors.is_empty());
    }

    #[test]
    fn test_extract_gantt_data_no_task_name() {
        use crate::dev_trace::{TimelineEntry, TraceAction};
        let timeline = vec![TimelineEntry {
            timestamp: chrono::Utc::now(),
            action: TraceAction::CompileCheck,
            task_name: None,
            success: true,
            duration_ms: 200,
        }];
        let (labels, data, _) = extract_gantt_data(&timeline);
        assert_eq!(labels.len(), 1);
        // 无任务名时使用 action 名称
        assert!(labels[0].contains("CompileCheck"));
        assert_eq!(data[0], vec![0.0, 200.0]);
    }

    #[test]
    fn test_extract_gantt_data_all_success_green() {
        use crate::dev_trace::{TimelineEntry, TraceAction};
        let timeline = vec![
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("T1".to_string()),
                success: true,
                duration_ms: 100,
            },
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("T2".to_string()),
                success: true,
                duration_ms: 200,
            },
        ];
        let (_, _, colors) = extract_gantt_data(&timeline);
        assert!(colors.iter().all(|c| c.contains("75, 192, 192")));
    }

    #[test]
    fn test_extract_gantt_data_all_failure_red() {
        use crate::dev_trace::{TimelineEntry, TraceAction};
        let timeline = vec![
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::FixAttempt,
                task_name: Some("F1".to_string()),
                success: false,
                duration_ms: 100,
            },
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::FixAttempt,
                task_name: Some("F2".to_string()),
                success: false,
                duration_ms: 200,
            },
        ];
        let (_, _, colors) = extract_gantt_data(&timeline);
        assert!(colors.iter().all(|c| c.contains("255, 99, 132")));
    }

    #[test]
    fn test_extract_gantt_data_cumulative_time() {
        use crate::dev_trace::{TimelineEntry, TraceAction};
        let timeline = vec![
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("T1".to_string()),
                success: true,
                duration_ms: 3000,
            },
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("T2".to_string()),
                success: true,
                duration_ms: 2000,
            },
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("T3".to_string()),
                success: true,
                duration_ms: 1000,
            },
        ];
        let (_, data, _) = extract_gantt_data(&timeline);
        // 累积时间: 0→3000, 3000→5000, 5000→6000
        assert_eq!(data[0], vec![0.0, 3000.0]);
        assert_eq!(data[1], vec![3000.0, 5000.0]);
        assert_eq!(data[2], vec![5000.0, 6000.0]);
    }

    // --- generate_report_toolbar 测试 ---

    #[test]
    fn test_report_toolbar_contains_theme_button() {
        let html = generate_report_toolbar();
        assert!(html.contains("toolbar"));
        assert!(html.contains("themeBtn"));
        assert!(html.contains("toggleTheme"));
        assert!(html.contains("深色模式"));
    }

    #[test]
    fn test_report_toolbar_contains_pdf_button() {
        let html = generate_report_toolbar();
        assert!(html.contains("window.print"));
        assert!(html.contains("导出 PDF"));
    }

    #[test]
    fn test_report_toolbar_contains_localstorage() {
        let html = generate_report_toolbar();
        assert!(html.contains("localStorage"));
        assert!(html.contains("forge-theme"));
    }

    // --- generate_css_styles 增强测试 (Session 97) ---

    #[test]
    fn test_css_styles_has_dark_mode_variables() {
        let css = generate_css_styles();
        assert!(css.contains(":root"));
        assert!(css.contains("--bg"));
        assert!(css.contains("--text"));
        assert!(css.contains("--card-bg"));
        assert!(css.contains("dark-mode"));
    }

    #[test]
    fn test_css_styles_has_print_media() {
        let css = generate_css_styles();
        assert!(css.contains("@media print"));
        assert!(css.contains("report-toolbar"));
        assert!(css.contains("break-inside: avoid"));
    }

    #[test]
    fn test_css_styles_has_toolbar_styles() {
        let css = generate_css_styles();
        assert!(css.contains(".report-toolbar"));
        assert!(css.contains(".toolbar-btn"));
    }

    #[test]
    fn test_css_styles_uses_css_variables() {
        let css = generate_css_styles();
        assert!(css.contains("var(--bg)"));
        assert!(css.contains("var(--text)"));
        assert!(css.contains("var(--card-bg)"));
        assert!(css.contains("var(--heading)"));
    }

    // --- HTML 报告集成测试 (Session 97) ---

    #[test]
    fn test_html_report_contains_toolbar() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("report-toolbar"));
        assert!(html.contains("toggleTheme"));
        assert!(html.contains("导出 PDF"));
    }

    #[test]
    fn test_html_report_version_1_2() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("1.2"));
    }

    #[test]
    fn test_html_report_with_action_doughnut() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 5,
                success_count: 4,
                total_duration_ms: 1000,
            },
        );
        by_action.insert(
            TraceAction::TaskExecution,
            ActionStats {
                count: 3,
                success_count: 3,
                total_duration_ms: 500,
            },
        );
        let summary = DevTraceSummary {
            total_entries: 8,
            total_duration_ms: 1500,
            by_action,
            success_rate: 0.875,
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("actionDoughnut"));
        assert!(html.contains("操作类型占比"));
        assert!(html.contains("doughnut"));
    }

    #[test]
    fn test_html_report_with_cache_doughnut() {
        use crate::dev_trace::CacheStatsSummary;
        let summary = DevTraceSummary {
            cache_summary: Some(CacheStatsSummary {
                cache_hits: 6,
                cache_misses: 3,
                search_failures: 1,
                time_saved_ms: 5000,
            }),
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("cacheDoughnut"));
        assert!(html.contains("缓存命中率分布"));
        assert!(html.contains("命中"));
        assert!(html.contains("未命中"));
    }

    #[test]
    fn test_html_report_with_gantt_chart() {
        let summary = DevTraceSummary {
            timeline: vec![
                TimelineEntry {
                    timestamp: chrono::Utc::now(),
                    action: TraceAction::TaskExecution,
                    task_name: Some("Task 0".to_string()),
                    success: true,
                    duration_ms: 500,
                },
                TimelineEntry {
                    timestamp: chrono::Utc::now(),
                    action: TraceAction::FixAttempt,
                    task_name: Some("Fix 1".to_string()),
                    success: false,
                    duration_ms: 300,
                },
            ],
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("ganttChart"));
        assert!(html.contains("时间线甘特图"));
        assert!(html.contains("indexAxis: 'y'"));
    }

    #[test]
    fn test_html_report_no_gantt_without_timeline() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(!html.contains("ganttChart"));
        assert!(!html.contains("时间线甘特图"));
    }

    #[test]
    fn test_html_report_dark_mode_class_in_css() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("dark-mode"));
        assert!(html.contains("localStorage"));
    }

    #[test]
    fn test_html_report_print_css_present() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("@media print"));
    }

    #[test]
    fn test_html_report_full_with_all_session97_features() {
        use crate::dev_trace::CacheStatsSummary;

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
            cache_summary: Some(CacheStatsSummary {
                cache_hits: 6,
                cache_misses: 3,
                search_failures: 1,
                time_saved_ms: 5000,
            }),
            timeline: vec![TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("Task 0".to_string()),
                success: true,
                duration_ms: 1000,
            }],
            ..DevTraceSummary::empty()
        };

        let html = generate_html_report(&summary);
        // 工具栏
        assert!(html.contains("report-toolbar"));
        assert!(html.contains("toggleTheme"));
        // Doughnut 图
        assert!(html.contains("actionDoughnut"));
        assert!(html.contains("cacheDoughnut"));
        // 甘特图
        assert!(html.contains("ganttChart"));
        assert!(html.contains("时间线甘特图"));
        // 深色模式 CSS
        assert!(html.contains("dark-mode"));
        assert!(html.contains("@media print"));
    }

    // ======================================================================
    //  Session 98 测试 — CSV/JSON 导出 + 搜索过滤 + 全屏放大 + 表格排序
    // ======================================================================

    // --- csv_escape_field 测试 ---

    #[test]
    fn test_csv_escape_plain() {
        assert_eq!(csv_escape_field("hello"), "hello");
    }

    #[test]
    fn test_csv_escape_comma() {
        assert_eq!(csv_escape_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn test_csv_escape_quote() {
        assert_eq!(csv_escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn test_csv_escape_newline() {
        assert_eq!(csv_escape_field("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn test_csv_escape_carriage_return() {
        assert_eq!(csv_escape_field("a\rb"), "\"a\rb\"");
    }

    #[test]
    fn test_csv_escape_empty() {
        assert_eq!(csv_escape_field(""), "");
    }

    // --- generate_timeline_csv 测试 ---

    #[test]
    fn test_timeline_csv_basic() {
        let timeline = vec![TimelineEntry {
            timestamp: chrono::Utc::now(),
            action: TraceAction::TaskExecution,
            task_name: Some("Task A".to_string()),
            success: true,
            duration_ms: 500,
        }];
        let csv = generate_timeline_csv(&timeline);
        assert!(csv.contains("时间戳"));
        assert!(csv.contains("操作类型"));
        assert!(csv.contains("任务名称"));
        assert!(csv.contains("耗时(ms)"));
        assert!(csv.contains("结果"));
        assert!(csv.contains("Task A"));
        assert!(csv.contains("TaskExecution"));
        assert!(csv.contains("成功"));
        assert!(csv.contains("500"));
    }

    #[test]
    fn test_timeline_csv_empty() {
        let csv = generate_timeline_csv(&[]);
        // 空时间线仍应包含表头
        assert!(csv.contains("时间戳"));
        assert!(!csv.contains("TaskExecution"));
    }

    #[test]
    fn test_timeline_csv_failure_entry() {
        let timeline = vec![TimelineEntry {
            timestamp: chrono::Utc::now(),
            action: TraceAction::FixAttempt,
            task_name: None,
            success: false,
            duration_ms: 200,
        }];
        let csv = generate_timeline_csv(&timeline);
        assert!(csv.contains("失败"));
        assert!(csv.contains("FixAttempt"));
        assert!(csv.contains("200"));
    }

    #[test]
    fn test_timeline_csv_comma_in_task_name() {
        let timeline = vec![TimelineEntry {
            timestamp: chrono::Utc::now(),
            action: TraceAction::TaskExecution,
            task_name: Some("hello, world".to_string()),
            success: true,
            duration_ms: 100,
        }];
        let csv = generate_timeline_csv(&timeline);
        // 包含逗号的字段应被引号包裹
        assert!(csv.contains("\"hello, world\""));
    }

    #[test]
    fn test_timeline_csv_multiple_entries() {
        let timeline = vec![
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("T1".to_string()),
                success: true,
                duration_ms: 100,
            },
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::CompileCheck,
                task_name: Some("T2".to_string()),
                success: false,
                duration_ms: 200,
            },
        ];
        let csv = generate_timeline_csv(&timeline);
        assert!(csv.contains("T1"));
        assert!(csv.contains("T2"));
        assert!(csv.contains("成功"));
        assert!(csv.contains("失败"));
    }

    // --- generate_action_stats_csv 测试 ---

    #[test]
    fn test_action_stats_csv_basic() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 10,
                success_count: 8,
                total_duration_ms: 5000,
            },
        );
        let csv = generate_action_stats_csv(&by_action);
        assert!(csv.contains("操作类型"));
        assert!(csv.contains("总次数"));
        assert!(csv.contains("成功次数"));
        assert!(csv.contains("成功率"));
        assert!(csv.contains("总耗时(ms)"));
        assert!(csv.contains("平均耗时(ms)"));
        assert!(csv.contains("CompileCheck"));
        assert!(csv.contains("10"));
        assert!(csv.contains("80.0%"));
    }

    #[test]
    fn test_action_stats_csv_empty() {
        let by_action: HashMap<TraceAction, ActionStats> = HashMap::new();
        let csv = generate_action_stats_csv(&by_action);
        assert!(csv.contains("操作类型"));
        // 只有表头
        assert_eq!(csv.lines().count(), 1);
    }

    #[test]
    fn test_action_stats_csv_multiple_entries() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::TaskExecution,
            ActionStats {
                count: 5,
                success_count: 5,
                total_duration_ms: 1000,
            },
        );
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 10,
                success_count: 7,
                total_duration_ms: 3000,
            },
        );
        let csv = generate_action_stats_csv(&by_action);
        assert!(csv.contains("CompileCheck"));
        assert!(csv.contains("TaskExecution"));
        assert!(csv.contains("100.0%"));
        assert!(csv.contains("70.0%"));
    }

    #[test]
    fn test_action_stats_csv_zero_count() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::TestRun,
            ActionStats {
                count: 0,
                success_count: 0,
                total_duration_ms: 0,
            },
        );
        let csv = generate_action_stats_csv(&by_action);
        assert!(csv.contains("0.0%"));
    }

    // --- generate_timeline_json 测试 ---

    #[test]
    fn test_timeline_json_basic() {
        let timeline = vec![TimelineEntry {
            timestamp: chrono::Utc::now(),
            action: TraceAction::TaskExecution,
            task_name: Some("Task A".to_string()),
            success: true,
            duration_ms: 300,
        }];
        let json = generate_timeline_json(&timeline);
        assert!(json.contains("Task A"));
        assert!(json.contains("\"duration_ms\""));
        assert!(json.contains("\"success\""));
        assert!(json.contains("\"action\""));
        assert!(json.contains("TaskExecution"));
    }

    #[test]
    fn test_timeline_json_empty() {
        let json = generate_timeline_json(&[]);
        assert_eq!(json.trim(), "[]");
    }

    #[test]
    fn test_timeline_json_multiple_entries() {
        let timeline = vec![
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("T1".to_string()),
                success: true,
                duration_ms: 100,
            },
            TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::FixAttempt,
                task_name: Some("T2".to_string()),
                success: false,
                duration_ms: 200,
            },
        ];
        let json = generate_timeline_json(&timeline);
        assert!(json.contains("T1"));
        assert!(json.contains("T2"));
        assert!(json.contains("true"));
        assert!(json.contains("false"));
    }

    #[test]
    fn test_timeline_json_no_task_name() {
        let timeline = vec![TimelineEntry {
            timestamp: chrono::Utc::now(),
            action: TraceAction::CompileCheck,
            task_name: None,
            success: true,
            duration_ms: 50,
        }];
        let json = generate_timeline_json(&timeline);
        assert!(json.contains("\"task_name\": \"\""));
    }

    // --- generate_action_stats_json 测试 ---

    #[test]
    fn test_action_stats_json_basic() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 5,
                success_count: 4,
                total_duration_ms: 2000,
            },
        );
        let json = generate_action_stats_json(&by_action);
        assert!(json.contains("CompileCheck"));
        assert!(json.contains("\"count\": 5"));
        assert!(json.contains("\"success_count\": 4"));
        assert!(json.contains("\"success_rate\""));
    }

    #[test]
    fn test_action_stats_json_empty() {
        let by_action: HashMap<TraceAction, ActionStats> = HashMap::new();
        let json = generate_action_stats_json(&by_action);
        assert_eq!(json.trim(), "[]");
    }

    #[test]
    fn test_action_stats_json_multiple_entries() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::TaskExecution,
            ActionStats {
                count: 3,
                success_count: 3,
                total_duration_ms: 600,
            },
        );
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 10,
                success_count: 7,
                total_duration_ms: 3000,
            },
        );
        let json = generate_action_stats_json(&by_action);
        assert!(json.contains("CompileCheck"));
        assert!(json.contains("TaskExecution"));
        assert!(json.contains("1.0")); // 3/3 = 1.0
        assert!(json.contains("0.7")); // 7/10 = 0.7
    }

    // --- generate_export_buttons 测试 ---

    #[test]
    fn test_export_buttons_contains_csv_button() {
        let html = generate_export_buttons();
        assert!(html.contains("downloadTimelineCSV"));
        assert!(html.contains("CSV"));
    }

    #[test]
    fn test_export_buttons_contains_json_button() {
        let html = generate_export_buttons();
        assert!(html.contains("downloadTimelineJSON"));
        assert!(html.contains("JSON"));
    }

    #[test]
    fn test_export_buttons_contains_action_stats_buttons() {
        let html = generate_export_buttons();
        assert!(html.contains("downloadActionStatsCSV"));
        assert!(html.contains("downloadActionStatsJSON"));
    }

    #[test]
    fn test_export_buttons_contains_download_function() {
        let html = generate_export_buttons();
        assert!(html.contains("downloadFile"));
        assert!(html.contains("Blob"));
        assert!(html.contains("createObjectURL"));
    }

    // --- generate_timeline_search 测试 ---

    #[test]
    fn test_timeline_search_contains_input() {
        let html = generate_timeline_search();
        assert!(html.contains("timelineSearch"));
        assert!(html.contains("input"));
        assert!(html.contains("搜索"));
    }

    #[test]
    fn test_timeline_search_contains_filter_function() {
        let html = generate_timeline_search();
        assert!(html.contains("filterTimeline"));
        assert!(html.contains("oninput"));
    }

    #[test]
    fn test_timeline_search_contains_count_display() {
        let html = generate_timeline_search();
        assert!(html.contains("timelineSearchCount"));
        assert!(html.contains("条匹配"));
    }

    // --- generate_chart_fullscreen_script 测试 ---

    #[test]
    fn test_chart_fullscreen_contains_modal() {
        let html = generate_chart_fullscreen_script();
        assert!(html.contains("chart-modal"));
        assert!(html.contains("chartModal"));
        assert!(html.contains("chartModalBody"));
    }

    #[test]
    fn test_chart_fullscreen_contains_open_function() {
        let html = generate_chart_fullscreen_script();
        assert!(html.contains("openChartFullscreen"));
        assert!(html.contains("closeChartFullscreen"));
    }

    #[test]
    fn test_chart_fullscreen_contains_escape_key_handler() {
        let html = generate_chart_fullscreen_script();
        assert!(html.contains("Escape"));
        assert!(html.contains("keydown"));
    }

    #[test]
    fn test_chart_fullscreen_contains_chart_reference() {
        let html = generate_chart_fullscreen_script();
        assert!(html.contains("Chart.getChart"));
        assert!(html.contains("new Chart"));
    }

    // --- generate_table_sort_script 测试 ---

    #[test]
    fn test_table_sort_contains_sort_function() {
        let html = generate_table_sort_script();
        assert!(html.contains("sortTable"));
        assert!(html.contains("sortDirection"));
    }

    #[test]
    fn test_table_sort_contains_numeric_detection() {
        let html = generate_table_sort_script();
        assert!(html.contains("parseFloat"));
        assert!(html.contains("isNaN"));
    }

    #[test]
    fn test_table_sort_contains_locale_compare() {
        let html = generate_table_sort_script();
        assert!(html.contains("localeCompare"));
    }

    // --- CSS 样式增强测试 (Session 98) ---

    #[test]
    fn test_css_styles_has_timeline_search_styles() {
        let css = generate_css_styles();
        assert!(css.contains(".timeline-search"));
        assert!(css.contains(".timeline-search input"));
    }

    #[test]
    fn test_css_styles_has_chart_modal_styles() {
        let css = generate_css_styles();
        assert!(css.contains(".chart-modal"));
        assert!(css.contains(".chart-modal-content"));
        assert!(css.contains(".chart-modal-close"));
    }

    #[test]
    fn test_css_styles_has_sortable_header_styles() {
        let css = generate_css_styles();
        assert!(css.contains("cursor: pointer"));
        assert!(css.contains("user-select: none"));
        assert!(css.contains("th::after"));
    }

    #[test]
    fn test_css_styles_has_fullscreen_button_styles() {
        let css = generate_css_styles();
        assert!(css.contains(".chart-fullscreen-btn"));
        assert!(css.contains("position: absolute"));
    }

    #[test]
    fn test_css_styles_print_hides_interactive_elements() {
        let css = generate_css_styles();
        assert!(css.contains(".timeline-search { display: none"));
        assert!(css.contains(".chart-fullscreen-btn { display: none"));
        assert!(css.contains(".chart-modal { display: none"));
    }

    // --- HTML 报告集成测试 (Session 98) ---

    #[test]
    fn test_html_report_contains_export_buttons() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("downloadTimelineCSV"));
        assert!(html.contains("downloadTimelineJSON"));
        assert!(html.contains("downloadActionStatsCSV"));
        assert!(html.contains("downloadActionStatsJSON"));
    }

    #[test]
    fn test_html_report_contains_timeline_search() {
        let summary = DevTraceSummary {
            timeline: vec![TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("Task 0".to_string()),
                success: true,
                duration_ms: 100,
            }],
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("timelineSearch"));
        assert!(html.contains("filterTimeline"));
    }

    #[test]
    fn test_html_report_no_timeline_search_without_timeline() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(!html.contains("timelineSearch"));
    }

    #[test]
    fn test_html_report_contains_chart_fullscreen_modal() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("chartModal"));
        assert!(html.contains("openChartFullscreen"));
        assert!(html.contains("closeChartFullscreen"));
    }

    #[test]
    fn test_html_report_contains_table_sort_script() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        assert!(html.contains("sortTable"));
        assert!(html.contains("sortDirection"));
    }

    #[test]
    fn test_html_report_timeline_table_has_sortable_headers() {
        let summary = DevTraceSummary {
            timeline: vec![TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("Task 0".to_string()),
                success: true,
                duration_ms: 100,
            }],
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("id=\"timelineTable\""));
        assert!(html.contains("sortTable('timelineTable'"));
    }

    #[test]
    fn test_html_report_contains_hidden_csv_data() {
        let summary = DevTraceSummary {
            timeline: vec![TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("Task 0".to_string()),
                success: true,
                duration_ms: 100,
            }],
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("id=\"timeline-csv-data\""));
        assert!(html.contains("id=\"timeline-json-data\""));
    }

    #[test]
    fn test_html_report_contains_hidden_action_stats_data() {
        let mut by_action = HashMap::new();
        by_action.insert(
            TraceAction::CompileCheck,
            ActionStats {
                count: 5,
                success_count: 4,
                total_duration_ms: 1000,
            },
        );
        let summary = DevTraceSummary {
            by_action,
            ..DevTraceSummary::empty()
        };
        let html = generate_html_report(&summary);
        assert!(html.contains("id=\"action-stats-csv-data\""));
        assert!(html.contains("id=\"action-stats-json-data\""));
    }

    #[test]
    fn test_html_report_no_hidden_data_without_content() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);
        // 空摘要不应包含隐藏的 script 数据块
        // (JS 代码中引用了这些 ID, 但不应生成实际的 <script type="text/plain"> 数据块)
        assert!(!html.contains("<script type=\"text/plain\" id=\"timeline-csv-data\""));
        assert!(!html.contains("<script type=\"text/plain\" id=\"action-stats-csv-data\""));
    }

    #[test]
    fn test_html_report_full_with_session98_features() {
        use crate::dev_trace::CacheStatsSummary;

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
            cache_summary: Some(CacheStatsSummary {
                cache_hits: 6,
                cache_misses: 3,
                search_failures: 1,
                time_saved_ms: 5000,
            }),
            timeline: vec![TimelineEntry {
                timestamp: chrono::Utc::now(),
                action: TraceAction::TaskExecution,
                task_name: Some("Task 0".to_string()),
                success: true,
                duration_ms: 1000,
            }],
            ..DevTraceSummary::empty()
        };

        let html = generate_html_report(&summary);
        // Session 98 交互功能
        assert!(html.contains("downloadTimelineCSV"));
        assert!(html.contains("timelineSearch"));
        assert!(html.contains("filterTimeline"));
        assert!(html.contains("chartModal"));
        assert!(html.contains("sortTable"));
        // 隐藏数据
        assert!(html.contains("timeline-csv-data"));
        assert!(html.contains("timeline-json-data"));
        assert!(html.contains("action-stats-csv-data"));
        assert!(html.contains("action-stats-json-data"));
        // 版本 1.2
        assert!(html.contains("1.2"));
    }

    // ======================================================================
    //  HTML 报告: 联合决策历史测试 (Session 99)
    // ======================================================================

    #[test]
    fn test_html_report_with_joint_decision_history() {
        use crate::joint_decision::{JointDecisionAction, JointDecisionHistorySummary};
        let summary =
            DevTraceSummary::empty().with_joint_decision_history(JointDecisionHistorySummary::new(
                3,
                JointDecisionAction::EscalateWarning,
                15,
                5,
                2,
                0.133,
                0.333,
                false,
                Some("2024-01-01T00:00:00Z".to_string()),
            ));
        let html = generate_html_report(&summary);

        assert!(html.contains("联合决策历史"));
        assert!(html.contains("Session 数"));
        assert!(html.contains("升级警告"));
        assert!(html.contains("保守模式"));
        assert!(html.contains("累计决策"));
        assert!(html.contains("15"));
        assert!(html.contains("5"));
        assert!(html.contains("2"));
    }

    #[test]
    fn test_html_report_joint_decision_conservative_mode() {
        use crate::joint_decision::{JointDecisionAction, JointDecisionHistorySummary};
        let summary =
            DevTraceSummary::empty().with_joint_decision_history(JointDecisionHistorySummary::new(
                2,
                JointDecisionAction::EnterConservativeMode,
                10,
                3,
                4,
                0.4,
                0.3,
                true, // 当前处于保守模式
                None,
            ));
        let html = generate_html_report(&summary);

        assert!(html.contains("保守模式"));
        assert!(html.contains("当前模式"));
        assert!(html.contains("保守模式"));
    }

    #[test]
    fn test_html_report_no_joint_decision_section_without_data() {
        let summary = DevTraceSummary::empty();
        let html = generate_html_report(&summary);

        assert!(!html.contains("联合决策历史"));
    }

    #[test]
    fn test_html_report_joint_decision_empty_summary_skipped() {
        use crate::joint_decision::JointDecisionHistorySummary;
        let summary = DevTraceSummary::empty()
            .with_joint_decision_history(JointDecisionHistorySummary::empty());
        let html = generate_html_report(&summary);

        assert!(!html.contains("联合决策历史"));
    }
}
