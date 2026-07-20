use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CgroupError {
    #[error("cgroup error: {0}")]
    Cgroup(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct CgroupManager {
    user_id: String,
    cgroup_path: PathBuf,
}

impl CgroupManager {
    pub fn new(user_id: &str) -> Self {
        Self {
            user_id: user_id.to_string(),
            cgroup_path: PathBuf::from("/sys/fs/cgroup")
                .join("ironclaw")
                .join(user_id),
        }
    }

    pub fn setup(&self, memory_limit_mb: u32) -> Result<(), CgroupError> {
        self.ensure_root()?;

        std::fs::create_dir_all(&self.cgroup_path)
            .map_err(|e| CgroupError::Cgroup(format!("create cgroup dir failed: {}", e)))?;

        std::fs::write(
            self.cgroup_path.join("memory.max"),
            format!("{}M", memory_limit_mb),
        )
        .map_err(|e| CgroupError::Cgroup(format!("set memory limit failed: {}", e)))?;

        std::fs::write(self.cgroup_path.join("memory.swap.max"), "0")
            .map_err(|e| CgroupError::Cgroup(format!("disable swap failed: {}", e)))?;

        Ok(())
    }

    pub fn add_process(&self, pid: u32) -> Result<(), CgroupError> {
        std::fs::write(self.cgroup_path.join("cgroup.procs"), pid.to_string())
            .map_err(|e| CgroupError::Cgroup(format!("add process to cgroup failed: {}", e)))?;
        Ok(())
    }

    pub fn cleanup(&self) -> Result<(), CgroupError> {
        if self.cgroup_path.exists() {
            std::fs::remove_dir(&self.cgroup_path)
                .map_err(|e| CgroupError::Cgroup(format!("remove cgroup failed: {}", e)))?;
        }
        Ok(())
    }

    fn ensure_root(&self) -> Result<(), CgroupError> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| CgroupError::Cgroup(e.to_string()))?;

        if String::from_utf8_lossy(&output.stdout).trim() != "0" {
            return Err(CgroupError::Cgroup(
                "must run as root for cgroup management".to_string(),
            ));
        }
        Ok(())
    }
}

pub fn setup_process_cgroup(user_id: &str, memory_limit_mb: u32) -> Result<(), CgroupError> {
    let manager = CgroupManager::new(user_id);
    manager.setup(memory_limit_mb)?;
    Ok(())
}

pub fn add_to_cgroup(user_id: &str, pid: u32) -> Result<(), CgroupError> {
    let manager = CgroupManager::new(user_id);
    manager.add_process(pid)
}

pub fn cleanup_cgroup(user_id: &str) -> Result<(), CgroupError> {
    let manager = CgroupManager::new(user_id);
    manager.cleanup()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cgroup_path() {
        let m = CgroupManager::new("testuser");
        assert_eq!(
            m.cgroup_path,
            PathBuf::from("/sys/fs/cgroup/ironclaw/testuser")
        );
    }
}
