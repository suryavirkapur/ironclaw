use super::{FileReadTool, FileWriteTool, ToolRegistry};

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
