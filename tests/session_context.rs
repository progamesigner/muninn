//! Integration coverage for the session-context resource and prompt surfaces
//! (change `configurable-session-context`, tasks 7.3).
//!
//! Drives the real `muninn` binary over stdio and exercises
//! `resources/templates/list` + `resources/read` and `prompts/list` +
//! `prompts/get`, including the empty-vault case and a VFS-scheme variation.

use rmcp::model::{
    CallToolRequestParams, GetPromptRequestParams, PromptMessageContent, ReadResourceRequestParams,
    ResourceContents,
};
use rmcp::service::ServiceExt;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::json;
use tokio::process::Command;

/// Launch the server over stdio with the given VFS scheme.
async fn serve(
    tmp: &assert_fs::TempDir,
    scheme: &str,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_muninn");
    ().serve(
        TokioChildProcess::new(Command::new(bin).configure(|cmd| {
            cmd.env("MUNINN_ROOT_DIR", tmp.path());
            cmd.env("MUNINN_TRANSPORT", "stdio");
            cmd.env("MUNINN_VFS_SCHEME", scheme);
        }))
        .unwrap(),
    )
    .await
    .expect("server should initialize")
}

fn resource_text(result: &rmcp::model::ReadResourceResult) -> String {
    match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        _ => panic!("expected text resource contents"),
    }
}

fn prompt_text(result: &rmcp::model::GetPromptResult) -> String {
    match &result.messages[0].content {
        PromptMessageContent::Text { text } => text.clone(),
        _ => panic!("expected text prompt content"),
    }
}

#[tokio::test]
async fn resource_template_and_read_render_context() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let service = serve(&tmp, "<agent>.<user>").await;

    // Seed a foundational file for jarvis/tony.
    service
        .call_tool(
            CallToolRequestParams::new("evolve_core_persona").with_arguments(
                json!({"agent":"jarvis","user":"tony","which":"persona","content":"PERSONA-BODY"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();

    // resources/templates/list → the three session resources, URI params follow
    // the scheme.
    let templates = service
        .list_resource_templates(Default::default())
        .await
        .unwrap();
    let uris: Vec<&str> = templates
        .resource_templates
        .iter()
        .map(|t| t.uri_template.as_str())
        .collect();
    assert_eq!(templates.resource_templates.len(), 3);
    assert!(uris.contains(&"muninn://session-context/{agent}/{user}"));
    assert!(uris.contains(&"muninn://session-bootstrap/{agent}/{user}"));
    assert!(uris.contains(&"muninn://session-layout/{agent}/{user}"));

    // resources/read for a populated scope renders the persona body.
    let read = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-context/jarvis/tony",
        ))
        .await
        .unwrap();
    let text = resource_text(&read);
    assert!(text.contains("PERSONA-BODY"));
    assert!(text.contains("# Session Context"));

    // Empty-vault scope still succeeds, with the missing sentinel.
    let read_empty = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-context/jarvis/sam",
        ))
        .await
        .unwrap();
    assert!(resource_text(&read_empty).contains("(not yet recorded"));

    service.cancel().await.unwrap();
}

#[tokio::test]
async fn bootstrap_and_layout_resources_render() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let service = serve(&tmp, "<agent>.<user>").await;

    service
        .call_tool(
            CallToolRequestParams::new("evolve_core_persona").with_arguments(
                json!({"agent":"jarvis","user":"tony","which":"rules","content":"RULES-BODY"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();

    // The lean bootstrap resource renders the scope banner, pointers, and the
    // untagged rules — no persona, no heavier slots, no wrapper tags.
    let boot = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-bootstrap/jarvis/tony",
        ))
        .await
        .unwrap();
    let boot_text = resource_text(&boot);
    assert!(boot_text.contains("# Session Bootstrap"));
    assert!(boot_text.contains("RULES-BODY"));
    assert!(boot_text.contains("load_session_context"));
    assert!(!boot_text.contains("<PERSONA>"));
    assert!(!boot_text.contains("<RULES>"));
    assert!(!boot_text.contains("<MEMORY>"));

    // The layout resource renders the vault-mechanics guidance.
    let layout = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-layout/jarvis/tony",
        ))
        .await
        .unwrap();
    let layout_text = resource_text(&layout);
    assert!(layout_text.contains("# Memory Layout"));
    assert!(layout_text.contains("ordinary filesystem"));

    service.cancel().await.unwrap();
}

#[tokio::test]
async fn onboarding_directive_appears_for_a_fresh_scope() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let service = serve(&tmp, "<agent>.<user>").await;

    // A fresh scope: every foundational file is missing.
    let read = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-context/jarvis/tony",
        ))
        .await
        .unwrap();
    let text = resource_text(&read);
    assert!(text.contains("(not yet recorded"));
    // The onboarding directive prompts an interview, then a batch commit.
    assert!(text.contains("Onboarding needed"));
    assert!(text.contains("identity, role, working style, and boundaries"));
    assert!(text.contains("evolve_core_persona"));
    assert!(text.contains("`updates` batch form"));

    service.cancel().await.unwrap();
}

#[tokio::test]
async fn prompt_lists_args_and_renders() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let service = serve(&tmp, "<agent>.<user>").await;

    service
        .call_tool(
            CallToolRequestParams::new("evolve_core_persona").with_arguments(
                json!({"agent":"jarvis","user":"tony","which":"persona","content":"PROMPT-PERSONA"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();

    // prompts/list → required args follow the scheme.
    let prompts = service.list_prompts(Default::default()).await.unwrap();
    assert_eq!(prompts.prompts.len(), 1);
    let p = &prompts.prompts[0];
    assert_eq!(p.name, "session-context");
    let arg_names: Vec<&str> = p
        .arguments
        .as_ref()
        .unwrap()
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(arg_names, vec!["agent", "user"]);

    // prompts/get renders the context for the supplied scope.
    let got = service
        .get_prompt(
            GetPromptRequestParams::new("session-context").with_arguments(
                json!({"agent":"jarvis","user":"tony"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert!(prompt_text(&got).contains("PROMPT-PERSONA"));

    // Missing required argument is rejected.
    let err = service
        .get_prompt(
            GetPromptRequestParams::new("session-context")
                .with_arguments(json!({"agent":"jarvis"}).as_object().unwrap().clone()),
        )
        .await;
    assert!(err.is_err(), "missing scope arg should error");

    service.cancel().await.unwrap();
}

#[tokio::test]
async fn surfaces_follow_a_custom_scheme() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let service = serve(&tmp, "<agent>").await;

    let templates = service
        .list_resource_templates(Default::default())
        .await
        .unwrap();
    assert_eq!(
        templates.resource_templates[0].uri_template,
        "muninn://session-context/{agent}"
    );

    let prompts = service.list_prompts(Default::default()).await.unwrap();
    let arg_names: Vec<&str> = prompts.prompts[0]
        .arguments
        .as_ref()
        .unwrap()
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(arg_names, vec!["agent"]);

    // A single-segment read resolves under the one-key scheme.
    let read = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-context/jarvis",
        ))
        .await
        .unwrap();
    assert!(resource_text(&read).contains("# Session Context"));

    service.cancel().await.unwrap();
}
