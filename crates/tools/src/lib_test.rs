use super::{
    BrowserAutomationTool, CodeExecutionTool, FileReadTool, FileWriteTool, RestrictedBashTool,
    Tool, ToolCallTool, ToolInstallTool, ToolRegistry,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn temp_workspace(name: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("ironclaw-tools-{name}-{ts}"));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::create_dir_all(&path);
    path
}

#[test]
fn file_write_and_read_work_with_allowlist() {
    let root = temp_workspace("allow");
    let mut registry = ToolRegistry::new(&["file_read".to_string(), "file_write".to_string()]);
    registry.register("file_read", Box::new(FileReadTool::new(root.clone())));
    registry.register("file_write", Box::new(FileWriteTool::new(root.clone())));

    let write = registry.execute("file_write", "notes/a.txt\nhello");
    assert!(write.is_ok());

    let read = registry.execute("file_read", "notes/a.txt");
    assert!(read.is_ok());
    assert_eq!(read.map(|r| r.output).unwrap_or_default(), "hello");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn file_read_blocks_parent_path_escape() {
    let root = temp_workspace("escape");
    let mut registry = ToolRegistry::new(&["file_read".to_string()]);
    registry.register("file_read", Box::new(FileReadTool::new(root.clone())));

    let result = registry.execute("file_read", "../etc/passwd");
    assert!(result.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn registry_blocks_non_allowlisted_tool() {
    let root = temp_workspace("deny");
    let mut registry = ToolRegistry::new(&[]);
    registry.register("file_read", Box::new(FileReadTool::new(root.clone())));

    let result = registry.execute("file_read", "notes/a.txt");
    assert!(result.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn file_write_blocks_absolute_path_escape() {
    let root = temp_workspace("abs-escape");
    let mut registry = ToolRegistry::new(&["file_write".to_string()]);
    registry.register("file_write", Box::new(FileWriteTool::new(root.clone())));

    let result = registry.execute("file_write", "/etc/passwd\nblocked");
    assert!(result.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn file_write_blocks_parent_path_escape() {
    let root = temp_workspace("parent-escape");
    let mut registry = ToolRegistry::new(&["file_write".to_string()]);
    registry.register("file_write", Box::new(FileWriteTool::new(root.clone())));

    let result = registry.execute("file_write", "../outside.txt\nblocked");
    assert!(result.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn restricted_bash_blocks_dangerous_commands() {
    let root = temp_workspace("bash-policy");
    let bash = RestrictedBashTool::new(true, root.clone());

    let sudo = bash.run("sudo ls");
    assert!(sudo.is_err());

    let rm = bash.run("rm -rf /tmp/something");
    assert!(rm.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn code_exec_runs_python() {
    let root = temp_workspace("code-exec");
    let exec = CodeExecutionTool::new(root.clone(), 10, vec![]);

    let input = serde_json::json!({
        "language": "python",
        "code": "print('hello from python')"
    });

    let result = exec.run(&input.to_string());
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.ok);

    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert!(output["stdout"]
        .as_str()
        .unwrap()
        .contains("hello from python"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn code_exec_runs_node() {
    let root = temp_workspace("code-exec-node");
    let exec = CodeExecutionTool::new(root.clone(), 10, vec![]);

    let input = serde_json::json!({
        "language": "node",
        "code": "console.log('hello from node')"
    });

    let result = exec.run(&input.to_string());
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.ok);

    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert!(output["stdout"]
        .as_str()
        .unwrap()
        .contains("hello from node"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn code_exec_times_out() {
    let root = temp_workspace("code-exec-timeout");
    let exec = CodeExecutionTool::new(root.clone(), 1, vec![]);

    let input = serde_json::json!({
        "language": "python",
        "code": "import time; time.sleep(10)"
    });

    let result = exec.run(&input.to_string());
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(!result.ok);

    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert!(output["timed_out"].as_bool().unwrap());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn code_exec_blocks_network_when_allowlist_set() {
    let root = temp_workspace("code-exec-network");
    let exec = CodeExecutionTool::new(root.clone(), 10, vec!["example.com".to_string()]);

    let input = serde_json::json!({
        "language": "python",
        "code": "import urllib.request; print(urllib.request.urlopen('http://evil.com').read())"
    });

    let result = exec.run(&input.to_string());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("network") || err.message.contains("allowed"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tool_install_saves_tool() {
    let root = temp_workspace("tool-install");
    let tools_dir = root.join("tools");
    std::fs::create_dir_all(&tools_dir).unwrap();

    let install = ToolInstallTool::new(tools_dir.clone(), root.clone(), 30, vec![]);

    let input = serde_json::json!({
        "name": "my_analyzer",
        "language": "python",
        "code": "print('analyzer running')",
        "description": "Test analyzer tool"
    });

    let result = install.run(&input.to_string());
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.ok);
    assert!(result.output.contains("my_analyzer"));

    let meta_path = tools_dir.join("my_analyzer.meta.json");
    assert!(meta_path.exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tool_install_validates_name() {
    let root = temp_workspace("tool-install-name");
    let tools_dir = root.join("tools");
    std::fs::create_dir_all(&tools_dir).unwrap();

    let install = ToolInstallTool::new(tools_dir.clone(), root.clone(), 30, vec![]);

    let input = serde_json::json!({
        "name": "invalid name!",
        "language": "python",
        "code": "print('test')"
    });

    let result = install.run(&input.to_string());
    assert!(result.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tool_call_runs_installed_tool() {
    let root = temp_workspace("tool-call");
    let tools_dir = root.join("tools");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    let install = ToolInstallTool::new(tools_dir.clone(), workspace.clone(), 30, vec![]);

    let install_input = serde_json::json!({
        "name": "analyzer",
        "language": "python",
        "code": "print('tool executed successfully')\nprint('analyzer ready')",
        "description": "Test analyzer tool"
    });

    install.run(&install_input.to_string()).unwrap();

    let call = ToolCallTool::new(tools_dir.clone(), workspace.clone(), 30, vec![]);
    let result = call.run("analyzer");
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.ok);
    assert!(result.output.contains("tool executed successfully"));
    assert!(result.output.contains("analyzer ready"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn browser_tool_rejects_unsafe_command() {
    let root = temp_workspace("browser-block");
    let browser = BrowserAutomationTool::with_executable("agent-browser", root.clone(), vec![]);

    let result = browser.run(r#"{"command":"eval","args":["window.location.href"]}"#);
    assert!(result.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn browser_tool_wraps_agent_browser_with_guardrails() {
    let root = temp_workspace("browser-fake");
    let script_path = root.join("fake-agent-browser.sh");
    let args_path = root.join("browser-args.txt");
    let home_path = root.join("browser-home.txt");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nprintf '%s\n' \"$@\" > \"{}\"\nprintf '%s' \"$HOME\" > \"{}\"\nprintf '{{\"success\":true}}'\n",
            args_path.display(),
            home_path.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).unwrap();

    let browser = BrowserAutomationTool::with_executable(
        script_path.display().to_string(),
        root.clone(),
        vec!["example.com".to_string(), "www.example.com".to_string()],
    );

    let result = browser.run(r#"{"command":"snapshot","args":["-i","-c"]}"#);
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.ok);
    assert!(result.output.contains("success"));

    let args = std::fs::read_to_string(&args_path).unwrap();
    assert!(args.contains("--json"));
    assert!(args.contains("--content-boundaries"));
    assert!(args.contains("--max-output"));
    assert!(args.contains("50000"));
    assert!(args.contains("--allowed-domains"));
    assert!(args.contains("example.com,www.example.com"));
    assert!(args.contains("--session"));
    assert!(args.contains("ironclaw"));
    assert!(args.contains("snapshot"));
    assert!(args.contains("-i"));
    assert!(args.contains("-c"));

    let home = std::fs::read_to_string(&home_path).unwrap();
    assert!(home.contains(".agent-browser-home"));

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn browser_action_tool_navigate_converts_correctly() {
    use super::BrowserActionTool;
    let root = temp_workspace("browser-action-nav");
    let script_path = root.join("fake-browser.sh");
    let args_path = root.join("browser-args.txt");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nprintf '%s\n' \"$@\" > \"{}\"\nprintf '{{\"success\":true}}'\n",
            args_path.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let tool =
        BrowserActionTool::with_executable(script_path.display().to_string(), root.clone(), vec![]);
    let result = tool
        .run(r#"{"action":"navigate","url":"https://example.com"}"#)
        .unwrap();
    assert!(result.ok);

    let args = std::fs::read_to_string(&args_path).unwrap();
    assert!(args.contains("open"));
    assert!(args.contains("https://example.com"));

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn browser_action_tool_snapshot_interactive_converts_correctly() {
    use super::BrowserActionTool;
    let root = temp_workspace("browser-action-snap");
    let script_path = root.join("fake-browser.sh");
    let args_path = root.join("browser-args.txt");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nprintf '%s\n' \"$@\" > \"{}\"\nprintf '{{\"success\":true}}'\n",
            args_path.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let tool =
        BrowserActionTool::with_executable(script_path.display().to_string(), root.clone(), vec![]);
    let result = tool
        .run(r#"{"action":"snapshot","interactive":true,"compact":true}"#)
        .unwrap();
    assert!(result.ok);

    let args = std::fs::read_to_string(&args_path).unwrap();
    assert!(args.contains("snapshot"));
    assert!(args.contains("-i"));
    assert!(args.contains("-c"));

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn browser_action_tool_click_converts_correctly() {
    use super::BrowserActionTool;
    let root = temp_workspace("browser-action-click");
    let script_path = root.join("fake-browser.sh");
    let args_path = root.join("browser-args.txt");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nprintf '%s\n' \"$@\" > \"{}\"\nprintf '{{\"success\":true}}'\n",
            args_path.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let tool =
        BrowserActionTool::with_executable(script_path.display().to_string(), root.clone(), vec![]);
    let result = tool.run(r#"{"action":"click","ref":"e5"}"#).unwrap();
    assert!(result.ok);

    let args = std::fs::read_to_string(&args_path).unwrap();
    assert!(args.contains("click"));
    assert!(args.contains("@e5"));

    let _ = std::fs::remove_dir_all(&root);
}
