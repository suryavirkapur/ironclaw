use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum QuotaError {
    #[error("quota error: {0}")]
    Quota(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct DiskQuota {
    user_id: String,
    limit_bytes: u64,
}

impl DiskQuota {
    pub fn new(user_id: &str, limit_mb: u32) -> Self {
        Self {
            user_id: user_id.to_string(),
            limit_bytes: (limit_mb as u64) * 1024 * 1024,
        }
    }

    pub fn setup(&self, brain_path: &Path) -> Result<(), QuotaError> {
        self.ensure_root()?;

        let quota_file = brain_path.join(".quota");

        Command::new("setquota")
            .args(["-u", &self.user_id])
            .args(["0"])
            .args([&(self.limit_bytes / 1024).to_string()])
            .args(["0"])
            .args(["0"])
            .arg(brain_path.to_str().unwrap())
            .output()
            .map_err(|e| QuotaError::Quota(e.to_string()))?;

        Ok(())
    }

    pub fn check_usage(&self, brain_path: &Path) -> Result<u64, QuotaError> {
        let output = Command::new("quota")
            .args(["-u", &self.user_id])
            .args(["--no-pipe", "-w"])
            .output()
            .map_err(|e| QuotaError::Quota(e.to_string()))?;

        if !output.status.success() {
            return Ok(0);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains(&self.user_id) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(used) = parts[1].parse::<u64>() {
                        return Ok(used * 1024);
                    }
                }
            }
        }

        Ok(0)
    }

    pub fn enforce_limit(
        &self,
        brain_path: &Path,
        additional_bytes: u64,
    ) -> Result<(), QuotaError> {
        let current_usage = self.check_usage(brain_path)?;

        if current_usage + additional_bytes > self.limit_bytes {
            return Err(QuotaError::Quota(format!(
                "disk quota exceeded: {} + {} > {} bytes",
                current_usage, additional_bytes, self.limit_bytes
            )));
        }

        Ok(())
    }

    fn ensure_root(&self) -> Result<(), QuotaError> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| QuotaError::Quota(e.to_string()))?;

        if String::from_utf8_lossy(&output.stdout).trim() != "0" {
            return Err(QuotaError::Quota(
                "must run as root for quota management".to_string(),
            ));
        }
        Ok(())
    }
}

pub fn setup_user_quota(user_id: &str, brain_path: &Path, limit_mb: u32) -> Result<(), QuotaError> {
    let quota = DiskQuota::new(user_id, limit_mb);
    quota.setup(brain_path)
}

pub fn check_quota(user_id: &str, brain_path: &Path, limit_mb: u32) -> Result<u64, QuotaError> {
    let quota = DiskQuota::new(user_id, limit_mb);
    quota.check_usage(brain_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disk_quota_creation() {
        let q = DiskQuota::new("testuser", 512);
        assert_eq!(q.limit_bytes, 512 * 1024 * 1024);
    }
}
