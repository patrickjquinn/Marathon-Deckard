//! The container's bridge, NAT rules and dnsmasq (upstream
//! `data/scripts/waydroid-net.sh`).

use std::fs;
use std::path::{Path, PathBuf};

use deckard_host_tools::files;

use crate::Result;
use crate::command::{Command, Runner};

const SUBNET: &str = "192.168.240.0/24";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Add,
    Remove,
}

#[derive(Clone, Debug)]
pub struct Network {
    pub bridge: String,
    pub runtime: PathBuf,
}

impl Network {
    /// # Errors
    /// The bridge is not a plain interface name or the runtime path is relative.
    pub fn new(bridge: String, runtime: PathBuf) -> Result<Self> {
        let plain = bridge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if bridge.is_empty() || bridge.len() > 15 || !plain || !runtime.is_absolute() {
            return Err("invalid bridge or runtime directory".into());
        }
        Ok(Self { bridge, runtime })
    }

    #[must_use]
    pub fn marker(&self) -> PathBuf {
        self.runtime.join("network_up")
    }

    fn pidfile(&self) -> PathBuf {
        self.runtime.join("dnsmasq.pid")
    }

    fn firewall(&self, change: Change) -> Vec<Command> {
        let (insert, append) = match change {
            Change::Add => ("-I", "-A"),
            Change::Remove => ("-D", "-D"),
        };
        let bridge = &self.bridge;
        let mut rules = Vec::new();
        for protocol in ["udp", "tcp"] {
            for port in ["67", "53"] {
                rules.push(format!(
                    "-w {insert} INPUT -i {bridge} -p {protocol} --dport {port} -j ACCEPT"
                ));
            }
        }
        for direction in ["-i", "-o"] {
            rules.push(format!(
                "-w {insert} FORWARD {direction} {bridge} -j ACCEPT"
            ));
        }
        rules.push(format!(
            "-w -t nat {append} POSTROUTING -s {SUBNET} ! -d {SUBNET} -j MASQUERADE"
        ));
        rules.push(format!(
            "-w -t mangle {append} POSTROUTING -o {bridge} -p udp -m udp --dport 68 -j CHECKSUM --checksum-fill"
        ));
        let mut commands = Vec::with_capacity(rules.len());
        for rule in rules {
            commands.push(Command::line(&format!("iptables {rule}")));
        }
        commands
    }

    fn dnsmasq(&self) -> Command {
        let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
        let bridge = &self.bridge;
        Command::new([
            "dnsmasq".into(),
            "--conf-file=/dev/null".into(),
            "--strict-order".into(),
            "--bind-interfaces".into(),
            format!("--pid-file={}", self.pidfile().display()),
            format!("--user={}", dnsmasq_account(&passwd)),
            "--listen-address=192.168.240.1".into(),
            "--dhcp-range=192.168.240.2,192.168.240.254".into(),
            "--dhcp-lease-max=253".into(),
            "--dhcp-no-override".into(),
            "--except-interface=lo".into(),
            format!("--interface={bridge}"),
            format!("--dhcp-leasefile=/var/lib/misc/dnsmasq.{bridge}.leases"),
            "--dhcp-authoritative".into(),
        ])
    }

    fn delete_bridge(&self) -> Command {
        Command::line(&format!("ip link delete {}", self.bridge))
    }

    fn rollback(&self, runner: &mut impl Runner, rules: &[Command]) {
        for rule in rules.iter().rev() {
            let _ = runner.run(rule.clone());
        }
        let _ = runner.run(self.delete_bridge());
    }

    /// Brings the network up unless our marker says it is; undoes what it
    /// did on failure.
    ///
    /// # Errors
    /// A foreign bridge exists, the runtime directory is unsafe or a command fails.
    pub fn start(&self, runner: &mut impl Runner) -> Result<()> {
        if self.marker().exists() {
            return Ok(());
        }
        if Path::new("/sys/class/net").join(&self.bridge).exists() {
            return Err("bridge already exists without our marker".into());
        }
        if fs::symlink_metadata(&self.runtime).is_ok_and(|metadata| metadata.is_symlink()) {
            return Err("runtime directory is a symlink".into());
        }
        fs::create_dir_all(&self.runtime)?;
        let bridge = &self.bridge;
        let setup = [
            format!("ip link add dev {bridge} type bridge"),
            "sysctl -w net.ipv4.ip_forward=1".into(),
            format!("ip addr add 192.168.240.1/24 dev {bridge}"),
            format!("ip link set dev {bridge} address 00:16:3e:00:00:01"),
            format!("ip link set dev {bridge} up"),
        ];
        for (index, line) in setup.iter().enumerate() {
            if let Err(error) = runner.run(Command::line(line)) {
                if index > 0 {
                    let _ = runner.run(self.delete_bridge());
                }
                return Err(error);
            }
        }
        let remove = self.firewall(Change::Remove);
        let add = self.firewall(Change::Add);
        for (installed, rule) in add.into_iter().enumerate() {
            if let Err(error) = runner.run(rule) {
                self.rollback(runner, &remove[..installed]);
                return Err(error);
            }
        }
        if let Err(error) = runner.run(self.dnsmasq()) {
            self.rollback(runner, &remove);
            return Err(error);
        }
        if let Err(error) = fs::write(self.marker(), b"up\n") {
            if let Ok(Some(pid)) = verified_dnsmasq_pid(&self.pidfile()) {
                let _ = runner.run(Command::new(["kill".into(), pid.to_string()]));
            }
            self.rollback(runner, &remove);
            return Err(error.into());
        }
        Ok(())
    }

    /// # Errors
    /// The dnsmasq PID file is foreign or a command fails.
    pub fn stop(&self, runner: &mut impl Runner) -> Result<()> {
        if !self.marker().exists() {
            return Ok(());
        }
        if let Some(pid) = verified_dnsmasq_pid(&self.pidfile())? {
            runner.run(Command::new(["kill".into(), pid.to_string()]))?;
        }
        let rules = self.firewall(Change::Remove);
        for rule in rules.into_iter().rev() {
            runner.run(rule)?;
        }
        let bridge = &self.bridge;
        runner.run(Command::line(&format!("ip addr flush dev {bridge}")))?;
        runner.run(Command::line(&format!("ip link set dev {bridge} down")))?;
        runner.run(self.delete_bridge())?;
        fs::remove_file(self.marker())?;
        let _ = fs::remove_file(self.pidfile());
        Ok(())
    }
}

fn dnsmasq_account(passwd: &str) -> &'static str {
    let listed = |account: &str| {
        passwd.lines().any(|line| {
            line.strip_prefix(account)
                .is_some_and(|rest| rest.starts_with(':'))
        })
    };
    ["lxc-dnsmasq", "dnsmasq"]
        .into_iter()
        .find(|account| listed(account))
        .unwrap_or("nobody")
}

fn verified_dnsmasq_pid(pidfile: &Path) -> Result<Option<u32>> {
    let Some(contents) = files::found(fs::read_to_string(pidfile))? else {
        return Ok(None);
    };
    let pid: u32 = contents.trim().parse().map_err(|_| "invalid dnsmasq PID")?;
    if pid <= 1 {
        return Err("unsafe dnsmasq PID".into());
    }
    let Some(cmdline) = files::found(fs::read(format!("/proc/{pid}/cmdline")))? else {
        return Ok(None);
    };
    let expected = format!("--pid-file={}", pidfile.display());
    let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
    let program = args
        .first()
        .map(|arg| String::from_utf8_lossy(arg).into_owned());
    let is_dnsmasq = program.is_some_and(|program| {
        Path::new(&program)
            .file_name()
            .is_some_and(|name| name == "dnsmasq")
    });
    if !is_dnsmasq || !args.contains(&expected.as_bytes()) {
        return Err("PID file does not identify our dnsmasq".into());
    }
    Ok(Some(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_dnsmasq_account_takes_precedence() {
        let passwd = "nobody:x:65534:65534::/:/sbin/nologin\ndnsmasq:x:107:110::/:/sbin/nologin\n";
        assert_eq!(dnsmasq_account(passwd), "dnsmasq");
        assert_eq!(dnsmasq_account("dnsmasqx:x:1:1::/:/\n"), "nobody");
        assert!(Network::new("bad;name".into(), "/run/deckard".into()).is_err());
    }

    struct FailingRunner(Vec<String>);

    impl Runner for FailingRunner {
        fn run(&mut self, command: Command) -> Result<String> {
            self.0.push(command.args.join(" "));
            if self.0.len() == 7 {
                return Err("injected failure".into());
            }
            Ok(String::new())
        }
    }

    #[test]
    fn failed_firewall_setup_rolls_back_only_our_rules_and_bridge() -> Result<()> {
        let runtime = tempfile::tempdir()?;
        let network = Network::new("deckard0".into(), runtime.path().into())?;
        let mut runner = FailingRunner(Vec::new());
        assert!(network.start(&mut runner).is_err());
        assert_eq!(
            runner.0[5..],
            [
                "iptables -w -I INPUT -i deckard0 -p udp --dport 67 -j ACCEPT",
                "iptables -w -I INPUT -i deckard0 -p udp --dport 53 -j ACCEPT",
                "iptables -w -D INPUT -i deckard0 -p udp --dport 67 -j ACCEPT",
                "ip link delete deckard0",
            ]
        );
        assert!(!network.marker().exists());
        Ok(())
    }
}
