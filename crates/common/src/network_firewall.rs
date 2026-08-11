use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FirewallError {
    #[error("iptables failed: {0}")]
    Iptables(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct NetworkFirewall {
    chain_prefix: String,
}

impl NetworkFirewall {
    pub fn new(user_id: &str) -> Self {
        Self {
            chain_prefix: format!("IRONCLAW_{}", sanitize_name(user_id)),
        }
    }

    pub fn setup(&self, allowed_domains: &[String]) -> Result<(), FirewallError> {
        let chain = &self.chain_prefix;

        self.ensure_root()?;

        self.run_iptables(&["-N", chain])?;
        self.run_iptables(&["-F", chain])?;

        if allowed_domains.is_empty() {
            self.run_iptables(&["-A", chain, "-j", "ACCEPT"])?;
        } else {
            for domain in allowed_domains {
                let ip = resolve_domain(domain)?;
                self.run_iptables(&["-A", chain, "-d", &ip, "-j", "ACCEPT"])?;
            }
            self.run_iptables(&["-j", "DROP"])?;
        }

        Ok(())
    }

    pub fn cleanup(&self) -> Result<(), FirewallError> {
        let chain = &self.chain_prefix;

        self.run_iptables(&["-F", chain])?;
        self.run_iptables(&["-X", chain])?;

        Ok(())
    }

    fn ensure_root(&self) -> Result<(), FirewallError> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| FirewallError::Iptables(e.to_string()))?;

        if String::from_utf8_lossy(&output.stdout).trim() != "0" {
            return Err(FirewallError::Iptables("must run as root".to_string()));
        }
        Ok(())
    }

    fn run_iptables(&self, args: &[&str]) -> Result<(), FirewallError> {
        let output = Command::new("iptables")
            .args(["-w"])
            .args(args)
            .output()
            .map_err(|e| FirewallError::Iptables(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(FirewallError::Iptables(stderr.to_string()));
        }
        Ok(())
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

fn resolve_domain(domain: &str) -> Result<String, FirewallError> {
    let output = Command::new("getent")
        .args(["hosts", domain])
        .output()
        .map_err(|e| FirewallError::Iptables(format!("dns lookup failed: {}", e)))?;

    if !output.status.success() {
        return Err(FirewallError::Iptables(format!(
            "cannot resolve domain: {}",
            domain
        )));
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let ip = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("0.0.0.0")
        .to_string();

    Ok(ip)
}

pub fn setup_vm_network(user_id: &str, allowed_domains: &[String]) -> Result<(), FirewallError> {
    let _ = user_id;
    if !allowed_domains.is_empty() {
        tracing::warn!(
            "direct Ubuntu guest networking is enabled; domain allowlists are not enforced at the TAP layer"
        );
    }
    if let Err(err) = std::fs::write("/proc/sys/net/ipv4/ip_forward", "1\n") {
        tracing::warn!(
            "could not enable ip_forward from daemon ({err}); it must be enabled by host setup"
        );
    }
    ensure_iptables_rule(&[], &["FORWARD", "-i", "tap+", "-j", "ACCEPT"])?;
    ensure_iptables_rule(
        &[],
        &[
            "FORWARD",
            "-o",
            "tap+",
            "-m",
            "conntrack",
            "--ctstate",
            "ESTABLISHED,RELATED",
            "-j",
            "ACCEPT",
        ],
    )?;
    ensure_iptables_rule(
        &["-t", "nat"],
        &["POSTROUTING", "-s", "172.16.0.0/24", "-j", "MASQUERADE"],
    )?;
    Ok(())
}

pub fn cleanup_vm_network(user_id: &str) -> Result<(), FirewallError> {
    let _ = user_id;
    Ok(())
}

fn ensure_iptables_rule(table_args: &[&str], rule: &[&str]) -> Result<(), FirewallError> {
    let check = Command::new("iptables")
        .arg("-w")
        .args(table_args)
        .arg("-C")
        .args(rule)
        .output()?;
    if check.status.success() {
        return Ok(());
    }
    let add = Command::new("iptables")
        .arg("-w")
        .args(table_args)
        .arg("-A")
        .args(rule)
        .output()?;
    if !add.status.success() {
        return Err(FirewallError::Iptables(
            String::from_utf8_lossy(&add.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("user@123"), "user123");
        assert_eq!(sanitize_name("user_name"), "user_name");
        assert_eq!(sanitize_name("a-b-c"), "abc");
    }
}
