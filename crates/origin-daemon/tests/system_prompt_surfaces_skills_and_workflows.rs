//! Regression: the agent's system prompt must surface enough doctrine for
//! the model to answer "what skills do you have?" and "what workflows do
//! you have?" without hallucinating.
//!
//! It is not enough to embed the skills catalog only; the model also needs
//! the `default_workflow::directive()` framing (names brainstorming /
//! writing-plans / dispatching-parallel-agents / TDD /
//! verification-before-completion as the orchestration steps) and a list
//! of user workflows from `workflows.toml`. Origin used to leak both —
//! the agent treated catalog_block + recall_block as the entire system
//! prompt and the model would invent capabilities and workflows from the
//! tool list alone.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_daemon::skill_catalog::SkillCatalog;
use origin_daemon::workflows::{Workflow, WorkflowStep, WorkflowsFile};
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

struct CapturingProvider {
    seen_systems: Mutex<Vec<String>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    fn name(&self) -> &'static str {
        "capturing"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen_systems.lock().expect("lock").push(req.system);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

fn write_skill(dir: &std::path::Path, name: &str, desc: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("mkdir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\nallowed-tools: [\"Read\"]\n---\nbody\n"),
    )
    .expect("write");
}

#[tokio::test]
async fn system_prompt_opens_with_origin_identity_block() {
    // Without an identity block, models with strong Claude-Code training
    // priors keep answering as CC and ignore origin's directive/skills.
    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let sys = provider.seen_systems.lock().expect("lock").remove(0);
    assert!(
        sys.starts_with("<origin-identity>"),
        "system prompt must open with <origin-identity>:\n{sys}"
    );
    assert!(
        sys.contains("You are Origin"),
        "identity block must name the agent:\n{sys}"
    );
    assert!(
        sys.contains("NOT Claude Code"),
        "identity block must disclaim CC behavior:\n{sys}"
    );
}

#[tokio::test]
async fn system_prompt_includes_default_workflow_directive() {
    // No skills, no workflows — directive should still land so the model
    // knows the brainstorm → plan → dispatch flow exists.
    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let sys = provider.seen_systems.lock().expect("lock").remove(0);
    assert!(
        sys.contains("/brainstorming"),
        "system prompt missing default-workflow directive:\n{sys}"
    );
    assert!(
        sys.contains("/writing-plans"),
        "default-workflow directive missing /writing-plans:\n{sys}"
    );
    assert!(
        sys.contains("/dispatching-parallel-agents"),
        "directive missing /dispatching-parallel-agents:\n{sys}"
    );
}

#[tokio::test]
async fn system_prompt_lists_workflows_by_name() {
    let workflows = Arc::new(WorkflowsFile {
        schema_version: 1,
        workflows: vec![
            Workflow {
                name: "frontend-design".into(),
                description: Some(
                    "Two-step UI feature build: shape with frontend-design, then teach impeccable."
                        .into(),
                ),
                steps: vec![
                    WorkflowStep {
                        skill: "frontend-design:frontend-design".into(),
                        args: None,
                    },
                    WorkflowStep {
                        skill: "impeccable".into(),
                        args: Some("teach".into()),
                    },
                ],
            },
        ],
    });

    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        workflows: Some(workflows),
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let sys = provider.seen_systems.lock().expect("lock").remove(0);
    assert!(
        sys.contains("<origin-workflows>"),
        "system prompt missing <origin-workflows> tag:\n{sys}"
    );
    assert!(
        sys.contains("frontend-design"),
        "system prompt missing workflow name `frontend-design`:\n{sys}"
    );
}

#[tokio::test]
async fn system_prompt_combines_directive_skills_and_workflows() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_skill(dir.path(), "alpha", "Does alpha things");
    let catalog = Arc::new(SkillCatalog::load_from(dir.path()).expect("load"));
    let workflows = Arc::new(WorkflowsFile {
        schema_version: 1,
        workflows: vec![Workflow {
            name: "ship-it".into(),
            description: Some("plan then merge".into()),
            steps: vec![WorkflowStep {
                skill: "writing-plans".into(),
                args: None,
            }],
        }],
    });

    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        skill_catalog: Some(Arc::clone(&catalog)),
        workflows: Some(Arc::clone(&workflows)),
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let sys = provider.seen_systems.lock().expect("lock").remove(0);
    assert!(sys.contains("/brainstorming"), "directive missing:\n{sys}");
    assert!(sys.contains("alpha"), "skills missing:\n{sys}");
    assert!(sys.contains("ship-it"), "workflows missing:\n{sys}");
}
