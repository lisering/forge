//! ASCII Sparkline 生成器 — 将数值序列渲染为紧凑的 Unicode 柱状图
//!
//! 使用 Unicode 方块字符 (▁▂▃▄▅▆▇█) 将数值序列渲染为单行 ASCII 图表,
//! 适用于终端输出和文本报告中展示趋势变化。
//!
//! ## 核心功能
//!
//! - [`render_sparkline`] — 将数值序列渲染为 sparkline 字符串
//! - [`compute_sparkline_stats`] — 计算数值序列的统计信息
//! - [`format_trend_sparkline`] — 格式化带标签和统计的完整趋势行
//! - [`format_multi_sparkline`] — 格式化多组 sparkline
//!
//! ## 字符集
//!
//! 8 级 Unicode 方块字符, 从低到高:
//!
//! ```text
//! ▁ ▂ ▃ ▄ ▅ ▆ ▇ █
//! 0 1 2 3 4 5 6 7
//! ```
//!
//! ## 示例
//!
//! ```
//! # use forge::sparkline::{render_sparkline, SparklineConfig};
//! let values = vec![0.1, 0.3, 0.5, 0.7, 0.9];
//! let config = SparklineConfig::default();
//! let sparkline = render_sparkline(&values, &config);
//! // "▁▃▅▇█"
//! assert!(sparkline.contains('█'));
//! assert!(sparkline.contains('▁'));
//! ```

use serde::{Deserialize, Serialize};

// ============================================================================
//  常量
// ============================================================================

/// Sparkline 字符集 — 8 级 Unicode 方块字符 (从低到高)
///
/// `▁` (U+2581) 到 `█` (U+2588), 共 8 个字符,
/// 将归一化后的 0.0~1.0 值映射到对应的字符。
pub const SPARKLINE_CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// 默认最大 sparkline 宽度 (字符数)
pub const DEFAULT_SPARKLINE_MAX_WIDTH: usize = 60;

/// 默认空值占位符
pub const DEFAULT_FILL_CHAR: char = '·';

/// 趋势阈值 — 差值小于此值视为 "平稳"
pub const TREND_THRESHOLD: f64 = 0.05;

// ============================================================================
//  SparklineConfig — 配置
// ============================================================================

/// Sparkline 渲染配置
///
/// 控制 sparkline 的渲染行为, 包括最大宽度、是否显示统计信息等。
///
/// # 字段
///
/// - `max_width`: 最大宽度 (字符数), 超出时保留最近的 N 个值
/// - `show_min_max`: 是否在末尾显示 min/max 标注
/// - `fill_char`: 空序列的占位字符
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{SparklineConfig, render_sparkline};
/// let config = SparklineConfig::new(40).with_min_max(true);
/// let values = vec![0.2, 0.4, 0.6, 0.8];
/// let s = render_sparkline(&values, &config);
/// assert!(!s.is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparklineConfig {
    /// 最大宽度 (字符数), 超出时保留最近的 N 个值
    pub max_width: usize,
    /// 是否在末尾显示 min/max 标注
    pub show_min_max: bool,
    /// 空序列的占位字符
    pub fill_char: char,
}

impl Default for SparklineConfig {
    fn default() -> Self {
        Self {
            max_width: DEFAULT_SPARKLINE_MAX_WIDTH,
            show_min_max: false,
            fill_char: DEFAULT_FILL_CHAR,
        }
    }
}

impl SparklineConfig {
    /// 创建指定最大宽度的配置
    ///
    /// # 参数
    ///
    /// - `max_width`: 最大宽度 (字符数), 建议 20~80
    pub fn new(max_width: usize) -> Self {
        Self {
            max_width,
            ..Default::default()
        }
    }

    /// 启用 min/max 标注
    pub fn with_min_max(mut self, show: bool) -> Self {
        self.show_min_max = show;
        self
    }

    /// 设置占位字符
    pub fn with_fill_char(mut self, ch: char) -> Self {
        self.fill_char = ch;
        self
    }
}

// ============================================================================
//  SparklineStats — 统计信息
// ============================================================================

/// 趋势方向 — 数值序列的整体变化趋势
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrendDirection {
    /// 上升趋势
    Up,
    /// 下降趋势
    Down,
    /// 平稳 (变化小于阈值)
    Flat,
    /// 数据不足 (少于 2 个值)
    Insufficient,
}

impl std::fmt::Display for TrendDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.arrow())
    }
}

impl TrendDirection {
    /// 获取趋势箭头符号
    pub fn arrow(&self) -> &'static str {
        match self {
            Self::Up => "↑",
            Self::Down => "↓",
            Self::Flat => "→",
            Self::Insufficient => "?",
        }
    }

    /// 获取趋势中文标签
    pub fn label(&self) -> &'static str {
        match self {
            Self::Up => "上升",
            Self::Down => "下降",
            Self::Flat => "平稳",
            Self::Insufficient => "数据不足",
        }
    }
}

/// 数值序列的统计信息
///
/// 由 [`compute_sparkline_stats`] 计算, 包含最小值、最大值、平均值、
/// 极差和趋势方向。
///
/// # 字段
///
/// - `min`: 最小值
/// - `max`: 最大值
/// - `avg`: 平均值
/// - `range`: 极差 (max - min)
/// - `trend`: 趋势方向
/// - `first`: 第一个值
/// - `last`: 最后一个值
/// - `count`: 值的数量
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{compute_sparkline_stats, TrendDirection};
/// let stats = compute_sparkline_stats(&[0.2, 0.4, 0.6, 0.8]);
/// assert!((stats.min - 0.2).abs() < 0.001);
/// assert!((stats.max - 0.8).abs() < 0.001);
/// assert!((stats.avg - 0.5).abs() < 0.001);
/// assert_eq!(stats.trend, TrendDirection::Up);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparklineStats {
    /// 最小值
    pub min: f64,
    /// 最大值
    pub max: f64,
    /// 平均值
    pub avg: f64,
    /// 极差 (max - min)
    pub range: f64,
    /// 趋势方向
    pub trend: TrendDirection,
    /// 第一个值
    pub first: f64,
    /// 最后一个值
    pub last: f64,
    /// 值的数量
    pub count: usize,
}

impl SparklineStats {
    /// 计算首尾差值 (last - first)
    pub fn delta(&self) -> f64 {
        self.last - self.first
    }

    /// 是否有足够数据
    pub fn has_data(&self) -> bool {
        self.count >= 2
    }

    /// 格式化为简要摘要
    pub fn to_summary(&self) -> String {
        if self.count == 0 {
            return "无数据".to_string();
        }
        format!(
            "{:.2}~{:.2} (avg {:.2}, {} {})",
            self.min,
            self.max,
            self.avg,
            self.trend.label(),
            self.trend.arrow()
        )
    }
}

// ============================================================================
//  纯函数
// ============================================================================

/// 将值归一化到 [0.0, 1.0] 范围
///
/// 给定值在 [min, max] 范围内, 线性映射到 [0.0, 1.0]。
/// 如果 min == max, 返回 0.5 (避免除零)。
/// 值超出 [min, max] 范围时会被 clamp。
///
/// # 参数
///
/// - `value`: 待归一化的值
/// - `min`: 范围最小值
/// - `max`: 范围最大值
///
/// # 示例
///
/// ```
/// # use forge::sparkline::normalize_value;
/// assert!((normalize_value(0.5, 0.0, 1.0) - 0.5).abs() < 0.001);
/// assert!((normalize_value(0.0, 0.0, 1.0) - 0.0).abs() < 0.001);
/// assert!((normalize_value(1.0, 0.0, 1.0) - 1.0).abs() < 0.001);
/// // min == max → 0.5
/// assert!((normalize_value(0.5, 0.5, 0.5) - 0.5).abs() < 0.001);
/// ```
pub fn normalize_value(value: f64, min: f64, max: f64) -> f64 {
    if (max - min).abs() < f64::EPSILON {
        return 0.5;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// 将值映射到 sparkline 字符
///
/// 根据值在 [min, max] 范围内的位置, 返回对应的 Unicode 方块字符。
///
/// # 参数
///
/// - `value`: 待映射的值
/// - `min`: 范围最小值
/// - `max`: 范围最大值
///
/// # 返回
///
/// 8 级字符之一: `▁▂▃▄▅▆▇█`
///
/// # 示例
///
/// ```
/// # use forge::sparkline::map_value_to_char;
/// assert_eq!(map_value_to_char(0.0, 0.0, 1.0), '▁');
/// assert_eq!(map_value_to_char(1.0, 0.0, 1.0), '█');
/// // min == max → normalize 返回 0.5 → '▅'
/// assert_eq!(map_value_to_char(0.5, 0.5, 0.5), '▅');
/// ```
pub fn map_value_to_char(value: f64, min: f64, max: f64) -> char {
    let normalized = normalize_value(value, min, max);
    // 将 [0.0, 1.0] 均分为 8 级, 每级对应一个字符
    // normalized = 0.0 → index 0 ('▁')
    // normalized = 0.5 → index 4 ('▅')
    // normalized = 1.0 → index 8 → clamp 7 ('█')
    let index = ((normalized * 8.0).floor() as usize).min(7);
    SPARKLINE_CHARS[index]
}

/// 计算数值序列的统计信息
///
/// 包括最小值、最大值、平均值、极差和趋势方向。
/// 趋势方向通过比较前半段和后半段的平均值计算。
///
/// # 参数
///
/// - `values`: 数值序列
///
/// # 返回
///
/// 统计信息。空序列返回默认值 (min=0, max=0, avg=0, trend=Insufficient)。
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{compute_sparkline_stats, TrendDirection};
/// let stats = compute_sparkline_stats(&[0.8, 0.6, 0.4, 0.2]);
/// assert_eq!(stats.trend, TrendDirection::Down);
/// assert!((stats.avg - 0.5).abs() < 0.001);
/// ```
pub fn compute_sparkline_stats(values: &[f64]) -> SparklineStats {
    if values.is_empty() {
        return SparklineStats {
            min: 0.0,
            max: 0.0,
            avg: 0.0,
            range: 0.0,
            trend: TrendDirection::Insufficient,
            first: 0.0,
            last: 0.0,
            count: 0,
        };
    }

    if values.len() == 1 {
        return SparklineStats {
            min: values[0],
            max: values[0],
            avg: values[0],
            range: 0.0,
            trend: TrendDirection::Insufficient,
            first: values[0],
            last: values[0],
            count: 1,
        };
    }

    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let avg = values.iter().sum::<f64>() / values.len() as f64;
    let range = max - min;

    // 趋势: 比较前半段和后半段的平均值
    let mid = values.len() / 2;
    let first_half_avg = values[..mid].iter().sum::<f64>() / mid as f64;
    let second_half_avg = if values.len() - mid > 0 {
        values[mid..].iter().sum::<f64>() / (values.len() - mid) as f64
    } else {
        first_half_avg
    };

    let trend = if (second_half_avg - first_half_avg).abs() < TREND_THRESHOLD {
        TrendDirection::Flat
    } else if second_half_avg > first_half_avg {
        TrendDirection::Up
    } else {
        TrendDirection::Down
    };

    SparklineStats {
        min,
        max,
        avg,
        range,
        trend,
        first: values[0],
        last: values[values.len() - 1],
        count: values.len(),
    }
}

/// 将数值序列渲染为 sparkline 字符串
///
/// 使用 Unicode 方块字符将数值序列渲染为单行 ASCII 图表。
/// 如果序列长度超过 `config.max_width`, 只保留最近的 N 个值。
///
/// # 参数
///
/// - `values`: 数值序列
/// - `config`: 渲染配置
///
/// # 返回
///
/// Sparkline 字符串。空序列返回占位字符。
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{render_sparkline, SparklineConfig};
/// let values = vec![0.0, 0.25, 0.5, 0.75, 1.0];
/// let s = render_sparkline(&values, &SparklineConfig::default());
/// assert_eq!(s, "▁▃▅▇█");
///
/// // 空序列返回占位符
/// let empty = render_sparkline(&[], &SparklineConfig::default());
/// assert_eq!(empty, "·");
/// ```
pub fn render_sparkline(values: &[f64], config: &SparklineConfig) -> String {
    if values.is_empty() {
        return config.fill_char.to_string();
    }

    if values.len() == 1 {
        // 单个值映射到中间字符
        return map_value_to_char(values[0], values[0], values[0]).to_string();
    }

    // 截断到 max_width (保留最近的 N 个值), max_width 至少为 1
    let effective_width = config.max_width.max(1);
    let display_values: &[f64] = if values.len() > effective_width {
        &values[values.len() - effective_width..]
    } else {
        values
    };

    let min = display_values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = display_values
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);

    let mut result = String::with_capacity(display_values.len());
    for &v in display_values {
        result.push(map_value_to_char(v, min, max));
    }

    if config.show_min_max {
        let stats = compute_sparkline_stats(values);
        result.push_str(&format!(
            " (min {:.2}, max {:.2}, {})",
            stats.min,
            stats.max,
            stats.trend.arrow()
        ));
    }

    result
}

/// 使用指定的 min/max 范围渲染 sparkline
///
/// 与 [`render_sparkline`] 类似, 但使用显式指定的范围而非从数据中计算。
/// 适用于多个 sparkline 需要使用相同范围进行对比的场景。
///
/// # 参数
///
/// - `values`: 数值序列
/// - `min`: 范围最小值
/// - `max`: 范围最大值
/// - `config`: 渲染配置
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{render_sparkline_with_range, SparklineConfig};
/// let values = vec![0.3, 0.5, 0.7];
/// let s = render_sparkline_with_range(&values, 0.0, 1.0, &SparklineConfig::default());
/// assert_eq!(s, "▃▅▆");
/// ```
pub fn render_sparkline_with_range(
    values: &[f64],
    min: f64,
    max: f64,
    config: &SparklineConfig,
) -> String {
    if values.is_empty() {
        return config.fill_char.to_string();
    }

    // 截断到 max_width, max_width 至少为 1
    let effective_width = config.max_width.max(1);
    let display_values: &[f64] = if values.len() > effective_width {
        &values[values.len() - effective_width..]
    } else {
        values
    };

    let mut result = String::with_capacity(display_values.len());
    for &v in display_values {
        result.push(map_value_to_char(v, min, max));
    }

    if config.show_min_max {
        let stats = compute_sparkline_stats(values);
        result.push_str(&format!(
            " (min {:.2}, max {:.2}, {})",
            stats.min,
            stats.max,
            stats.trend.arrow()
        ));
    }

    result
}

/// 格式化带标签和统计的完整趋势行
///
/// 生成格式: `  {label}: {sparkline} ({first:.0%}→{last:.0%} {trend_arrow})`
///
/// 值以百分比形式显示 (0.0~1.0 → 0%~100%)。
/// 对于非百分比值 (如 TTL 秒数、带符号差值), 请使用 [`format_trend_sparkline_with`]。
///
/// # 参数
///
/// - `label`: 行标签 (如 "协同评分")
/// - `values`: 数值序列
/// - `config`: 渲染配置
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{format_trend_sparkline, SparklineConfig};
/// let values = vec![0.3, 0.5, 0.7, 0.9];
/// let line = format_trend_sparkline("评分", &values, &SparklineConfig::default());
/// assert!(line.contains("评分"));
/// assert!(line.contains("▁"));
/// assert!(line.contains("█"));
/// ```
pub fn format_trend_sparkline(label: &str, values: &[f64], config: &SparklineConfig) -> String {
    format_trend_sparkline_with(label, values, config, |v| format!("{:.0}%", v * 100.0))
}

/// 格式化带标签和统计的趋势行 (自定义值格式)
///
/// 与 [`format_trend_sparkline`] 类似, 但使用自定义的值格式化函数,
/// 适用于非百分比值 (如 TTL 秒数、带符号差值等)。
///
/// 生成格式: `  {label}: {sparkline} ({first_fmt}→{last_fmt} {trend_arrow})`
///
/// # 参数
///
/// - `label`: 行标签
/// - `values`: 数值序列
/// - `config`: 渲染配置
/// - `value_fmt`: 值格式化闭包, 接收 `f64` 返回格式化字符串
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{format_trend_sparkline_with, SparklineConfig};
/// let values = vec![1800.0, 2700.0, 3600.0];
/// let line = format_trend_sparkline_with("TTL", &values, &SparklineConfig::default(), |v| format!("{}s", v as u64));
/// assert!(line.contains("TTL"));
/// assert!(line.contains("1800s"));
/// assert!(line.contains("3600s"));
/// ```
pub fn format_trend_sparkline_with(
    label: &str,
    values: &[f64],
    config: &SparklineConfig,
    value_fmt: impl Fn(f64) -> String,
) -> String {
    if values.is_empty() {
        return format!("  {}: {}", label, config.fill_char);
    }

    let stats = compute_sparkline_stats(values);
    let sparkline = render_sparkline(values, config);

    if stats.count < 2 {
        return format!("  {}: {} ({})", label, sparkline, value_fmt(stats.first));
    }

    format!(
        "  {}: {} ({}→{} {})",
        label,
        sparkline,
        value_fmt(stats.first),
        value_fmt(stats.last),
        stats.trend.arrow()
    )
}

/// 格式化多组 sparkline
///
/// 在标题下渲染多组数值序列, 每组一行。
///
/// # 参数
///
/// - `title`: 面板标题
/// - `series`: 数值序列列表, 每项为 (标签, 数值切片)
/// - `config`: 渲染配置
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{format_multi_sparkline, SparklineConfig};
/// let series: Vec<(&str, &[f64])> = vec![
///     ("评分", &[0.3, 0.5, 0.7, 0.9]),
///     ("修复率", &[0.6, 0.65, 0.7, 0.75]),
/// ];
/// let panel = format_multi_sparkline("趋势", &series, &SparklineConfig::default());
/// assert!(panel.contains("趋势"));
/// assert!(panel.contains("评分"));
/// assert!(panel.contains("修复率"));
/// ```
pub fn format_multi_sparkline(
    title: &str,
    series: &[(&str, &[f64])],
    config: &SparklineConfig,
) -> String {
    let mut result = String::new();
    result.push_str(&format!("  ── {} ──\n", title));

    for (label, values) in series {
        result.push_str(&format_trend_sparkline(label, values, config));
        result.push('\n');
    }

    result
}

/// 转义 HTML 特殊字符
///
/// 将 `&`, `<`, `>`, `"`, `'` 转义为 HTML 实体。
///
/// # 参数
///
/// - `s`: 原始字符串
///
/// # 返回
///
/// 转义后的字符串
///
/// # 示例
///
/// ```
/// # use forge::sparkline::escape_html;
/// assert_eq!(escape_html("a < b & c > d"), "a &lt; b &amp; c &gt; d");
/// assert_eq!(escape_html("\"hello\""), "&quot;hello&quot;");
/// ```
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// 将百分比数值序列渲染为 sparkline (0.0~1.0 → 0%~100%)
///
/// 便捷函数, 将 0.0~1.0 范围的值渲染为 sparkline, 并附带百分比标注。
///
/// # 参数
///
/// - `values`: 百分比序列 (0.0~1.0)
/// - `config`: 渲染配置
///
/// # 示例
///
/// ```
/// # use forge::sparkline::{render_percentage_sparkline, SparklineConfig};
/// let values = vec![0.2, 0.4, 0.6, 0.8];
/// let s = render_percentage_sparkline(&values, &SparklineConfig::default());
/// assert!(!s.is_empty());
/// ```
pub fn render_percentage_sparkline(values: &[f64], config: &SparklineConfig) -> String {
    if values.is_empty() {
        return config.fill_char.to_string();
    }

    // 百分比 sparkline 使用固定范围 0.0~1.0
    render_sparkline_with_range(values, 0.0, 1.0, config)
}

// ============================================================================
//  测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ======================================================================
    //  常量测试
    // ======================================================================

    #[test]
    fn test_sparkline_chars_count() {
        assert_eq!(SPARKLINE_CHARS.len(), 8);
    }

    #[test]
    fn test_sparkline_chars_order() {
        // 从低到高
        for i in 1..SPARKLINE_CHARS.len() {
            assert_ne!(SPARKLINE_CHARS[i], SPARKLINE_CHARS[i - 1]);
        }
    }

    // ======================================================================
    //  SparklineConfig 测试
    // ======================================================================

    #[test]
    fn test_config_default() {
        let config = SparklineConfig::default();
        assert_eq!(config.max_width, DEFAULT_SPARKLINE_MAX_WIDTH);
        assert!(!config.show_min_max);
        assert_eq!(config.fill_char, DEFAULT_FILL_CHAR);
    }

    #[test]
    fn test_config_new() {
        let config = SparklineConfig::new(40);
        assert_eq!(config.max_width, 40);
        assert!(!config.show_min_max);
    }

    #[test]
    fn test_config_with_min_max() {
        let config = SparklineConfig::new(40).with_min_max(true);
        assert!(config.show_min_max);
    }

    #[test]
    fn test_config_with_fill_char() {
        let config = SparklineConfig::new(40).with_fill_char('-');
        assert_eq!(config.fill_char, '-');
    }

    #[test]
    fn test_config_serde() {
        let config = SparklineConfig::new(50).with_min_max(true);
        let json = serde_json::to_string(&config).unwrap();
        let loaded: SparklineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.max_width, 50);
        assert!(loaded.show_min_max);
    }

    // ======================================================================
    //  TrendDirection 测试
    // ======================================================================

    #[test]
    fn test_trend_arrow() {
        assert_eq!(TrendDirection::Up.arrow(), "↑");
        assert_eq!(TrendDirection::Down.arrow(), "↓");
        assert_eq!(TrendDirection::Flat.arrow(), "→");
        assert_eq!(TrendDirection::Insufficient.arrow(), "?");
    }

    #[test]
    fn test_trend_label() {
        assert_eq!(TrendDirection::Up.label(), "上升");
        assert_eq!(TrendDirection::Down.label(), "下降");
        assert_eq!(TrendDirection::Flat.label(), "平稳");
        assert_eq!(TrendDirection::Insufficient.label(), "数据不足");
    }

    #[test]
    fn test_trend_display() {
        assert_eq!(format!("{}", TrendDirection::Up), "↑");
        assert_eq!(format!("{}", TrendDirection::Down), "↓");
    }

    #[test]
    fn test_trend_serde() {
        let json = serde_json::to_string(&TrendDirection::Up).unwrap();
        let loaded: TrendDirection = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, TrendDirection::Up);
    }

    // ======================================================================
    //  SparklineStats 测试
    // ======================================================================

    #[test]
    fn test_stats_delta() {
        let stats = SparklineStats {
            min: 0.2,
            max: 0.8,
            avg: 0.5,
            range: 0.6,
            trend: TrendDirection::Up,
            first: 0.3,
            last: 0.7,
            count: 4,
        };
        assert!((stats.delta() - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_stats_has_data() {
        let stats = SparklineStats {
            min: 0.0,
            max: 0.0,
            avg: 0.0,
            range: 0.0,
            trend: TrendDirection::Insufficient,
            first: 0.0,
            last: 0.0,
            count: 1,
        };
        assert!(!stats.has_data());

        let stats2 = SparklineStats {
            min: 0.0,
            max: 0.0,
            avg: 0.0,
            range: 0.0,
            trend: TrendDirection::Flat,
            first: 0.0,
            last: 0.0,
            count: 2,
        };
        assert!(stats2.has_data());
    }

    #[test]
    fn test_stats_to_summary_empty() {
        let stats = SparklineStats {
            min: 0.0,
            max: 0.0,
            avg: 0.0,
            range: 0.0,
            trend: TrendDirection::Insufficient,
            first: 0.0,
            last: 0.0,
            count: 0,
        };
        assert_eq!(stats.to_summary(), "无数据");
    }

    #[test]
    fn test_stats_to_summary_with_data() {
        let stats = SparklineStats {
            min: 0.2,
            max: 0.8,
            avg: 0.5,
            range: 0.6,
            trend: TrendDirection::Up,
            first: 0.2,
            last: 0.8,
            count: 4,
        };
        let s = stats.to_summary();
        assert!(s.contains("0.20"));
        assert!(s.contains("0.80"));
        assert!(s.contains("上升"));
    }

    #[test]
    fn test_stats_serde() {
        let stats = SparklineStats {
            min: 0.1,
            max: 0.9,
            avg: 0.5,
            range: 0.8,
            trend: TrendDirection::Up,
            first: 0.1,
            last: 0.9,
            count: 5,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let loaded: SparklineStats = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.count, 5);
        assert_eq!(loaded.trend, TrendDirection::Up);
    }

    // ======================================================================
    //  normalize_value 测试
    // ======================================================================

    #[test]
    fn test_normalize_basic() {
        assert!((normalize_value(0.5, 0.0, 1.0) - 0.5).abs() < 0.001);
        assert!((normalize_value(0.0, 0.0, 1.0) - 0.0).abs() < 0.001);
        assert!((normalize_value(1.0, 0.0, 1.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_normalize_clamp() {
        // 值超出范围 → clamp
        assert!((normalize_value(-0.5, 0.0, 1.0) - 0.0).abs() < 0.001);
        assert!((normalize_value(1.5, 0.0, 1.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_normalize_equal_min_max() {
        // min == max → 0.5
        assert!((normalize_value(0.5, 0.5, 0.5) - 0.5).abs() < 0.001);
        assert!((normalize_value(1.0, 1.0, 1.0) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_normalize_negative_range() {
        assert!((normalize_value(0.0, -1.0, 1.0) - 0.5).abs() < 0.001);
        assert!((normalize_value(-1.0, -1.0, 1.0) - 0.0).abs() < 0.001);
        assert!((normalize_value(1.0, -1.0, 1.0) - 1.0).abs() < 0.001);
    }

    // ======================================================================
    //  map_value_to_char 测试
    // ======================================================================

    #[test]
    fn test_map_value_min() {
        assert_eq!(map_value_to_char(0.0, 0.0, 1.0), '▁');
    }

    #[test]
    fn test_map_value_max() {
        assert_eq!(map_value_to_char(1.0, 0.0, 1.0), '█');
    }

    #[test]
    fn test_map_value_mid() {
        // 0.5 → normalized 0.5 → floor(0.5*8)=4 → '▅'
        assert_eq!(map_value_to_char(0.5, 0.0, 1.0), '▅');
    }

    #[test]
    fn test_map_value_equal_min_max() {
        // min == max → normalize returns 0.5 → floor(0.5*8)=4 → '▅'
        assert_eq!(map_value_to_char(0.5, 0.5, 0.5), '▅');
    }

    #[test]
    fn test_map_value_clamp() {
        // 超出范围的值 → clamp
        assert_eq!(map_value_to_char(-1.0, 0.0, 1.0), '▁');
        assert_eq!(map_value_to_char(2.0, 0.0, 1.0), '█');
    }

    #[test]
    fn test_map_value_ascending() {
        // 0.0, 0.143, 0.286, 0.429, 0.571, 0.714, 0.857, 1.0
        // → ▁ ▂ ▃ ▄ ▅ ▆ ▇ █
        let values = [
            0.0,
            1.0 / 7.0,
            2.0 / 7.0,
            3.0 / 7.0,
            4.0 / 7.0,
            5.0 / 7.0,
            6.0 / 7.0,
            1.0,
        ];
        let chars: Vec<char> = values
            .iter()
            .map(|&v| map_value_to_char(v, 0.0, 1.0))
            .collect();
        assert_eq!(chars, SPARKLINE_CHARS.to_vec());
    }

    // ======================================================================
    //  compute_sparkline_stats 测试
    // ======================================================================

    #[test]
    fn test_stats_empty() {
        let stats = compute_sparkline_stats(&[]);
        assert_eq!(stats.count, 0);
        assert_eq!(stats.trend, TrendDirection::Insufficient);
    }

    #[test]
    fn test_stats_single() {
        let stats = compute_sparkline_stats(&[0.5]);
        assert_eq!(stats.count, 1);
        assert!((stats.min - 0.5).abs() < 0.001);
        assert!((stats.max - 0.5).abs() < 0.001);
        assert!((stats.avg - 0.5).abs() < 0.001);
        assert_eq!(stats.trend, TrendDirection::Insufficient);
    }

    #[test]
    fn test_stats_ascending() {
        let stats = compute_sparkline_stats(&[0.1, 0.3, 0.5, 0.7, 0.9]);
        assert_eq!(stats.count, 5);
        assert!((stats.min - 0.1).abs() < 0.001);
        assert!((stats.max - 0.9).abs() < 0.001);
        assert!((stats.avg - 0.5).abs() < 0.001);
        assert!((stats.range - 0.8).abs() < 0.001);
        assert_eq!(stats.trend, TrendDirection::Up);
        assert!((stats.first - 0.1).abs() < 0.001);
        assert!((stats.last - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_stats_descending() {
        let stats = compute_sparkline_stats(&[0.9, 0.7, 0.5, 0.3, 0.1]);
        assert_eq!(stats.trend, TrendDirection::Down);
    }

    #[test]
    fn test_stats_flat() {
        // 变化幅度 < 0.05 → Flat
        let stats = compute_sparkline_stats(&[0.50, 0.51, 0.52, 0.51, 0.50]);
        assert_eq!(stats.trend, TrendDirection::Flat);
    }

    #[test]
    fn test_stats_two_values_up() {
        let stats = compute_sparkline_stats(&[0.3, 0.8]);
        assert_eq!(stats.trend, TrendDirection::Up);
    }

    #[test]
    fn test_stats_two_values_down() {
        let stats = compute_sparkline_stats(&[0.8, 0.3]);
        assert_eq!(stats.trend, TrendDirection::Down);
    }

    #[test]
    fn test_stats_two_values_flat() {
        let stats = compute_sparkline_stats(&[0.5, 0.52]);
        // delta = 0.02 < 0.05 → Flat
        assert_eq!(stats.trend, TrendDirection::Flat);
    }

    #[test]
    fn test_stats_with_negatives() {
        let stats = compute_sparkline_stats(&[-0.5, 0.0, 0.5]);
        assert_eq!(stats.trend, TrendDirection::Up);
        assert!((stats.min - (-0.5)).abs() < 0.001);
        assert!((stats.max - 0.5).abs() < 0.001);
    }

    // ======================================================================
    //  render_sparkline 测试
    // ======================================================================

    #[test]
    fn test_render_empty() {
        let config = SparklineConfig::default();
        assert_eq!(render_sparkline(&[], &config), "·");
    }

    #[test]
    fn test_render_single() {
        let config = SparklineConfig::default();
        let s = render_sparkline(&[0.5], &config);
        assert_eq!(s, "▅"); // 单值 → normalize 0.5 → '▅'
    }

    #[test]
    fn test_render_ascending() {
        let config = SparklineConfig::default();
        let s = render_sparkline(&[0.0, 0.25, 0.5, 0.75, 1.0], &config);
        // 0.0→▁, 0.25→▃, 0.5→▅, 0.75→▇, 1.0→█
        assert_eq!(s, "▁▃▅▇█");
    }

    #[test]
    fn test_render_descending() {
        let config = SparklineConfig::default();
        let s = render_sparkline(&[1.0, 0.75, 0.5, 0.25, 0.0], &config);
        // 1.0→█, 0.75→▇, 0.5→▅, 0.25→▃, 0.0→▁
        assert_eq!(s, "█▇▅▃▁");
    }

    #[test]
    fn test_render_all_same() {
        let config = SparklineConfig::default();
        let s = render_sparkline(&[0.5, 0.5, 0.5, 0.5], &config);
        // min == max → normalize returns 0.5 → '▅'
        assert_eq!(s, "▅▅▅▅");
    }

    #[test]
    fn test_render_with_min_max() {
        let config = SparklineConfig::new(60).with_min_max(true);
        let s = render_sparkline(&[0.2, 0.4, 0.6, 0.8], &config);
        assert!(s.contains("min"));
        assert!(s.contains("max"));
        assert!(s.contains("↑"));
    }

    #[test]
    fn test_render_truncation() {
        // 超过 max_width → 只保留最近 N 个
        let config = SparklineConfig::new(5);
        let values: Vec<f64> = (0..20).map(|i| i as f64).collect();
        let s = render_sparkline(&values, &config);
        // 只保留最后 5 个值
        assert_eq!(s.chars().count(), 5);
    }

    #[test]
    fn test_render_custom_fill_char() {
        let config = SparklineConfig::new(60).with_fill_char('-');
        assert_eq!(render_sparkline(&[], &config), "-");
    }

    #[test]
    fn test_render_two_values() {
        let config = SparklineConfig::default();
        let s = render_sparkline(&[0.0, 1.0], &config);
        assert_eq!(s, "▁█");
    }

    // ======================================================================
    //  render_sparkline_with_range 测试
    // ======================================================================

    #[test]
    fn test_render_with_range_basic() {
        let config = SparklineConfig::default();
        let s = render_sparkline_with_range(&[0.3, 0.5, 0.7], 0.0, 1.0, &config);
        // 0.3→▃, 0.5→▅, 0.7→▆
        assert_eq!(s, "▃▅▆");
    }

    #[test]
    fn test_render_with_range_empty() {
        let config = SparklineConfig::default();
        assert_eq!(render_sparkline_with_range(&[], 0.0, 1.0, &config), "·");
    }

    #[test]
    fn test_render_with_range_negative() {
        let config = SparklineConfig::default();
        // 范围 -1.0 ~ 1.0
        let s = render_sparkline_with_range(&[-1.0, 0.0, 1.0], -1.0, 1.0, &config);
        // -1.0→▁, 0.0→▅, 1.0→█
        assert_eq!(s, "▁▅█");
    }

    #[test]
    fn test_render_with_range_clamped() {
        let config = SparklineConfig::default();
        // 值超出范围 → clamp
        let s = render_sparkline_with_range(&[-0.5, 0.5, 1.5], 0.0, 1.0, &config);
        // -0.5→clamp 0.0→▁, 0.5→▅, 1.5→clamp 1.0→█
        assert_eq!(s, "▁▅█");
    }

    #[test]
    fn test_render_with_range_min_max_annotation() {
        let config = SparklineConfig::new(60).with_min_max(true);
        let s = render_sparkline_with_range(&[0.2, 0.4, 0.6], 0.0, 1.0, &config);
        assert!(s.contains("min"));
        assert!(s.contains("max"));
    }

    // ======================================================================
    //  format_trend_sparkline 测试
    // ======================================================================

    #[test]
    fn test_format_trend_empty() {
        let config = SparklineConfig::default();
        let line = format_trend_sparkline("评分", &[], &config);
        assert!(line.contains("评分"));
        assert!(line.contains("·"));
    }

    #[test]
    fn test_format_trend_single() {
        let config = SparklineConfig::default();
        let line = format_trend_sparkline("评分", &[0.5], &config);
        assert!(line.contains("评分"));
        assert!(line.contains("50%"));
    }

    #[test]
    fn test_format_trend_ascending() {
        let config = SparklineConfig::default();
        let line = format_trend_sparkline("评分", &[0.3, 0.5, 0.7, 0.9], &config);
        assert!(line.contains("评分"));
        assert!(line.contains("▁"));
        assert!(line.contains("█"));
        assert!(line.contains("30%"));
        assert!(line.contains("90%"));
        assert!(line.contains("↑"));
    }

    #[test]
    fn test_format_trend_descending() {
        let config = SparklineConfig::default();
        let line = format_trend_sparkline("修复率", &[0.9, 0.7, 0.5, 0.3], &config);
        assert!(line.contains("↓"));
        assert!(line.contains("90%"));
        assert!(line.contains("30%"));
    }

    #[test]
    fn test_format_trend_flat() {
        let config = SparklineConfig::default();
        let line = format_trend_sparkline("稳定度", &[0.50, 0.51, 0.52, 0.51], &config);
        assert!(line.contains("→"));
    }

    // ======================================================================
    //  format_trend_sparkline_with 测试
    // ======================================================================

    #[test]
    fn test_format_trend_with_empty() {
        let config = SparklineConfig::default();
        let line = format_trend_sparkline_with("TTL", &[], &config, |v| format!("{}s", v as u64));
        assert!(line.contains("TTL"));
        assert!(line.contains("·"));
    }

    #[test]
    fn test_format_trend_with_single() {
        let config = SparklineConfig::default();
        let line =
            format_trend_sparkline_with("TTL", &[1800.0], &config, |v| format!("{}s", v as u64));
        assert!(line.contains("TTL"));
        assert!(line.contains("1800s"));
    }

    #[test]
    fn test_format_trend_with_seconds_ascending() {
        let config = SparklineConfig::default();
        let values = vec![1800.0, 2700.0, 3600.0];
        let line =
            format_trend_sparkline_with("TTL", &values, &config, |v| format!("{}s", v as u64));
        assert!(line.contains("TTL"));
        assert!(line.contains("1800s"));
        assert!(line.contains("3600s"));
        assert!(line.contains("↑"));
    }

    #[test]
    fn test_format_trend_with_seconds_descending() {
        let config = SparklineConfig::default();
        let values = vec![3600.0, 2700.0, 1800.0];
        let line =
            format_trend_sparkline_with("TTL", &values, &config, |v| format!("{}s", v as u64));
        assert!(line.contains("↓"));
        assert!(line.contains("3600s"));
        assert!(line.contains("1800s"));
    }

    #[test]
    fn test_format_trend_with_signed_percentage() {
        let config = SparklineConfig::default();
        let values = vec![0.1, -0.2, -0.5];
        let line = format_trend_sparkline_with("差值", &values, &config, |v| {
            format!("{:+.1}%", v * 100.0)
        });
        assert!(line.contains("差值"));
        assert!(line.contains("+10.0%"));
        assert!(line.contains("-50.0%"));
        assert!(line.contains("↓"));
    }

    #[test]
    fn test_format_trend_with_raw_values() {
        let config = SparklineConfig::default();
        let values = vec![0.67, 0.80, 0.50, 0.90];
        let line =
            format_trend_sparkline_with("相关差值", &values, &config, |v| format!("{:.2}", v));
        assert!(line.contains("相关差值"));
        assert!(line.contains("0.67"));
        assert!(line.contains("0.90"));
    }

    #[test]
    fn test_format_trend_with_percentage_compat() {
        // format_trend_sparkline 应与 format_trend_sparkline_with 百分比格式一致
        let config = SparklineConfig::default();
        let values = vec![0.3, 0.5, 0.7, 0.9];
        let line1 = format_trend_sparkline("评分", &values, &config);
        let line2 = format_trend_sparkline_with("评分", &values, &config, |v| {
            format!("{:.0}%", v * 100.0)
        });
        assert_eq!(line1, line2);
    }

    // ======================================================================
    //  format_multi_sparkline 测试
    // ======================================================================

    #[test]
    fn test_format_multi_empty() {
        let config = SparklineConfig::default();
        let series: Vec<(&str, &[f64])> = vec![];
        let panel = format_multi_sparkline("趋势", &series, &config);
        assert!(panel.contains("趋势"));
    }

    #[test]
    fn test_format_multi_single() {
        let config = SparklineConfig::default();
        let values = vec![0.3, 0.5, 0.7];
        let series: Vec<(&str, &[f64])> = vec![("评分", &values)];
        let panel = format_multi_sparkline("趋势", &series, &config);
        assert!(panel.contains("趋势"));
        assert!(panel.contains("评分"));
    }

    #[test]
    fn test_format_multi_multiple() {
        let config = SparklineConfig::default();
        let v1 = vec![0.3, 0.5, 0.7, 0.9];
        let v2 = vec![0.6, 0.65, 0.7, 0.75];
        let v3 = vec![0.8, 0.6, 0.4, 0.2];
        let series: Vec<(&str, &[f64])> = vec![("评分", &v1), ("修复率", &v2), ("错误率", &v3)];
        let panel = format_multi_sparkline("面板", &series, &config);
        assert!(panel.contains("面板"));
        assert!(panel.contains("评分"));
        assert!(panel.contains("修复率"));
        assert!(panel.contains("错误率"));
    }

    #[test]
    fn test_format_multi_with_empty_series() {
        let config = SparklineConfig::default();
        let v1 = vec![0.3, 0.5, 0.7];
        let series: Vec<(&str, &[f64])> = vec![("有数据", &v1), ("无数据", &[])];
        let panel = format_multi_sparkline("面板", &series, &config);
        assert!(panel.contains("有数据"));
        assert!(panel.contains("无数据"));
        assert!(panel.contains("·"));
    }

    // ======================================================================
    //  escape_html 测试
    // ======================================================================

    #[test]
    fn test_escape_html_ampersand() {
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }

    #[test]
    fn test_escape_html_less_than() {
        assert_eq!(escape_html("a < b"), "a &lt; b");
    }

    #[test]
    fn test_escape_html_greater_than() {
        assert_eq!(escape_html("a > b"), "a &gt; b");
    }

    #[test]
    fn test_escape_html_quotes() {
        assert_eq!(escape_html("\"hello\""), "&quot;hello&quot;");
        assert_eq!(escape_html("it's"), "it&#x27;s");
    }

    #[test]
    fn test_escape_html_all() {
        assert_eq!(
            escape_html("<div class=\"a\">&</div>"),
            "&lt;div class=&quot;a&quot;&gt;&amp;&lt;/div&gt;"
        );
    }

    #[test]
    fn test_escape_html_no_special() {
        assert_eq!(escape_html("hello world"), "hello world");
    }

    #[test]
    fn test_escape_html_empty() {
        assert_eq!(escape_html(""), "");
    }

    #[test]
    fn test_escape_html_ampersand_first() {
        // & must be escaped first to avoid double-escaping
        assert_eq!(escape_html("&&"), "&amp;&amp;");
        assert_eq!(escape_html("<&>"), "&lt;&amp;&gt;");
    }

    // ======================================================================
    //  render_percentage_sparkline 测试
    // ======================================================================

    #[test]
    fn test_percentage_sparkline_basic() {
        let config = SparklineConfig::default();
        let s = render_percentage_sparkline(&[0.0, 0.5, 1.0], &config);
        // 0.0→▁, 0.5→▅, 1.0→█
        assert_eq!(s, "▁▅█");
    }

    #[test]
    fn test_percentage_sparkline_empty() {
        let config = SparklineConfig::default();
        assert_eq!(render_percentage_sparkline(&[], &config), "·");
    }

    #[test]
    fn test_percentage_sparkline_fixed_range() {
        let config = SparklineConfig::default();
        // 即使值都在 0.7~0.9 范围内, 也使用 0.0~1.0 范围
        let s = render_percentage_sparkline(&[0.7, 0.8, 0.9], &config);
        // 0.7 → ▄ (normalized 0.7 → index 5 → '▆')
        // 0.8 → ▆ (normalized 0.8 → index 6 → '▇')
        // 0.9 → ▇ (normalized 0.9 → index 6 → '▇')
        // Actually: 0.7*7=4.9→5→'▆', 0.8*7=5.6→6→'▇', 0.9*7=6.3→6→'▇'
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars.len(), 3);
        // 确认使用固定范围 (所有字符都在中高区域)
        assert!(chars.iter().all(|&c| c == '▆' || c == '▇' || c == '█'));
    }

    // ======================================================================
    //  边界条件测试
    // ======================================================================

    #[test]
    fn test_render_zero_width_config() {
        // max_width = 0 → effective_width = 1, 保留最后 1 个值
        let config = SparklineConfig::new(0);
        let s = render_sparkline(&[0.1, 0.2, 0.3], &config);
        assert_eq!(s.chars().count(), 1);
    }

    #[test]
    fn test_render_large_sequence() {
        let config = SparklineConfig::new(30);
        let values: Vec<f64> = (0..100).map(|i| (i as f64) / 100.0).collect();
        let s = render_sparkline(&values, &config);
        assert_eq!(s.chars().count(), 30); // 截断到 max_width
    }

    #[test]
    fn test_render_with_infinity() {
        let config = SparklineConfig::default();
        let s = render_sparkline(&[0.0, f64::INFINITY, 1.0], &config);
        // f64::INFINITY 会被 fold 计算为 max
        // normalize_value(0.0, 0.0, INF) → 0.0 → '▁'
        // normalize_value(INF, 0.0, INF) → 1.0 → '█'
        // normalize_value(1.0, 0.0, INF) → 1.0/INF ≈ 0.0 → '▁'
        assert!(!s.is_empty());
    }

    #[test]
    fn test_render_with_nan() {
        let config = SparklineConfig::default();
        let s = render_sparkline(&[0.0, f64::NAN, 1.0], &config);
        // NaN 在 fold 中会被忽略 (因为 NaN 比较都返回 false)
        // min = 0.0, max = 1.0
        // normalize_value(NaN, 0.0, 1.0) → NaN.clamp(0.0, 1.0) → NaN
        // NaN * 7.0 → NaN, as usize → 0 → '▁'
        assert!(!s.is_empty());
    }

    // ======================================================================
    //  集成场景测试
    // ======================================================================

    #[test]
    fn test_integration_synergy_score_trend() {
        // 模拟协同评分历史: 5 个 session 的评分
        let scores = vec![0.45, 0.52, 0.61, 0.73, 0.85];
        let config = SparklineConfig::new(20).with_min_max(true);
        let line = format_trend_sparkline("协同评分", &scores, &config);
        assert!(line.contains("协同评分"));
        assert!(line.contains("45%"));
        assert!(line.contains("85%"));
        assert!(line.contains("↑"));
    }

    #[test]
    fn test_integration_fix_rate_trend() {
        let rates = vec![0.60, 0.55, 0.50, 0.45, 0.40];
        let config = SparklineConfig::new(20);
        let line = format_trend_sparkline("修复率", &rates, &config);
        assert!(line.contains("↓"));
    }

    #[test]
    fn test_integration_multi_panel() {
        let scores = vec![0.45, 0.55, 0.65, 0.75];
        let fix_rates = vec![0.60, 0.65, 0.70, 0.75];
        let ttl_changes = vec![1800.0, 2700.0, 3600.0, 3600.0];

        let v1 = scores.clone();
        let v2 = fix_rates.clone();
        let v3 = ttl_changes.clone();
        let series: Vec<(&str, &[f64])> = vec![("协同评分", &v1), ("修复率", &v2), ("TTL", &v3)];

        let config = SparklineConfig::new(20);
        let panel = format_multi_sparkline("跨 Session 趋势", &series, &config);
        assert!(panel.contains("跨 Session 趋势"));
        assert!(panel.contains("协同评分"));
        assert!(panel.contains("修复率"));
        assert!(panel.contains("TTL"));
    }

    #[test]
    fn test_integration_percentage_panel() {
        let v1 = vec![0.3, 0.5, 0.7, 0.9];
        let v2 = vec![0.8, 0.6, 0.4, 0.2];

        let series: Vec<(&str, &[f64])> = vec![("上升指标", &v1), ("下降指标", &v2)];

        let config = SparklineConfig::default();
        let panel = format_multi_sparkline("百分比趋势", &series, &config);
        assert!(panel.contains("↑"));
        assert!(panel.contains("↓"));
    }
}
