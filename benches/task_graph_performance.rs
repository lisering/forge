#![allow(clippy::useless_vec)]

//! TaskGraph 模块性能基准测试
//!
//! 测试目标:
//! 1. build_from_tasks - DAG 构建性能 (无依赖/线性/菱形/随机)
//! 2. topological_sort - 拓扑排序性能
//! 3. parallel_groups - 并行分组性能
//! 4. has_cycle - 环检测性能
//! 5. edge_cases - 边界条件性能 (空/单任务/自循环/深度链)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::memory::{Task, TaskStatus};
use forge::task_graph::TaskGraph;

/// 创建测试任务
fn make_task(id: &str, phase_id: usize, depends_on: Vec<String>) -> Task {
    Task {
        id: id.to_string(),
        phase_id,
        name: format!("Task {id}"),
        prompt: format!("Do task {id}"),
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

/// 构建无依赖任务列表
fn build_independent_tasks(n: usize) -> Vec<Task> {
    (0..n)
        .map(|i| make_task(&format!("0-{i}"), 0, vec![]))
        .collect()
}

/// 构建线性依赖链: 0-0 → 0-1 → 0-2 → ... → 0-(n-1)
fn build_linear_chain(n: usize) -> Vec<Task> {
    (0..n)
        .map(|i| {
            let deps = if i == 0 {
                vec![]
            } else {
                vec![format!("0-{}", i - 1)]
            };
            make_task(&format!("0-{i}"), 0, deps)
        })
        .collect()
}

/// 构建菱形依赖: 0-0 → (0-1, 0-2, ...) → 0-(n-1)
fn build_diamond(n: usize) -> Vec<Task> {
    let mut tasks = Vec::with_capacity(n);
    if n == 0 {
        return tasks;
    }
    // 0-0: 无依赖
    tasks.push(make_task("0-0", 0, vec![]));
    // 中间任务: 依赖 0-0
    for i in 1..n - 1 {
        tasks.push(make_task(&format!("0-{i}"), 0, vec!["0-0".to_string()]));
    }
    // 最后任务: 依赖所有中间任务
    if n > 1 {
        let last = n - 1;
        let deps: Vec<String> = (1..last).map(|i| format!("0-{i}")).collect();
        tasks.push(make_task(&format!("0-{last}"), 0, deps));
    }
    tasks
}

/// 构建带循环依赖的任务列表 (0-0 → 0-1 → 0-0)
fn build_cyclic_tasks(n: usize) -> Vec<Task> {
    let mut tasks: Vec<Task> = (0..n)
        .map(|i| make_task(&format!("0-{i}"), 0, vec![]))
        .collect();
    if n >= 2 {
        // 0-0 依赖 0-1, 0-1 依赖 0-0
        tasks[0].depends_on = vec![format!("0-{}", 1)];
        tasks[1].depends_on = vec![format!("0-{}", 0)];
    }
    tasks
}

/// 基准测试: build_from_tasks
fn bench_build_from_tasks(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_from_tasks");

    let sizes: Vec<usize> = vec![10, 100, 500, 1000];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));

        let independent = build_independent_tasks(size);
        let linear = build_linear_chain(size);
        let diamond = build_diamond(size);

        group.bench_with_input(
            BenchmarkId::new("independent", size),
            &independent,
            |b, tasks| b.iter(|| black_box(TaskGraph::build_from_tasks(black_box(tasks)).unwrap())),
        );

        group.bench_with_input(BenchmarkId::new("linear", size), &linear, |b, tasks| {
            b.iter(|| black_box(TaskGraph::build_from_tasks(black_box(tasks)).unwrap()))
        });

        group.bench_with_input(BenchmarkId::new("diamond", size), &diamond, |b, tasks| {
            b.iter(|| black_box(TaskGraph::build_from_tasks(black_box(tasks)).unwrap()))
        });
    }
    group.finish();
}

/// 基准测试: topological_sort
fn bench_topological_sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("topological_sort");

    let sizes: Vec<usize> = vec![10, 100, 500, 1000];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));

        let independent = TaskGraph::build_from_tasks(&build_independent_tasks(size)).unwrap();
        let linear = TaskGraph::build_from_tasks(&build_linear_chain(size)).unwrap();
        let diamond = TaskGraph::build_from_tasks(&build_diamond(size)).unwrap();

        group.bench_with_input(
            BenchmarkId::new("independent", size),
            &independent,
            |b, graph| b.iter(|| black_box(graph.topological_sort().unwrap())),
        );

        group.bench_with_input(BenchmarkId::new("linear", size), &linear, |b, graph| {
            b.iter(|| black_box(graph.topological_sort().unwrap()))
        });

        group.bench_with_input(BenchmarkId::new("diamond", size), &diamond, |b, graph| {
            b.iter(|| black_box(graph.topological_sort().unwrap()))
        });
    }
    group.finish();
}

/// 基准测试: parallel_groups
fn bench_parallel_groups(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_groups");

    let sizes: Vec<usize> = vec![10, 100, 500, 1000];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));

        let independent = TaskGraph::build_from_tasks(&build_independent_tasks(size)).unwrap();
        let linear = TaskGraph::build_from_tasks(&build_linear_chain(size)).unwrap();
        let diamond = TaskGraph::build_from_tasks(&build_diamond(size)).unwrap();

        group.bench_with_input(
            BenchmarkId::new("independent", size),
            &independent,
            |b, graph| b.iter(|| black_box(graph.parallel_groups().unwrap())),
        );

        group.bench_with_input(BenchmarkId::new("linear", size), &linear, |b, graph| {
            b.iter(|| black_box(graph.parallel_groups().unwrap()))
        });

        group.bench_with_input(BenchmarkId::new("diamond", size), &diamond, |b, graph| {
            b.iter(|| black_box(graph.parallel_groups().unwrap()))
        });
    }
    group.finish();
}

/// 基准测试: has_cycle
fn bench_has_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("has_cycle");

    let sizes: Vec<usize> = vec![10, 100, 500, 1000];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));

        let no_cycle = TaskGraph::build_from_tasks(&build_linear_chain(size)).unwrap();
        let with_cycle = TaskGraph::build_from_tasks(&build_cyclic_tasks(size)).unwrap();

        group.bench_with_input(BenchmarkId::new("no_cycle", size), &no_cycle, |b, graph| {
            b.iter(|| black_box(graph.has_cycle()))
        });

        group.bench_with_input(
            BenchmarkId::new("with_cycle", size),
            &with_cycle,
            |b, graph| b.iter(|| black_box(graph.has_cycle())),
        );
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空任务列表
    let empty_graph = TaskGraph::build_from_tasks(&[]).unwrap();
    group.bench_function("empty_build", |b| {
        b.iter(|| black_box(TaskGraph::build_from_tasks(black_box(&[] as &[Task])).unwrap()))
    });
    group.bench_function("empty_topo_sort", |b| {
        b.iter(|| black_box(empty_graph.topological_sort().unwrap()))
    });
    group.bench_function("empty_parallel", |b| {
        b.iter(|| black_box(empty_graph.parallel_groups().unwrap()))
    });
    group.bench_function("empty_has_cycle", |b| {
        b.iter(|| black_box(empty_graph.has_cycle()))
    });

    // 单任务
    let single = vec![make_task("0-0", 0, vec![])];
    let single_graph = TaskGraph::build_from_tasks(&single).unwrap();
    group.bench_function("single_topo_sort", |b| {
        b.iter(|| black_box(single_graph.topological_sort().unwrap()))
    });
    group.bench_function("single_max_parallelism", |b| {
        b.iter(|| black_box(single_graph.max_parallelism().unwrap()))
    });

    // 自循环 (单任务依赖自身)
    let self_dep = vec![make_task("0-0", 0, vec!["0-0".to_string()])];
    let self_graph = TaskGraph::build_from_tasks(&self_dep).unwrap();
    group.bench_function("self_cycle_has_cycle", |b| {
        b.iter(|| black_box(self_graph.has_cycle()))
    });

    // all_dependencies 传递闭包
    let chain_graph = TaskGraph::build_from_tasks(&build_linear_chain(100)).unwrap();
    group.bench_function("all_deps_chain_100", |b| {
        b.iter(|| black_box(chain_graph.all_dependencies(black_box(99))))
    });

    // 深度链 1000
    let deep_chain = TaskGraph::build_from_tasks(&build_linear_chain(1000)).unwrap();
    group.bench_function("deep_chain_topo_sort", |b| {
        b.iter(|| black_box(deep_chain.topological_sort().unwrap()))
    });

    group.finish();
}

/// 配置基准测试参数
fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/task_graph"))
}

criterion_group! {
    name = task_graph_benches;
    config = configure_criterion();
    targets =
        bench_build_from_tasks,
        bench_topological_sort,
        bench_parallel_groups,
        bench_has_cycle,
        bench_edge_cases,
}

criterion_main!(task_graph_benches);
