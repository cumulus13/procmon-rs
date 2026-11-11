// procmon - Process Monitor & Auto-Restart (Rust port)
// Config format compatible with the Python version (procmon.ini)

use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use clap::Parser;
use ini::Ini;
use sysinfo::{Pid, Process, ProcessesToUpdate, System};

// ---------- ANSI helpers ----------
const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

const TEMPLATE: &str = r#"; ========================================
; Process Monitor Configuration File
; ========================================
;
; Each section represents a process to monitor
;
; Section name = Display name in monitor
; exe_path     = Full path to executable (required)
; args         = Command line arguments (optional)
; working_dir  = Working directory (optional)
; enabled      = yes/no to enable/disable monitoring
;
; Notifications:
; - Growl: GNTP over TCP (no password support in this build)
; - Desktop: native notifications (feature "desktop-notify")

[Settings]
; Check interval in seconds
check_interval = 5

[Notifications]
use_growl = yes
use_plyer = yes
growl_host = localhost
growl_port = 23053
growl_password =
growl_icon =

[Growl]
exe_path = C:\Program Files (x86)\Growl for Windows\Growl.exe
args =
working_dir = C:\Program Files (x86)\Growl for Windows
enabled = yes

[Steam]
exe_path = C:\Program Files (x86)\Steam\steam.exe
args = -silent
working_dir = C:\Program Files (x86)\Steam
enabled = no

[CustomApp]
exe_path = C:\Path\To\YourApp.exe
args = -flag1 -flag2 value
working_dir = C:\Path\To
enabled = no
"#;

fn parse_bool(s: Option<&str>, default: bool) -> bool {
    match s.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "yes" | "true" | "on") => true,
        Some(v) if matches!(v.as_str(), "0" | "no" | "false" | "off") => false,
        _ => default,
    }
}

// ---------- Notifications ----------
struct Notifier {
    growl: Option<(String, u16)>,
    desktop: bool,
}

impl Notifier {
    fn from_ini(ini: &Ini) -> Option<Self> {
        let sec = ini.section(Some("Notifications"))?;
        let use_growl = parse_bool(sec.get("use_growl"), false);
        let use_desktop = parse_bool(sec.get("use_plyer"), false);
        let host = sec
            .get("growl_host")
            .unwrap_or("localhost")
            .trim()
            .to_string();
        let port: u16 = sec
            .get("growl_port")
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(23053);

        if use_growl && !sec.get("growl_password").unwrap_or("").trim().is_empty() {
            println!("{YELLOW}⚠  growl_password is ignored (not supported yet){RESET}");
        }

        let mut n = Notifier {
            growl: None,
            desktop: false,
        };

        if use_growl {
            let payload = "GNTP/1.0 REGISTER NONE\r\n\
                Application-Name: Process Monitor\r\n\
                Notifications-Count: 1\r\n\r\n\
                Notification-Name: Process Event\r\n\
                Notification-Display-Name: Process Event\r\n\
                Notification-Enabled: True\r\n\r\n"
                .to_string();
            n.growl = Some((host.clone(), port));
            gntp_send(host, port, payload);
            println!("{GREEN}✅ Growl notifications enabled{RESET}");
        }
        if use_desktop {
            #[cfg(feature = "desktop-notify")]
            {
                n.desktop = true;
                println!("{GREEN}✅ Desktop notifications enabled{RESET}");
            }
            #[cfg(not(feature = "desktop-notify"))]
            println!("{YELLOW}⚠  Desktop notifications were not compiled in{RESET}");
        }
        Some(n)
    }

    fn send(&self, title: &str, msg: &str, priority: i8, sticky: bool) {
        if let Some((host, port)) = &self.growl {
            let payload = format!(
                "GNTP/1.0 NOTIFY NONE\r\n\
                 Application-Name: Process Monitor\r\n\
                 Notification-Name: Process Event\r\n\
                 Notification-Title: {}\r\n\
                 Notification-Text: {}\r\n\
                 Notification-Priority: {}\r\n\
                 Notification-Sticky: {}\r\n\r\n",
                title.replace(['\r', '\n'], " "),
                msg.replace(['\r', '\n'], " "),
                priority.clamp(-2, 2),
                if sticky { "True" } else { "False" }
            );
            gntp_send(host.clone(), *port, payload);
        }
        #[cfg(feature = "desktop-notify")]
        if self.desktop {
            let (t, m) = (title.to_string(), msg.to_string());
            std::thread::spawn(move || {
                let _ = notify_rust::Notification::new()
                    .summary(&t)
                    .body(&m)
                    .appname("Process Monitor")
                    .timeout(10_000)
                    .show();
            });
        }
    }

    fn stopped(&self, name: &str) {
        self.send(
            "⚠️ Process Stopped",
            &format!("{name} has stopped running"),
            1,
            false,
        );
    }
    fn started(&self, name: &str, pid: u32) {
        self.send(
            "🚀 Process Started",
            &format!("{name} started (PID: {pid})"),
            0,
            false,
        );
    }
    fn error(&self, name: &str, err: &str) {
        self.send("❌ Process Error", &format!("{name}: {err}"), 2, true);
    }
    fn monitor_started(&self, n: usize) {
        self.send(
            "🔍 Process Monitor Started",
            &format!("Monitoring {n} process(es)"),
            0,
            false,
        );
    }
}

/// Fire-and-forget GNTP message; runs in its own short-lived thread so it never blocks the loop.
fn gntp_send(host: String, port: u16, payload: String) {
    std::thread::spawn(move || {
        let Some(addr) = format!("{host}:{port}")
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
        else {
            return;
        };
        if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = s.write_all(payload.as_bytes());
            let mut buf = [0u8; 256];
            let _ = s.read(&mut buf);
        }
    });
}

// ---------- Monitored process ----------
#[derive(Clone, Copy, PartialEq)]
enum Status {
    Running,
    Stopped,
    Error,
}

struct Monitored {
    name: String,
    exe_path: String,
    exe_name: String, // lowercase basename
    args: String,
    working_dir: Option<String>,
    pid: Option<u32>,
    status: Status,
    restarts: u32,
    last_seen: Option<DateTime<Local>>,
    child: Option<Child>, // kept so we can try_wait() and avoid zombies
}

fn matches_proc(p: &Process, exe_path: &str, exe_name: &str) -> bool {
    let name = p.name().to_string_lossy().to_lowercase();
    if name == exe_name {
        return true;
    }
    // Linux truncates comm names to 15 chars
    if name.len() >= 15 && exe_name.starts_with(&name) {
        return true;
    }
    if let Some(exe) = p.exe() {
        if exe.to_string_lossy().eq_ignore_ascii_case(exe_path) {
            return true;
        }
        if let Some(base) = exe.file_name() {
            if base.to_string_lossy().to_lowercase() == exe_name {
                return true;
            }
        }
    }
    false
}

fn find_pid(sys: &System, exe_path: &str, exe_name: &str) -> Option<u32> {
    sys.processes()
        .iter()
        .find(|(_, p)| matches_proc(p, exe_path, exe_name))
        .map(|(pid, _)| pid.as_u32())
}

fn start_process(m: &mut Monitored, notifier: Option<&Notifier>) -> bool {
    if !Path::new(&m.exe_path).exists() {
        let msg = format!("Executable not found: {}", m.exe_path);
        println!("{RED}❌ {msg}{RESET}");
        m.status = Status::Error;
        if let Some(n) = notifier {
            n.error(&m.name, &msg);
        }
        return false;
    }

    let mut cmd = Command::new(&m.exe_path);
    if !m.args.trim().is_empty() {
        match shell_words::split(&m.args) {
            Ok(v) => {
                cmd.args(v);
            }
            Err(_) => {
                cmd.args(m.args.split_whitespace());
            }
        }
    }
    if let Some(dir) = m.working_dir.as_deref().filter(|d| !d.is_empty()) {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            m.pid = Some(pid);
            m.status = Status::Running;
            m.restarts += 1;
            m.last_seen = Some(Local::now());
            m.child = Some(child);
            println!("{GREEN}🚀 Started '{}' (PID: {pid}){RESET}", m.name);
            if let Some(n) = notifier {
                n.started(&m.name, pid);
            }
            true
        }
        Err(e) => {
            println!("{RED}❌ Failed to start '{}': {e}{RESET}", m.name);
            m.status = Status::Error;
            if let Some(n) = notifier {
                n.error(&m.name, &e.to_string());
            }
            false
        }
    }
}

// ---------- Monitor ----------
struct Monitor {
    procs: Vec<Monitored>,
    notifier: Option<Notifier>,
    sys: System, // reused for every cycle -> no allocation churn
    check_interval: u64,
    total_restarts: u32,
    start: Instant,
    last_check: Option<DateTime<Local>>,
    config_path: String,
}

impl Monitor {
    fn load(path: &str) -> Result<Self, String> {
        let ini = Ini::load_from_file(path).map_err(|e| format!("Error loading config: {e}"))?;

        let check_interval = ini
            .section(Some("Settings"))
            .and_then(|s| s.get("check_interval"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(5u64)
            .max(1);

        let notifier = if ini.section(Some("Notifications")).is_some() {
            Notifier::from_ini(&ini)
        } else {
            println!("{DIM}ℹ️  No [Notifications] section found in config{RESET}");
            None
        };

        let mut procs = Vec::new();
        for (sec, props) in ini.iter() {
            let Some(name) = sec else { continue };
            if name == "Settings" || name == "Notifications" {
                continue;
            }
            let exe_path = props.get("exe_path").unwrap_or("").trim().to_string();
            if exe_path.is_empty() {
                println!("{YELLOW}⚠️  Skipping '{name}': no exe_path defined{RESET}");
                continue;
            }
            if !parse_bool(props.get("enabled"), true) {
                println!("{DIM}⏭️  Skipping '{name}': disabled in config{RESET}");
                continue;
            }
            let working_dir = props
                .get("working_dir")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    Path::new(&exe_path)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                        .filter(|s| !s.is_empty())
                });
            let exe_name = Path::new(&exe_path)
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_else(|| exe_path.to_lowercase());

            procs.push(Monitored {
                name: name.to_string(),
                exe_name,
                exe_path,
                args: props.get("args").unwrap_or("").trim().to_string(),
                working_dir,
                pid: None,
                status: Status::Stopped,
                restarts: 0,
                last_seen: None,
                child: None,
            });
        }

        println!(
            "{GREEN}✅ Loaded {} process(es) from {path}{RESET}",
            procs.len()
        );
        for p in &procs {
            println!(
                "  {CYAN}{:<20}{RESET} {GREEN}{}{RESET} {YELLOW}{}{RESET}",
                p.name,
                p.exe_path,
                if p.args.is_empty() { "-" } else { &p.args }
            );
        }

        Ok(Monitor {
            procs,
            notifier,
            sys: System::new(),
            check_interval,
            total_restarts: 0,
            start: Instant::now(),
            last_check: None,
            config_path: path.to_string(),
        })
    }

    fn check_and_restart(&mut self) {
        self.last_check = Some(Local::now());
        self.sys.refresh_processes(ProcessesToUpdate::All, true);

        for m in self.procs.iter_mut() {
            // Reap our own child if it exited (prevents zombies on Unix)
            if let Some(c) = m.child.as_mut() {
                if let Ok(Some(_)) = c.try_wait() {
                    m.child = None;
                }
            }

            let mut running = m
                .pid
                .and_then(|p| self.sys.process(Pid::from_u32(p)))
                .map(|p| matches_proc(p, &m.exe_path, &m.exe_name))
                .unwrap_or(false);

            if !running {
                if let Some(pid) = find_pid(&self.sys, &m.exe_path, &m.exe_name) {
                    m.pid = Some(pid);
                    running = true;
                }
            }

            if running {
                m.status = Status::Running;
                m.last_seen = Some(Local::now());
            } else {
                if m.status == Status::Running {
                    println!("{YELLOW}⚠️  Process '{}' has stopped{RESET}", m.name);
                    if let Some(n) = &self.notifier {
                        n.stopped(&m.name);
                    }
                }
                m.status = Status::Stopped;
                println!("{CYAN}🔄 Restarting '{}'...{RESET}", m.name);
                std::thread::sleep(Duration::from_millis(500));
                if start_process(m, self.notifier.as_ref()) {
                    self.total_restarts += 1;
                }
            }
        }
    }

    fn render(&self) -> String {
        let mut o = String::with_capacity(2048);
        let up = self.start.elapsed().as_secs();
        let mem_mb = sysinfo::get_current_pid()
            .ok()
            .and_then(|p| self.sys.process(p))
            .map(|p| p.memory() as f64 / 1_048_576.0)
            .unwrap_or(0.0);

        let notif = match &self.notifier {
            Some(n) => {
                let mut v = Vec::new();
                if n.growl.is_some() {
                    v.push("Growl");
                }
                if n.desktop {
                    v.push("Desktop");
                }
                if v.is_empty() {
                    "Disabled".to_string()
                } else {
                    v.join(", ")
                }
            }
            None => "Disabled".to_string(),
        };

        let _ = writeln!(o, "{BOLD}{CYAN}📈 Statistics{RESET}");
        let _ = writeln!(
            o,
            "  Uptime        : {YELLOW}{:02}h {:02}m {:02}s{RESET}",
            up / 3600,
            (up % 3600) / 60,
            up % 60
        );
        let _ = writeln!(o, "  Processes     : {YELLOW}{}{RESET}", self.procs.len());
        let _ = writeln!(
            o,
            "  Total Restarts: {YELLOW}{}{RESET}",
            self.total_restarts
        );
        let _ = writeln!(
            o,
            "  Last Check    : {YELLOW}{}{RESET}",
            self.last_check
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "Never".into())
        );
        let _ = writeln!(o, "  Memory        : {YELLOW}{mem_mb:.1} MB{RESET}");
        let _ = writeln!(o, "  Config        : {YELLOW}{}{RESET}", self.config_path);
        let _ = writeln!(o, "  Notifications : {GREEN}{notif}{RESET}");
        let _ = writeln!(o, "  {DIM}Press Ctrl+C to stop{RESET}\n");

        let _ = writeln!(o, "{BOLD}📊 Process Monitor Status{RESET}");
        let _ = writeln!(
            o,
            "  {BOLD}{:<20} {:<12} {:>8} {:>9} {:>9}{RESET}",
            "Name", "Status", "PID", "Restarts", "Last Seen"
        );
        for m in &self.procs {
            let (color, label) = match m.status {
                Status::Running => (GREEN, "● Running"),
                Status::Stopped => (RED, "○ Stopped"),
                Status::Error => (RED, "✖ Error"),
            };
            let name: String = if m.name.chars().count() > 18 {
                m.name.chars().take(18).collect::<String>() + "..."
            } else {
                m.name.clone()
            };
            let _ = writeln!(
                o,
                "  {CYAN}{:<20}{RESET} {color}{:<12}{RESET} {YELLOW}{:>8}{RESET} {:>9} {GREEN}{:>9}{RESET}",
                name,
                label,
                m.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                m.restarts,
                m.last_seen.map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_else(|| "-".into()),
            );
        }
        o
    }

    fn run(&mut self, interval: u64, running: Arc<AtomicBool>) {
        println!(
            "\n{BOLD}🔍 Process Monitor Started{RESET} — {} process(es), interval {interval}s",
            self.procs.len()
        );
        if let Some(n) = &self.notifier {
            n.monitor_started(self.procs.len());
        }
        println!("{CYAN}Performing initial check...{RESET}");
        self.check_and_restart();
        std::thread::sleep(Duration::from_secs(1));

        let mut out = std::io::stdout();
        let _ = crossterm::execute!(
            out,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        );

        while running.load(Ordering::SeqCst) {
            self.check_and_restart();
            let frame = self.render();
            let _ = crossterm::execute!(
                out,
                crossterm::cursor::MoveTo(0, 0),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
            );
            let _ = out.write_all(frame.as_bytes());
            let _ = out.flush();

            // sleep in small steps so Ctrl+C responds quickly
            let deadline = Instant::now() + Duration::from_secs(interval);
            while running.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        println!("\n{YELLOW}⏹️  Monitoring stopped by user{RESET}");
        println!("{CYAN}👋 Process Monitor stopped{RESET}");
    }
}

// ---------- CLI ----------
#[derive(Parser)]
#[command(
    name = "procmon",
    about = "🔍 Process Monitor & Auto-Restart with Notifications"
)]
struct Cli {
    /// INI config file path
    #[arg(short, long, default_value = "procmon.ini")]
    config: String,

    /// Check interval in seconds (overrides config)
    #[arg(short, long)]
    interval: Option<u64>,

    /// List configured processes and exit
    #[arg(short, long)]
    list: bool,

    /// Create template config file
    #[arg(long, value_name = "FILENAME")]
    create_config: Option<String>,
}

fn main() {
    // Enable ANSI/VT processing on Windows consoles (module only exists on Windows)
    #[cfg(windows)]
    let _ = crossterm::ansi_support::supports_ansi();
    let cli = Cli::parse();

    if let Some(file) = cli.create_config {
        match std::fs::write(&file, TEMPLATE) {
            Ok(_) => {
                println!("{GREEN}✅ Template config saved to: {file}{RESET}");
                println!("\n📝 Next steps:\n   1. Edit the config file and set your process paths\n   2. Run: procmon -c {file}");
            }
            Err(e) => println!("{RED}❌ Error saving template: {e}{RESET}"),
        }
        return;
    }

    if !Path::new(&cli.config).exists() {
        println!("{RED}❌ Config file not found: {}{RESET}", cli.config);
        println!(
            "\n💡 Create a template config with:\n   {CYAN}procmon --create-config {}{RESET}",
            cli.config
        );
        std::process::exit(1);
    }

    let mut mon = match Monitor::load(&cli.config) {
        Ok(m) => m,
        Err(e) => {
            println!("{RED}❌ {e}{RESET}");
            std::process::exit(1);
        }
    };

    if mon.procs.is_empty() {
        println!("{RED}❌ No processes configured in config file{RESET}");
        std::process::exit(1);
    }

    if cli.list {
        mon.sys.refresh_processes(ProcessesToUpdate::All, true);
        for m in mon.procs.iter_mut() {
            if let Some(pid) = find_pid(&mon.sys, &m.exe_path, &m.exe_name) {
                m.pid = Some(pid);
                m.status = Status::Running;
                m.last_seen = Some(Local::now());
            }
        }
        print!("{}", mon.render());
        return;
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        let _ = ctrlc::set_handler(move || r.store(false, Ordering::SeqCst));
    }

    let interval = cli.interval.unwrap_or(mon.check_interval).max(1);
    mon.run(interval, running);
}
