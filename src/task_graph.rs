//! TaskGraph — 任务依赖分析与并行执行调度
//!
//! **方向 C: 并行任务执行**
//!
//! 在同一阶段内,分析任务之间的依赖关系,构建有向无环图 (DAG),
//! 识别可以并行执行的任务组,加速多任务阶段的开发。
//!
//! ## 核心功能
//!
//! - `build_from_tasks`: 从任务列表构建 TaskGraph
//! - `topological_sort`: 拓扑排序 (返回执行顺序)
//! - `parallel_groups`: 并行分组 (返回可同时执行的任务组)
//! - `has_cycle`: 环检测 (依赖循环检测)
//! - `dependencies_of` / `dependents_of`: 查询依赖关系
//!
//! ## 示例
//!
//! ```text
//! 任务 0-0 (无依赖)     ──┐
//! 任务 0-1 (无依赖)     ──┤── 任务 0-3 (依赖 0-0, 0-1)
//! 任务 0-2 (依赖 0-0)   ──┘
//!
//! parallel_groups: [[0-0, 0-1], [0-2], [0-3]]
//! ```

use crate::memory::{Phase, Task};
use std::collections::{HashMap, HashSet};

/// 任务依赖图错误
#[derive(Debug, Clone, PartialEq)]
pub enum TaskGraphError {
    /// 依赖的任务不存在 (task_id 引用了不存在的任务)
    MissingDependency(String),
    /// 检测到依赖环
    CycleDetected,
}

impl std::fmt::Display for TaskGraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskGraphError::MissingDependency(id) => {
                write!(f, "依赖的任务不存在: {}", id)
            }
            TaskGraphError::CycleDetected => write!(f, "检测到任务依赖环"),
        }
    }
}

impl std::error::Error for TaskGraphError {}

/// 任务依赖图 (DAG)
///
/// 从一组任务构建有向无环图,支持:
/// - 拓扑排序: 确定线性执行顺序
/// - 并行分组: 识别可同时执行的任务组
/// - 环检测: 发现循环依赖
///
/// 节点索引 = 任务在 tasks 数组中的索引。
/// 边 (A → B) 表示 B 依赖 A (A 必须在 B 之前完成)。
#[derive(Debug, Clone)]
pub struct TaskGraph {
    /// 任务数量
    num_tasks: usize,
    /// 任务 ID → 任务索引 的映射
    task_id_to_idx: HashMap<String, usize>,
    /// 任务索引 → 任务 ID 的映射
    task_idx_to_id: Vec<String>,
    /// 邻接表: adj[i] = 依赖于任务 i 的任务索引列表 (i → j 表示 j 依赖 i)
    adj: Vec<Vec<usize>>,
    /// 反向邻接表: rev_adj[i] = 任务 i 依赖的任务索引列表
    rev_adj: Vec<Vec<usize>>,
    /// 每个任务的入度 (依赖的任务数量)
    in_degree: Vec<usize>,
}

impl TaskGraph {
    /// 从 Phase 构建任务依赖图
    pub fn build_from_phase(phase: &Phase) -> Result<Self, TaskGraphError> {
        Self::build_from_tasks(&phase.tasks)
    }

    /// 从任务列表构建任务依赖图
    ///
    /// 解析每个任务的 `depends_on` 字段,建立 DAG。
    /// 如果依赖的任务 ID 不存在,返回 `MissingDependency` 错误。
    pub fn build_from_tasks(tasks: &[Task]) -> Result<Self, TaskGraphError> {
        let num_tasks = tasks.len();
        let mut task_id_to_idx: HashMap<String, usize> = HashMap::new();
        let mut task_idx_to_id: Vec<String> = Vec::with_capacity(num_tasks);

        // 建立 ID → 索引映射
        for (idx, task) in tasks.iter().enumerate() {
            task_id_to_idx.insert(task.id.clone(), idx);
            task_idx_to_id.push(task.id.clone());
        }

        let mut adj = vec![Vec::new(); num_tasks];
        let mut rev_adj = vec![Vec::new(); num_tasks];
        let mut in_degree = vec![0usize; num_tasks];

        // 解析依赖关系
        for (idx, task) in tasks.iter().enumerate() {
            for dep_id in &task.depends_on {
                let dep_idx = task_id_to_idx
                    .get(dep_id)
                    .ok_or_else(|| TaskGraphError::MissingDependency(dep_id.clone()))?;
                // dep_idx → idx (idx 依赖 dep_idx)
                adj[*dep_idx].push(idx);
                rev_adj[idx].push(*dep_idx);
                in_degree[idx] += 1;
            }
        }

        Ok(Self {
            num_tasks,
            task_id_to_idx,
            task_idx_to_id,
            adj,
            rev_adj,
            in_degree,
        })
    }

    /// 获取任务数量
    pub fn num_tasks(&self) -> usize {
        self.num_tasks
    }

    /// 根据任务 ID 获取索引
    pub fn task_index(&self, task_id: &str) -> Option<usize> {
        self.task_id_to_idx.get(task_id).copied()
    }

    /// 根据索引获取任务 ID
    pub fn task_id(&self, idx: usize) -> Option<&str> {
        self.task_idx_to_id.get(idx).map(|s| s.as_str())
    }

    /// 获取任务 i 依赖的所有任务索引
    pub fn dependencies_of(&self, idx: usize) -> &[usize] {
        &self.rev_adj[idx]
    }

    /// 获取依赖任务 i 的所有任务索引
    pub fn dependents_of(&self, idx: usize) -> &[usize] {
        &self.adj[idx]
    }

    /// 检测是否有环 (循环依赖)
    ///
    /// 使用 Kahn 算法: 如果拓扑排序无法覆盖所有节点,说明存在环。
    pub fn has_cycle(&self) -> bool {
        let mut in_degree = self.in_degree.clone();
        let mut queue: Vec<usize> = (0..self.num_tasks).filter(|&i| in_degree[i] == 0).collect();
        let mut visited = 0;

        while let Some(node) = queue.pop() {
            visited += 1;
            for &neighbor in &self.adj[node] {
                in_degree[neighbor] -= 1;
                if in_degree[neighbor] == 0 {
                    queue.push(neighbor);
                }
            }
        }

        visited < self.num_tasks
    }

    /// 拓扑排序 — 返回线性执行顺序
    ///
    /// 如果存在环,返回 `CycleDetected` 错误。
    /// 同层级的任务按索引顺序排列 (稳定排序)。
    pub fn topological_sort(&self) -> Result<Vec<usize>, TaskGraphError> {
        let mut in_degree = self.in_degree.clone();
        let mut result = Vec::with_capacity(self.num_tasks);

        // 使用 Vec 作为优先队列 (按索引排序,保证稳定)
        let mut ready: Vec<usize> = (0..self.num_tasks).filter(|&i| in_degree[i] == 0).collect();
        ready.sort();

        while !ready.is_empty() {
            let node = ready.remove(0);
            result.push(node);

            for &neighbor in &self.adj[node] {
                in_degree[neighbor] -= 1;
                if in_degree[neighbor] == 0 {
                    // 插入并保持有序
                    let pos = ready.binary_search(&neighbor).unwrap_or_else(|e| e);
                    ready.insert(pos, neighbor);
                }
            }
        }

        if result.len() < self.num_tasks {
            return Err(TaskGraphError::CycleDetected);
        }

        Ok(result)
    }

    /// 并行分组 — 返回可同时执行的任务组列表
    ///
    /// 每一组中的任务互不依赖,可以并行执行。
    /// 组与组之间有依赖关系,必须按顺序执行。
    ///
    /// 算法: Kahn 算法分层 — 每次取出所有入度为 0 的节点作为一组。
    ///
    /// 如果存在环,返回 `CycleDetected` 错误。
    pub fn parallel_groups(&self) -> Result<Vec<Vec<usize>>, TaskGraphError> {
        let mut in_degree = self.in_degree.clone();
        let mut groups = Vec::new();
        let mut visited = 0;

        loop {
            // 收集当前所有入度为 0 的节点
            let group: Vec<usize> = (0..self.num_tasks).filter(|&i| in_degree[i] == 0).collect();

            if group.is_empty() {
                break;
            }

            visited += group.len();

            // 更新入度
            for &node in &group {
                in_degree[node] = usize::MAX; // 标记为已处理
                for &neighbor in &self.adj[node] {
                    if in_degree[neighbor] != usize::MAX {
                        in_degree[neighbor] -= 1;
                    }
                }
            }

            groups.push(group);
        }

        if visited < self.num_tasks {
            return Err(TaskGraphError::CycleDetected);
        }

        Ok(groups)
    }

    /// 获取单个任务的最大并行度 (可以同时执行的任务数)
    ///
    /// 返回最大的一组的大小。
    pub fn max_parallelism(&self) -> Result<usize, TaskGraphError> {
        let groups = self.parallel_groups()?;
        Ok(groups.iter().map(|g| g.len()).max().unwrap_or(0))
    }

    /// 获取一个任务的所有直接和间接依赖 (传递闭包)
    ///
    /// 返回该任务依赖的所有任务索引 (包括间接依赖)。
    pub fn all_dependencies(&self, idx: usize) -> HashSet<usize> {
        let mut visited = HashSet::new();
        let mut stack = vec![idx];

        while let Some(node) = stack.pop() {
            for &dep in &self.rev_adj[node] {
                if visited.insert(dep) {
                    stack.push(dep);
                }
            }
        }

        visited
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Phase, PhaseStatus, Task, TaskStatus};

    /// 创建测试任务
    fn make_task(id: &str, phase_id: usize, depends_on: Vec<String>) -> Task {
        Task {
            id: id.to_string(),
            phase_id,
            name: format!("Task {}", id),
            prompt: format!("Do task {}", id),
            status: TaskStatus::Pending,
            result: None,
            attempts: 0,
            files_written: vec![],
            test_result: None,
            last_good_snapshot: None,
            clarifications: vec![],
            depends_on,
        }
    }

    /// 创建测试阶段
    fn make_phase(tasks: Vec<Task>) -> Phase {
        Phase {
            id: 0,
            name: "Test Phase".to_string(),
            description: "Test".to_string(),
            status: PhaseStatus::Pending,
            tasks,
        }
    }

    // ===== 基础构建测试 =====

    #[test]
    fn test_build_empty_tasks() {
        let graph = TaskGraph::build_from_tasks(&[]).unwrap();
        assert_eq!(graph.num_tasks(), 0);
    }

    #[test]
    fn test_build_single_task_no_deps() {
        let tasks = vec![make_task("0-0", 0, vec![])];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert_eq!(graph.num_tasks(), 1);
        assert!(!graph.has_cycle());
    }

    #[test]
    fn test_build_multiple_tasks_no_deps() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec![]),
            make_task("0-2", 0, vec![]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert_eq!(graph.num_tasks(), 3);
        assert!(!graph.has_cycle());
    }

    #[test]
    fn test_build_with_valid_dependencies() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-0".to_string(), "0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert_eq!(graph.num_tasks(), 3);
        assert!(!graph.has_cycle());
    }

    #[test]
    fn test_build_missing_dependency() {
        let tasks = vec![make_task("0-0", 0, vec!["0-99".to_string()])];
        let result = TaskGraph::build_from_tasks(&tasks);
        assert!(matches!(result, Err(TaskGraphError::MissingDependency(ref id)) if id == "0-99"));
    }

    // ===== 环检测测试 =====

    #[test]
    fn test_no_cycle_simple() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert!(!graph.has_cycle());
    }

    #[test]
    fn test_cycle_two_nodes() {
        let tasks = vec![
            make_task("0-0", 0, vec!["0-1".to_string()]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert!(graph.has_cycle());
    }

    #[test]
    fn test_cycle_three_nodes() {
        let tasks = vec![
            make_task("0-0", 0, vec!["0-2".to_string()]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert!(graph.has_cycle());
    }

    #[test]
    fn test_no_cycle_diamond() {
        // 0-0 → 0-1 → 0-3
        // 0-0 → 0-2 → 0-3  (菱形依赖,不是环)
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-0".to_string()]),
            make_task("0-3", 0, vec!["0-1".to_string(), "0-2".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert!(!graph.has_cycle());
    }

    #[test]
    fn test_self_cycle() {
        let tasks = vec![make_task("0-0", 0, vec!["0-0".to_string()])];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert!(graph.has_cycle());
    }

    // ===== 拓扑排序测试 =====

    #[test]
    fn test_topological_sort_empty() {
        let graph = TaskGraph::build_from_tasks(&[]).unwrap();
        let result = graph.topological_sort().unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_topological_sort_single() {
        let tasks = vec![make_task("0-0", 0, vec![])];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let result = graph.topological_sort().unwrap();
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn test_topological_sort_linear() {
        // 0-0 → 0-1 → 0-2
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let result = graph.topological_sort().unwrap();
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn test_topological_sort_with_cycle() {
        let tasks = vec![
            make_task("0-0", 0, vec!["0-1".to_string()]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let result = graph.topological_sort();
        assert_eq!(result, Err(TaskGraphError::CycleDetected));
    }

    #[test]
    fn test_topological_sort_diamond() {
        // 0-0 → 0-1 → 0-3
        // 0-0 → 0-2 → 0-3
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-0".to_string()]),
            make_task("0-3", 0, vec!["0-1".to_string(), "0-2".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let result = graph.topological_sort().unwrap();
        // 0-0 必须在前, 0-3 必须在后, 0-1 和 0-2 在中间 (按索引顺序)
        assert_eq!(result, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_topological_sort_independent_tasks() {
        // 三个独立任务, 应按索引顺序排列
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec![]),
            make_task("0-2", 0, vec![]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let result = graph.topological_sort().unwrap();
        assert_eq!(result, vec![0, 1, 2]);
    }

    // ===== 并行分组测试 =====

    #[test]
    fn test_parallel_groups_empty() {
        let graph = TaskGraph::build_from_tasks(&[]).unwrap();
        let groups = graph.parallel_groups().unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn test_parallel_groups_single() {
        let tasks = vec![make_task("0-0", 0, vec![])];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let groups = graph.parallel_groups().unwrap();
        assert_eq!(groups, vec![vec![0]]);
    }

    #[test]
    fn test_parallel_groups_all_independent() {
        // 三个独立任务, 应全部在一组
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec![]),
            make_task("0-2", 0, vec![]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let groups = graph.parallel_groups().unwrap();
        assert_eq!(groups, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn test_parallel_groups_linear_chain() {
        // 0-0 → 0-1 → 0-2 (每组只有一个任务)
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let groups = graph.parallel_groups().unwrap();
        assert_eq!(groups, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn test_parallel_groups_diamond() {
        // 0-0 → 0-1 → 0-3
        // 0-0 → 0-2 → 0-3
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-0".to_string()]),
            make_task("0-3", 0, vec!["0-1".to_string(), "0-2".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let groups = graph.parallel_groups().unwrap();
        // 第一组: [0-0], 第二组: [0-1, 0-2], 第三组: [0-3]
        assert_eq!(groups, vec![vec![0], vec![1, 2], vec![3]]);
    }

    #[test]
    fn test_parallel_groups_with_cycle() {
        let tasks = vec![
            make_task("0-0", 0, vec!["0-1".to_string()]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let result = graph.parallel_groups();
        assert_eq!(result, Err(TaskGraphError::CycleDetected));
    }

    #[test]
    fn test_parallel_groups_complex() {
        // 0-0 (无依赖) ──┐
        // 0-1 (无依赖) ──┤── 0-3 (依赖 0-0, 0-1)
        // 0-2 (依赖 0-0)─┘
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec![]),
            make_task("0-2", 0, vec!["0-0".to_string()]),
            make_task("0-3", 0, vec!["0-0".to_string(), "0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let groups = graph.parallel_groups().unwrap();
        // 第一组: [0-0, 0-1] (无依赖)
        // 第二组: [0-2, 0-3] (都只依赖第一组的任务)
        assert_eq!(groups, vec![vec![0, 1], vec![2, 3]]);
    }

    // ===== 最大并行度测试 =====

    #[test]
    fn test_max_parallelism_empty() {
        let graph = TaskGraph::build_from_tasks(&[]).unwrap();
        assert_eq!(graph.max_parallelism().unwrap(), 0);
    }

    #[test]
    fn test_max_parallelism_single() {
        let tasks = vec![make_task("0-0", 0, vec![])];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert_eq!(graph.max_parallelism().unwrap(), 1);
    }

    #[test]
    fn test_max_parallelism_all_independent() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec![]),
            make_task("0-2", 0, vec![]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert_eq!(graph.max_parallelism().unwrap(), 3);
    }

    #[test]
    fn test_max_parallelism_linear() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        assert_eq!(graph.max_parallelism().unwrap(), 1);
    }

    // ===== 依赖查询测试 =====

    #[test]
    fn test_dependencies_of() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-0".to_string(), "0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        assert!(graph.dependencies_of(0).is_empty());
        assert_eq!(graph.dependencies_of(1), &[0]);
        assert!(graph.dependencies_of(2).contains(&0));
        assert!(graph.dependencies_of(2).contains(&1));
    }

    #[test]
    fn test_dependents_of() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-0".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        // 0-0 被 0-1 和 0-2 依赖
        assert!(graph.dependents_of(0).contains(&1));
        assert!(graph.dependents_of(0).contains(&2));
        // 0-1 没有被任何任务依赖
        assert!(graph.dependents_of(1).is_empty());
    }

    #[test]
    fn test_task_index_lookup() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec![]),
            make_task("0-2", 0, vec![]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        assert_eq!(graph.task_index("0-0"), Some(0));
        assert_eq!(graph.task_index("0-1"), Some(1));
        assert_eq!(graph.task_index("0-2"), Some(2));
        assert_eq!(graph.task_index("0-3"), None);
    }

    #[test]
    fn test_task_id_lookup() {
        let tasks = vec![make_task("0-0", 0, vec![]), make_task("0-1", 0, vec![])];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        assert_eq!(graph.task_id(0), Some("0-0"));
        assert_eq!(graph.task_id(1), Some("0-1"));
        assert_eq!(graph.task_id(2), None);
    }

    // ===== 传递闭包测试 =====

    #[test]
    fn test_all_dependencies_direct_only() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        let deps = graph.all_dependencies(1);
        assert_eq!(deps, HashSet::from([0]));
    }

    #[test]
    fn test_all_dependencies_transitive() {
        // 0-0 → 0-1 → 0-2 → 0-3
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-1".to_string()]),
            make_task("0-3", 0, vec!["0-2".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        // 0-3 的所有依赖: 0-0, 0-1, 0-2
        let deps = graph.all_dependencies(3);
        assert_eq!(deps, HashSet::from([0, 1, 2]));
    }

    #[test]
    fn test_all_dependencies_no_deps() {
        let tasks = vec![make_task("0-0", 0, vec![])];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        let deps = graph.all_dependencies(0);
        assert!(deps.is_empty());
    }

    // ===== build_from_phase 测试 =====

    #[test]
    fn test_build_from_phase() {
        let phase = make_phase(vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
        ]);
        let graph = TaskGraph::build_from_phase(&phase).unwrap();
        assert_eq!(graph.num_tasks(), 2);
        assert!(!graph.has_cycle());
    }

    #[test]
    fn test_build_from_phase_empty() {
        let phase = make_phase(vec![]);
        let graph = TaskGraph::build_from_phase(&phase).unwrap();
        assert_eq!(graph.num_tasks(), 0);
    }

    // ===== TaskGraphError Display 测试 =====

    #[test]
    fn test_error_display_missing_dependency() {
        let err = TaskGraphError::MissingDependency("0-99".to_string());
        assert!(format!("{}", err).contains("0-99"));
    }

    #[test]
    fn test_error_display_cycle() {
        let err = TaskGraphError::CycleDetected;
        assert!(format!("{}", err).contains("环"));
    }

    // ===== 综合场景测试 =====

    #[test]
    fn test_complex_scenario_mixed_deps() {
        // 复杂场景:
        // 0-0 (无依赖)     ──┐
        // 0-1 (无依赖)     ──┤── 0-4 (依赖 0-1, 0-2)
        // 0-2 (依赖 0-0)   ──┤── 0-5 (依赖 0-3, 0-4)
        // 0-3 (依赖 0-1)   ──┘
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec![]),
            make_task("0-2", 0, vec!["0-0".to_string()]),
            make_task("0-3", 0, vec!["0-1".to_string()]),
            make_task("0-4", 0, vec!["0-1".to_string(), "0-2".to_string()]),
            make_task("0-5", 0, vec!["0-3".to_string(), "0-4".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();

        assert!(!graph.has_cycle());

        let topo = graph.topological_sort().unwrap();
        assert_eq!(topo.len(), 6);

        // 验证拓扑顺序: 0-0 和 0-1 在 0-2 和 0-3 之前, 0-2 和 0-3 在 0-4 之前, 0-4 在 0-5 之前
        let pos: HashMap<usize, usize> = topo
            .iter()
            .enumerate()
            .map(|(pos, &idx)| (idx, pos))
            .collect();
        assert!(pos[&0] < pos[&2]); // 0-0 before 0-2
        assert!(pos[&1] < pos[&3]); // 0-1 before 0-3
        assert!(pos[&1] < pos[&4]); // 0-1 before 0-4
        assert!(pos[&2] < pos[&4]); // 0-2 before 0-4
        assert!(pos[&3] < pos[&5]); // 0-3 before 0-5
        assert!(pos[&4] < pos[&5]); // 0-4 before 0-5

        let groups = graph.parallel_groups().unwrap();
        assert!(groups.len() >= 2); // 至少有 2 组
    }

    #[test]
    fn test_parallel_groups_all_tasks_covered() {
        let tasks = vec![
            make_task("0-0", 0, vec![]),
            make_task("0-1", 0, vec!["0-0".to_string()]),
            make_task("0-2", 0, vec!["0-0".to_string()]),
            make_task("0-3", 0, vec!["0-1".to_string()]),
        ];
        let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
        let groups = graph.parallel_groups().unwrap();

        // 验证所有任务都被覆盖
        let mut all_indices: Vec<usize> = groups.iter().flatten().copied().collect();
        all_indices.sort();
        assert_eq!(all_indices, vec![0, 1, 2, 3]);

        // 验证每个任务只出现一次
        let mut seen = HashSet::new();
        for &idx in groups.iter().flatten() {
            assert!(seen.insert(idx), "任务 {} 出现多次", idx);
        }
    }
}
