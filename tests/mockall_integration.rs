//! mockall 框架集成测试 (Session 68)
//!
//! 使用 mockall 替代手写 Mock, 验证 trait mock 的协作:
//! - MockChatClient: 可编程回复序列、超时模拟、调用计数
//! - MockTestRunner: 可编程编译/测试结果
//! - MockFileExtractor: 可编程文件提取结果
//! - MockHumanInteraction: 可编程确认/拒绝响应
//! - MockClarificationChecker: 可编程追问判断
//!
//! 对比手写 Mock (orchestrator_dip.rs), mockall 提供:
//! - 更简洁的 API (.expect_*().return_*.())
//! - 自动调用验证 (.expect_*() 必须被调用)
//! - 序列匹配 (.returning(|msg, _| ...))
//! - 无需手动维护 Mutex/Vec 状态

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::testrunner::TestResult;
use forge::traits::{
    ChatClient, ChatResult, ClarificationChecker, ClarificationContext, ClarificationResult,
    FileExtractor, FixContext, HumanInteraction, PlanInfo, TaskAction, TaskInfo, TestRunner,
};
use mockall::mock;
use std::path::Path;

// ============================================================================
//  mockall mock 定义
// ============================================================================

mock! {
    /// mockall 生成的 ChatClient mock
    ChatClientMock {}

    #[async_trait]
    impl ChatClient for ChatClientMock {
        async fn send_message(&self, msg: &str, timeout: u64) -> anyhow::Result<ChatResult>;
        async fn start_new_conversation(&self) -> anyhow::Result<()>;
        fn conversation_turn_count(&self) -> usize;
    }
}

mock! {
    /// mockall 生成的 TestRunner mock
    TestRunnerMock {}

    impl TestRunner for TestRunnerMock {
        fn check(&self, dir: &Path) -> anyhow::Result<TestResult>;
        fn test(&self, dir: &Path) -> anyhow::Result<TestResult>;
    }
}

mock! {
    /// mockall 生成的 FileExtractor mock
    FileExtractorMock {}

    impl FileExtractor for FileExtractorMock {
        fn extract(&self, text: &str) -> Vec<ExtractedFile>;
    }
}

mock! {
    /// mockall 生成的 HumanInteraction mock
    HumanInteractionMock {}

    #[async_trait]
    impl HumanInteraction for HumanInteractionMock {
        async fn confirm_planning(&self, plan: &PlanInfo) -> anyhow::Result<bool>;
        async fn confirm_task(&self, task: &TaskInfo) -> anyhow::Result<TaskAction>;
        async fn confirm_fix(&self, context: &FixContext) -> anyhow::Result<bool>;
        async fn confirm_requirement_change(&self, changes_summary: &str) -> anyhow::Result<bool>;
    }
}

mock! {
    /// mockall 生成的 ClarificationChecker mock
    ClarificationCheckerMock {}

    #[async_trait]
    impl ClarificationChecker for ClarificationCheckerMock {
        async fn check(
            &self,
            response: &str,
            context: &ClarificationContext,
        ) -> ClarificationResult;
    }
}

// ============================================================================
//  辅助函数
// ============================================================================

/// 构建成功编译的 TestResult
fn success_result() -> TestResult {
    TestResult {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        errors: vec![],
        test_summary: None,
    }
}

/// 构建失败编译的 TestResult
fn failure_result() -> TestResult {
    TestResult {
        success: false,
        stdout: String::new(),
        stderr: "error: expected `;`".to_string(),
        exit_code: 1,
        errors: vec![],
        test_summary: None,
    }
}

fn make_extracted_file(path: &str, content: &str) -> ExtractedFile {
    ExtractedFile {
        path: path.to_string(),
        content: content.to_string(),
        language: String::new(),
    }
}

// ============================================================================
//  mockall ChatClient 测试
// ============================================================================

#[tokio::test]
async fn test_mockall_chat_client_send_message() {
    let mut mock = MockChatClientMock::new();
    mock.expect_send_message()
        .times(1)
        .returning(|msg, _timeout| {
            Ok(ChatResult {
                text: format!("Response to: {}", msg),
                timed_out: false,
            })
        });

    let result = mock.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "Response to: hello");
    assert!(!result.timed_out);
}

#[tokio::test]
async fn test_mockall_chat_client_sequence() {
    let mut mock = MockChatClientMock::new();
    mock.expect_send_message().times(3).returning(|_, _| {
        Ok(ChatResult {
            text: "ok".to_string(),
            timed_out: false,
        })
    });

    for _ in 0..3 {
        let result = mock.send_message("msg", 60).await.unwrap();
        assert_eq!(result.text, "ok");
    }
}

#[tokio::test]
async fn test_mockall_chat_client_turn_count() {
    let mut mock = MockChatClientMock::new();
    mock.expect_conversation_turn_count()
        .times(1)
        .returning(|| 5);

    assert_eq!(mock.conversation_turn_count(), 5);
}

#[tokio::test]
async fn test_mockall_chat_client_start_new_conversation() {
    let mut mock = MockChatClientMock::new();
    mock.expect_start_new_conversation()
        .times(1)
        .returning(|| Ok(()));

    mock.start_new_conversation().await.unwrap();
}

#[tokio::test]
async fn test_mockall_chat_client_error_simulation() {
    let mut mock = MockChatClientMock::new();
    mock.expect_send_message()
        .times(1)
        .returning(|_, _| Err(anyhow::anyhow!("Connection refused")));

    let result = mock.send_message("hello", 60).await;
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Connection refused"));
}

#[tokio::test]
async fn test_mockall_chat_client_timed_out() {
    let mut mock = MockChatClientMock::new();
    mock.expect_send_message().times(1).returning(|_, _| {
        Ok(ChatResult {
            text: "partial response".to_string(),
            timed_out: true,
        })
    });

    let result = mock.send_message("hello", 60).await.unwrap();
    assert!(result.timed_out);
    assert_eq!(result.text, "partial response");
}

// ============================================================================
//  mockall TestRunner 测试
// ============================================================================

#[test]
fn test_mockall_test_runner_check_success() {
    let mut mock = MockTestRunnerMock::new();
    mock.expect_check()
        .times(1)
        .returning(|_| Ok(success_result()));

    let result = mock.check(Path::new(".")).unwrap();
    assert!(result.success);
}

#[test]
fn test_mockall_test_runner_check_failure() {
    let mut mock = MockTestRunnerMock::new();
    mock.expect_check()
        .times(1)
        .returning(|_| Ok(failure_result()));

    let result = mock.check(Path::new(".")).unwrap();
    assert!(!result.success);
}

#[test]
fn test_mockall_test_runner_test_success() {
    let mut mock = MockTestRunnerMock::new();
    mock.expect_test()
        .times(1)
        .returning(|_| Ok(success_result()));

    let result = mock.test(Path::new(".")).unwrap();
    assert!(result.success);
}

#[test]
fn test_mockall_test_runner_both_called() {
    let mut mock = MockTestRunnerMock::new();
    mock.expect_check()
        .times(1)
        .returning(|_| Ok(success_result()));
    mock.expect_test()
        .times(1)
        .returning(|_| Ok(success_result()));

    assert!(mock.check(Path::new(".")).unwrap().success);
    assert!(mock.test(Path::new(".")).unwrap().success);
}

// ============================================================================
//  mockall FileExtractor 测试
// ============================================================================

#[test]
fn test_mockall_file_extractor_extract_files() {
    let mut mock = MockFileExtractorMock::new();
    mock.expect_extract().times(1).returning(|_| {
        vec![
            make_extracted_file("src/main.rs", "fn main() {}"),
            make_extracted_file("Cargo.toml", "[package]"),
        ]
    });

    let files = mock.extract("some text");
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "src/main.rs");
    assert_eq!(files[1].path, "Cargo.toml");
}

#[test]
fn test_mockall_file_extractor_empty() {
    let mut mock = MockFileExtractorMock::new();
    mock.expect_extract().times(1).returning(|_| vec![]);

    let files = mock.extract("no code here");
    assert!(files.is_empty());
}

#[test]
fn test_mockall_file_extractor_multiple_calls() {
    let mut mock = MockFileExtractorMock::new();
    mock.expect_extract().times(3).returning(|text| {
        if text.contains("rust") {
            vec![make_extracted_file("main.rs", "fn main() {}")]
        } else {
            vec![]
        }
    });

    assert_eq!(mock.extract("rust code").len(), 1);
    assert_eq!(mock.extract("python code").len(), 0);
    assert_eq!(mock.extract("rust again").len(), 1);
}

// ============================================================================
//  mockall HumanInteraction 测试
// ============================================================================

#[tokio::test]
async fn test_mockall_human_interaction_approve_all() {
    let mut mock = MockHumanInteractionMock::new();
    mock.expect_confirm_planning()
        .times(1)
        .returning(|_| Ok(true));
    mock.expect_confirm_task()
        .times(1)
        .returning(|_| Ok(TaskAction::Execute));
    mock.expect_confirm_fix().times(1).returning(|_| Ok(true));
    mock.expect_confirm_requirement_change()
        .times(1)
        .returning(|_| Ok(true));

    assert!(mock
        .confirm_planning(&PlanInfo {
            goal: "test".to_string(),
            phases: vec![]
        })
        .await
        .unwrap());
    assert_eq!(
        mock.confirm_task(&TaskInfo {
            id: "0-0".to_string(),
            name: "Task".to_string(),
            prompt: "Do something".to_string(),
        })
        .await
        .unwrap(),
        TaskAction::Execute
    );
    assert!(mock
        .confirm_fix(&FixContext {
            phase_idx: 0,
            task_idx: 0,
            attempt: 1,
            max_attempts: 3,
            feedback: "error".to_string(),
        })
        .await
        .unwrap());
    assert!(mock.confirm_requirement_change("change").await.unwrap());
}

#[tokio::test]
async fn test_mockall_human_interaction_reject_all() {
    let mut mock = MockHumanInteractionMock::new();
    mock.expect_confirm_planning()
        .times(1)
        .returning(|_| Ok(false));
    mock.expect_confirm_task()
        .times(1)
        .returning(|_| Ok(TaskAction::Abort));
    mock.expect_confirm_fix().times(1).returning(|_| Ok(false));
    mock.expect_confirm_requirement_change()
        .times(1)
        .returning(|_| Ok(false));

    assert!(!mock
        .confirm_planning(&PlanInfo {
            goal: "test".to_string(),
            phases: vec![]
        })
        .await
        .unwrap());
    assert_eq!(
        mock.confirm_task(&TaskInfo {
            id: "0-0".to_string(),
            name: "Task".to_string(),
            prompt: "Do something".to_string(),
        })
        .await
        .unwrap(),
        TaskAction::Abort
    );
    assert!(!mock
        .confirm_fix(&FixContext {
            phase_idx: 0,
            task_idx: 0,
            attempt: 1,
            max_attempts: 3,
            feedback: "error".to_string(),
        })
        .await
        .unwrap());
    assert!(!mock.confirm_requirement_change("change").await.unwrap());
}

#[tokio::test]
async fn test_mockall_human_interaction_task_actions() {
    let mut mock = MockHumanInteractionMock::new();

    // 第一次 Execute, 第二次 Skip, 第三次 Abort
    mock.expect_confirm_task()
        .times(3)
        .returning(|task| match task.id.as_str() {
            "0-0" => Ok(TaskAction::Execute),
            "0-1" => Ok(TaskAction::Skip),
            _ => Ok(TaskAction::Abort),
        });

    let t0 = TaskInfo {
        id: "0-0".to_string(),
        name: "T0".to_string(),
        prompt: "p0".to_string(),
    };
    let t1 = TaskInfo {
        id: "0-1".to_string(),
        name: "T1".to_string(),
        prompt: "p1".to_string(),
    };
    let t2 = TaskInfo {
        id: "0-2".to_string(),
        name: "T2".to_string(),
        prompt: "p2".to_string(),
    };

    assert_eq!(mock.confirm_task(&t0).await.unwrap(), TaskAction::Execute);
    assert_eq!(mock.confirm_task(&t1).await.unwrap(), TaskAction::Skip);
    assert_eq!(mock.confirm_task(&t2).await.unwrap(), TaskAction::Abort);
}

// ============================================================================
//  mockall ClarificationChecker 测试
// ============================================================================

#[tokio::test]
async fn test_mockall_clarification_checker_yes() {
    let mut mock = MockClarificationCheckerMock::new();
    mock.expect_check()
        .times(1)
        .returning(|_, _| ClarificationResult::yes("What language?", "Ambiguous prompt"));

    let ctx = ClarificationContext {
        task_prompt: "Create a project".to_string(),
        timed_out: false,
        questions_asked: 0,
        max_questions: 3,
        previous_questions: vec![],
    };

    let result = mock.check("I'll create a project", &ctx).await;
    assert!(result.needs_clarification);
    assert_eq!(result.question, "What language?");
    assert_eq!(result.reason, "Ambiguous prompt");
}

#[tokio::test]
async fn test_mockall_clarification_checker_no() {
    let mut mock = MockClarificationCheckerMock::new();
    mock.expect_check()
        .times(1)
        .returning(|_, _| ClarificationResult::no());

    let ctx = ClarificationContext {
        task_prompt: "Create a Rust CLI".to_string(),
        timed_out: false,
        questions_asked: 0,
        max_questions: 3,
        previous_questions: vec![],
    };

    let result = mock.check("Here is the Rust code", &ctx).await;
    assert!(!result.needs_clarification);
}

#[tokio::test]
async fn test_mockall_clarification_checker_response_based() {
    let mut mock = MockClarificationCheckerMock::new();
    mock.expect_check().times(2).returning(|response, _| {
        if response.contains("not sure") {
            ClarificationResult::yes("Please clarify", "Response contains uncertainty")
        } else {
            ClarificationResult::no()
        }
    });

    let ctx = ClarificationContext {
        task_prompt: "test".to_string(),
        timed_out: false,
        questions_asked: 0,
        max_questions: 3,
        previous_questions: vec![],
    };

    let r1 = mock.check("I'm not sure what to do", &ctx).await;
    assert!(r1.needs_clarification);

    let r2 = mock.check("Here is the complete code", &ctx).await;
    assert!(!r2.needs_clarification);
}

// ============================================================================
//  mockall 验证调用次数和参数匹配
// ============================================================================

#[tokio::test]
async fn test_mockall_verify_call_count() {
    let mut mock = MockChatClientMock::new();
    mock.expect_send_message().times(3).returning(|_, _| {
        Ok(ChatResult {
            text: "ok".to_string(),
            timed_out: false,
        })
    });

    for i in 0..3 {
        let _ = mock.send_message(&format!("msg{}", i), 60).await.unwrap();
    }
    // mockall 在 drop 时验证 times(3) — 如果调用次数不匹配会 panic
}

#[tokio::test]
async fn test_mockall_predicate_eq() {
    let mut mock = MockChatClientMock::new();
    mock.expect_send_message()
        .withf(|msg, _timeout| msg == "specific message")
        .times(1)
        .returning(|_, _| {
            Ok(ChatResult {
                text: "matched".to_string(),
                timed_out: false,
            })
        });

    let result = mock.send_message("specific message", 60).await.unwrap();
    assert_eq!(result.text, "matched");
}

#[test]
fn test_mockall_never_called() {
    let mut mock = MockTestRunnerMock::new();
    mock.expect_check().times(0);
    mock.expect_test()
        .times(1)
        .returning(|_| Ok(success_result()));

    let _ = mock.test(Path::new(".")).unwrap();
    // mockall 在 drop 时验证 check 从未被调用
}

#[tokio::test]
async fn test_mockall_returning_closure_capture() {
    use std::sync::{Arc, Mutex};

    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = counter.clone();

    let mut mock = MockChatClientMock::new();
    mock.expect_send_message().times(3).returning(move |_, _| {
        let mut c = counter_clone.lock().unwrap();
        *c += 1;
        Ok(ChatResult {
            text: format!("call {}", *c),
            timed_out: false,
        })
    });

    let r1 = mock.send_message("a", 60).await.unwrap();
    let r2 = mock.send_message("b", 60).await.unwrap();
    let r3 = mock.send_message("c", 60).await.unwrap();

    assert_eq!(r1.text, "call 1");
    assert_eq!(r2.text, "call 2");
    assert_eq!(r3.text, "call 3");
    assert_eq!(*counter.lock().unwrap(), 3);
}

#[tokio::test]
async fn test_mockall_return_once_vs_returning() {
    let mut mock = MockChatClientMock::new();
    // return_once: 只调用一次, 返回特定值
    mock.expect_send_message().times(1).return_once(|_, _| {
        Ok(ChatResult {
            text: "once".to_string(),
            timed_out: false,
        })
    });

    let result = mock.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "once");
}

// ============================================================================
//  mockall trait object 测试
// ============================================================================

#[tokio::test]
async fn test_mockall_as_trait_object_chat() {
    let mut mock = MockChatClientMock::new();
    mock.expect_send_message().times(1).returning(|_, _| {
        Ok(ChatResult {
            text: "via trait object".to_string(),
            timed_out: false,
        })
    });
    mock.expect_conversation_turn_count().returning(|| 0);

    let chat: Box<dyn ChatClient> = Box::new(mock);
    let result = chat.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "via trait object");
}

#[tokio::test]
async fn test_mockall_as_trait_object_interaction() {
    let mut mock = MockHumanInteractionMock::new();
    mock.expect_confirm_planning()
        .times(1)
        .returning(|_| Ok(true));

    let interaction: Box<dyn HumanInteraction> = Box::new(mock);
    assert!(interaction
        .confirm_planning(&PlanInfo {
            goal: "test".to_string(),
            phases: vec![]
        })
        .await
        .unwrap());
}
