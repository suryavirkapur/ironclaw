use super::{
    CodeExecutionTool, FileReadTool, FileWriteTool, RestrictedBashTool, Tool, ToolCallTool,
    ToolInstallTool, ToolRegistry,
};

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
