use crate::command::{CommandSpec, Runner};
use crate::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Network {
    pub bridge: String,
    pub runtime: PathBuf,
}

impl Network {
    pub fn new(bridge: String, runtime: PathBuf) -> Result<Self> {
        if bridge.is_empty()
            || bridge.len() > 15
            || !bridge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !runtime.is_absolute()
        {
            return Err(Error::Invalid("invalid bridge or runtime directory".into()));
        }
        Ok(Self { bridge, runtime })
    }

    pub fn marker(&self) -> PathBuf {
        self.runtime.join("network_up")
    }

    fn pidfile(&self) -> PathBuf {
        self.runtime.join("dnsmasq.pid")
    }

    pub fn firewall_commands(&self, add: bool) -> Vec<CommandSpec> {
        let action = if add { "-I" } else { "-D" };
        let append = if add { "-A" } else { "-D" };
        let mut rules = Vec::new();
        for protocol in ["udp", "tcp"] {
            for port in ["67", "53"] {
                rules.push(CommandSpec::new(
                    "iptables",
                    [
                        "-w",
                        action,
                        "INPUT",
                        "-i",
                        &self.bridge,
                        "-p",
                        protocol,
                        "--dport",
                        port,
                        "-j",
                        "ACCEPT",
                    ],
                ));
            }
        }
        for direction in ["-i", "-o"] {
            rules.push(CommandSpec::new(
                "iptables",
                [
                    "-w",
                    action,
                    "FORWARD",
                    direction,
                    &self.bridge,
                    "-j",
                    "ACCEPT",
                ],
            ));
        }
        rules.push(CommandSpec::new(
            "iptables",
            [
                "-w",
                "-t",
                "nat",
                append,
                "POSTROUTING",
                "-s",
                "192.168.240.0/24",
                "!",
                "-d",
                "192.168.240.0/24",
                "-j",
                "MASQUERADE",
            ],
        ));
        rules.push(CommandSpec::new(
            "iptables",
            [
                "-w",
                "-t",
                "mangle",
                append,
                "POSTROUTING",
                "-o",
                &self.bridge,
                "-p",
                "udp",
                "-m",
                "udp",
                "--dport",
                "68",
                "-j",
                "CHECKSUM",
                "--checksum-fill",
            ],
        ));
        rules
    }

    pub fn dnsmasq_command(&self) -> CommandSpec {
        CommandSpec::new(
            "dnsmasq",
            [
                "--conf-file=/dev/null".into(),
                "--strict-order".into(),
                "--bind-interfaces".into(),
                format!("--pid-file={}", self.pidfile().display()),
                "--user=nobody".into(),
                "--listen-address=192.168.240.1".into(),
                "--dhcp-range=192.168.240.2,192.168.240.254".into(),
                "--dhcp-lease-max=253".into(),
                "--dhcp-no-override".into(),
                "--except-interface=lo".into(),
                format!("--interface={}", self.bridge),
                format!(
                    "--dhcp-leasefile=/var/lib/misc/dnsmasq.{}.leases",
                    self.bridge
                ),
                "--dhcp-authoritative".into(),
            ],
        )
    }

    pub fn start_commands(&self) -> Vec<CommandSpec> {
        vec![
            CommandSpec::new("ip", ["link", "add", "dev", &self.bridge, "type", "bridge"]),
            CommandSpec::new("sysctl", ["-w", "net.ipv4.ip_forward=1"]),
            CommandSpec::new(
                "ip",
                ["addr", "add", "192.168.240.1/24", "dev", &self.bridge],
            ),
            CommandSpec::new(
                "ip",
                [
                    "link",
                    "set",
                    "dev",
                    &self.bridge,
                    "address",
                    "00:16:3e:00:00:01",
                ],
            ),
            CommandSpec::new("ip", ["link", "set", "dev", &self.bridge, "up"]),
        ]
    }

    pub fn start(&self, runner: &mut impl Runner) -> Result<()> {
        if self.marker().exists() {
            return Ok(());
        }
        if Path::new("/sys/class/net").join(&self.bridge).exists() {
            return Err(Error::Invalid(
                "bridge already exists without our marker".into(),
            ));
        }
        ensure_runtime(&self.runtime)?;
        let mut bridge_created = false;
        for (index, command) in self.start_commands().into_iter().enumerate() {
            if let Err(error) = runner.run(command) {
                if bridge_created {
                    let _ = runner.run(CommandSpec::new("ip", ["link", "delete", &self.bridge]));
                }
                return Err(error);
            }
            bridge_created |= index == 0;
        }
        let add_rules = self.firewall_commands(true);
        let remove_rules = self.firewall_commands(false);
        for (installed_rules, rule) in add_rules.into_iter().enumerate() {
            if let Err(error) = runner.run(rule) {
                self.rollback_network(runner, &remove_rules[..installed_rules]);
                return Err(error);
            }
        }
        if let Err(error) = runner.run(self.dnsmasq_command()) {
            self.rollback_network(runner, &remove_rules);
            return Err(error);
        }
        if let Err(error) = fs::write(self.marker(), b"up\n") {
            if let Ok(Some(pid)) = verified_dnsmasq_pid(&self.pidfile()) {
                let _ = runner.run(CommandSpec::new("kill", [pid.to_string()]));
            }
            self.rollback_network(runner, &remove_rules);
            return Err(error.into());
        }
        Ok(())
    }

    fn rollback_network(&self, runner: &mut impl Runner, rules: &[CommandSpec]) {
        for rule in rules.iter().rev() {
            let _ = runner.run(rule.clone());
        }
        let _ = runner.run(CommandSpec::new("ip", ["link", "delete", &self.bridge]));
    }

    pub fn stop(&self, runner: &mut impl Runner) -> Result<()> {
        if !self.marker().exists() {
            return Ok(());
        }
        if let Some(pid) = verified_dnsmasq_pid(&self.pidfile())? {
            runner.run(CommandSpec::new("kill", [pid.to_string()]))?;
        }
        for rule in self.firewall_commands(false).into_iter().rev() {
            runner.run(rule)?;
        }
        runner.run(CommandSpec::new(
            "ip",
            ["addr", "flush", "dev", &self.bridge],
        ))?;
        runner.run(CommandSpec::new(
            "ip",
            ["link", "set", "dev", &self.bridge, "down"],
        ))?;
        runner.run(CommandSpec::new("ip", ["link", "delete", &self.bridge]))?;
        fs::remove_file(self.marker())?;
        let _ = fs::remove_file(self.pidfile());
        Ok(())
    }
}

fn ensure_runtime(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(Error::Invalid("runtime directory is a symlink".into()));
    }
    fs::create_dir_all(path)?;
    if !path.is_dir() {
        return Err(Error::Invalid("runtime path is not a directory".into()));
    }
    Ok(())
}

fn verified_dnsmasq_pid(pidfile: &Path) -> Result<Option<u32>> {
    let contents = match fs::read_to_string(pidfile) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let pid: u32 = contents
        .trim()
        .parse()
        .map_err(|_| Error::Invalid("invalid dnsmasq PID".into()))?;
    if pid <= 1 {
        return Err(Error::Invalid("unsafe dnsmasq PID".into()));
    }
    let cmdline = match fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => cmdline,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let expected = format!("--pid-file={}", pidfile.display());
    let args: Vec<_> = cmdline.split(|byte| *byte == 0).collect();
    let is_dnsmasq = args.first().is_some_and(|arg| {
        Path::new(&String::from_utf8_lossy(arg).to_string())
            .file_name()
            .is_some_and(|name| name == "dnsmasq")
    });
    if !is_dnsmasq || !args.contains(&expected.as_bytes()) {
        return Err(Error::Invalid(
            "PID file does not identify our dnsmasq".into(),
        ));
    }
    Ok(Some(pid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mounts::mountpoints_beneath;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeRunner {
        calls: Vec<CommandSpec>,
        fail_at: usize,
    }

    impl Runner for FakeRunner {
        fn run(&mut self, command: CommandSpec) -> Result<String> {
            self.calls.push(command);
            if self.calls.len() == self.fail_at {
                return Err(Error::Command("injected failure".into()));
            }
            Ok(String::new())
        }
    }

    #[test]
    fn direct_network_commands_match_device_iptables_path() -> Result<()> {
        let network = Network::new("deckard0".into(), "/run/deckard-container".into())?;
        let commands = network.start_commands();
        assert_eq!(commands[0].program, "ip");
        assert_eq!(commands.len(), 5);
        let rules = network.firewall_commands(true);
        assert_eq!(rules.len(), 8);
        assert!(rules.iter().all(|rule| rule.program == "iptables"));
        assert!(
            rules
                .iter()
                .all(|rule| rule.args.contains(&"deckard0".into())
                    || rule.args.contains(&"192.168.240.0/24".into()))
        );
        assert!(
            rules
                .iter()
                .all(|rule| !rule.args.contains(&"waydroid0".into()))
        );
        assert!(
            network
                .dnsmasq_command()
                .args
                .contains(&"--conf-file=/dev/null".into())
        );
        assert!(Network::new("bad;name".into(), "/run/deckard".into()).is_err());
        Ok(())
    }

    #[test]
    fn mount_table_parser_does_not_match_network_peer_prefix() {
        assert!(
            mountpoints_beneath(
                Path::new("/run/deckard"),
                "1 0 0:1 / /run/deckard-other rw - tmpfs x y"
            )
            .is_empty()
        );
    }

    #[test]
    fn failed_firewall_setup_rolls_back_only_our_rules_and_bridge() -> Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::Invalid(error.to_string()))?
            .as_nanos();
        let runtime = std::env::temp_dir().join(format!("deckard-network-{nonce}"));
        let network = Network::new("deckard0".into(), runtime.clone())?;
        let mut runner = FakeRunner {
            calls: Vec::new(),
            fail_at: 7,
        };
        assert!(network.start(&mut runner).is_err());
        assert_eq!(runner.calls[6].program, "iptables");
        assert!(
            runner
                .calls
                .iter()
                .skip(7)
                .any(|call| call.program == "iptables" && call.args.contains(&"-D".into()))
        );
        assert_eq!(
            runner.calls.last().map(|call| call.args.as_slice()),
            Some(
                CommandSpec::new("ip", ["link", "delete", "deckard0"])
                    .args
                    .as_slice()
            )
        );
        assert!(!network.marker().exists());
        fs::remove_dir(runtime)?;
        Ok(())
    }
}
