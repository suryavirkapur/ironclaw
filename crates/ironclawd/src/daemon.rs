use crate::IronclawError;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct CliArgs {
    pub daemon: bool,
    pub daemon_child: bool,
    pub stop: bool,
    pub telegram: bool,
    pub whatsapp: bool,
    pub pid_file: Option<PathBuf>,
    pub gateway_command: Option<GatewayCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayCommand {
    Pair {
        node_id: String,
        otp: Option<String>,
    },
    Status {
        node_id: String,
    },
}

impl CliArgs {
    pub fn parse() -> Result<Self, IronclawError> {
        let mut args = std::env::args().skip(1);
        let mut cli = Self::default();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--daemon" => cli.daemon = true,
                "--daemon-child" => cli.daemon_child = true,
                "--stop" => cli.stop = true,
                "--telegram" => cli.telegram = true,
                "--whatsapp" => cli.whatsapp = true,
                "gateway" => {
                    cli.gateway_command = Some(parse_gateway_subcommand(&mut args)?);
                }
                "--pid-file" => {
                    let Some(path) = args.next() else {
                        return Err(IronclawError::new("missing value for --pid-file"));
                    };
                    cli.pid_file = Some(PathBuf::from(path));
                }
                _ => {
                    return Err(IronclawError::new(format!("unknown argument: {arg}")));
                }
            }
        }

        Ok(cli)
    }

    pub fn should_spawn_daemon(&self) -> bool {
        self.daemon && !self.daemon_child && self.gateway_command.is_none()
    }
}

fn parse_gateway_subcommand(
    args: &mut impl Iterator<Item = String>,
) -> Result<GatewayCommand, IronclawError> {
    let Some(subcommand) = args.next() else {
        return Err(IronclawError::new("missing gateway subcommand"));
    };
    match subcommand.as_str() {
        "pair" => {
            let mut node_id: Option<String> = None;
            let mut otp: Option<String> = None;
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--node-id" => {
                        let Some(value) = args.next() else {
                            return Err(IronclawError::new("missing value for --node-id"));
                        };
                        node_id = Some(value);
                    }
                    "--otp" => {
                        let Some(value) = args.next() else {
                            return Err(IronclawError::new("missing value for --otp"));
                        };
                        otp = Some(value);
                    }
                    _ => {
                        return Err(IronclawError::new(format!(
                            "unknown gateway pair argument: {flag}"
                        )));
                    }
                }
            }
            let node_id = node_id.ok_or_else(|| IronclawError::new("missing --node-id"))?;
            Ok(GatewayCommand::Pair { node_id, otp })
        }
        "status" => {
            let Some(flag) = args.next() else {
                return Err(IronclawError::new("missing --node-id"));
            };
            if flag != "--node-id" {
                return Err(IronclawError::new(format!(
                    "unknown gateway status argument: {flag}"
                )));
            }
            let Some(node_id) = args.next() else {
                return Err(IronclawError::new("missing value for --node-id"));
            };
            Ok(GatewayCommand::Status { node_id })
        }
        _ => Err(IronclawError::new(format!(
            "unknown gateway subcommand: {subcommand}"
        ))),
    }
}

pub fn default_runtime_dir() -> Result<PathBuf, IronclawError> {
    let var_run = PathBuf::from("/var/run/ironclaw");
    if ensure_dir(&var_run).is_ok() {
        return Ok(var_run);
    }

    let local = dirs::data_local_dir()
        .ok_or_else(|| IronclawError::new("local data dir missing"))?
        .join("ironclaw");
    ensure_dir(&local)?;
    Ok(local)
}

pub fn spawn_daemon_child(cli: &CliArgs) -> Result<(), IronclawError> {
    let exe = std::env::current_exe()
        .map_err(|err| IronclawError::new(format!("resolve current exe failed: {err}")))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("--daemon-child")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    if let Some(path) = &cli.pid_file {
        command.arg("--pid-file").arg(path);
    }
    if cli.telegram {
        command.arg("--telegram");
    }
    if cli.whatsapp {
        command.arg("--whatsapp");
    }

    let _child = command
        .spawn()
        .map_err(|err| IronclawError::new(format!("spawn daemon child failed: {err}")))?;
    Ok(())
}

pub fn stop_daemon(pid_file: &Path) -> Result<(), IronclawError> {
    let pid = read_pid(pid_file)?;
    #[cfg(unix)]
    {
        use nix::errno::Errno;
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        let pid = Pid::from_raw(pid);
        match kill(pid, Signal::SIGTERM) {
            Ok(_) => {}
            Err(Errno::ESRCH) => {
                let _ = std::fs::remove_file(pid_file);
                return Ok(());
            }
            Err(err) => {
                return Err(IronclawError::new(format!("send sigterm failed: {err}")));
            }
        }
        let _ = kill(pid, None);
        let _ = std::fs::remove_file(pid_file);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        return Err(IronclawError::new("daemon stop is only supported on unix"));
    }

    Ok(())
}

pub struct PidFileGuard {
    pid_file: PathBuf,
}

impl PidFileGuard {
    pub fn create(pid_file: PathBuf) -> Result<Self, IronclawError> {
        if let Some(parent) = pid_file.parent() {
            ensure_dir(parent)?;
        }
        let pid = std::process::id();
        std::fs::write(&pid_file, format!("{pid}\n"))
            .map_err(|err| IronclawError::new(format!("write pid file failed: {err}")))?;
        Ok(Self { pid_file })
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

fn ensure_dir(path: &Path) -> Result<(), IronclawError> {
    std::fs::create_dir_all(path)
        .map_err(|err| IronclawError::new(format!("create directory failed: {err}")))
}

fn read_pid(path: &Path) -> Result<i32, IronclawError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| IronclawError::new(format!("read pid file failed: {err}")))?;
    let trimmed = raw.trim();
    trimmed
        .parse::<i32>()
        .map_err(|err| IronclawError::new(format!("parse pid failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_args_defaults() {
        let cli = CliArgs::default();
        assert!(!cli.daemon);
        assert!(!cli.daemon_child);
        assert!(!cli.stop);
        assert!(!cli.telegram);
        assert!(!cli.whatsapp);
        assert!(cli.pid_file.is_none());
        assert!(cli.gateway_command.is_none());
    }
}
