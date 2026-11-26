//! OS-specific helpers: which process owns a port (Local Radar), this
//! computer's name (license device list) and the OS version (telemetry).
//!
//! Everything here is best effort. When a tool or API is missing or access is
//! denied, functions return `None` / an empty string instead of failing, so
//! callers just show less detail.
//!
//! | | macOS | Linux | Windows |
//! |---|---|---|---|
//! | listening PID | `lsof` | `ss`, then `lsof` | `netstat -ano` |
//! | process path | `ps -o comm=` | `/proc/<pid>/exe`, `cmdline`, `comm` | `QueryFullProcessImageNameW` |
//! | device name | `scutil --get ComputerName` | `/etc/machine-info`, hostname | `%COMPUTERNAME%` |
//! | OS version | `sw_vers -productVersion` | `/etc/os-release` | `RtlGetVersion` |

use std::ffi::OsStr;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

/// A child-process builder that never flashes a console window on Windows
/// (release builds are GUI-subsystem binaries) and kills the child if the
/// future awaiting it is dropped, e.g. by a timeout.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Runs a program and returns its stdout whatever the exit status, or `None`
/// if it couldn't be started or didn't finish within `secs`.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
async fn stdout_of<I, S>(program: impl AsRef<OsStr>, args: I, secs: u64) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = timeout(Duration::from_secs(secs), command(program).args(args).output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(unix)]
fn first_existing(paths: &[&'static str]) -> Option<&'static str> {
    paths.iter().copied().find(|p| std::path::Path::new(p).exists())
}

/// Executable path (or at least the program name) of whatever is listening
/// on TCP `port` on this machine.
pub async fn port_owner_path(port: u16) -> Option<String> {
    let pid = listening_pid(port).await?;
    let path = process_path(pid).await?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_string())
}

// ---------------------------------------------------------------- listening PID

#[cfg(unix)]
async fn lsof_pid(port: u16) -> Option<u32> {
    let lsof = first_existing(&["/usr/sbin/lsof", "/usr/bin/lsof", "/bin/lsof"])?;
    let args = ["-nP".to_string(), format!("-iTCP:{port}"), "-sTCP:LISTEN".into(), "-t".into()];
    let out = stdout_of(lsof, args, 2).await?;
    out.lines().next()?.trim().parse().ok()
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn listening_pid(port: u16) -> Option<u32> {
    lsof_pid(port).await
}

/// `ss` ships with every mainstream distro (iproute2); `lsof` often doesn't.
/// Either only reveals the PID for the current user's processes unless run as
/// root, so system services usually come back as `None`.
#[cfg(target_os = "linux")]
async fn listening_pid(port: u16) -> Option<u32> {
    if let Some(ss) = first_existing(&["/usr/bin/ss", "/bin/ss", "/usr/sbin/ss", "/sbin/ss"]) {
        let args = ["-ltnp".to_string(), "sport".into(), "=".into(), format!(":{port}")];
        if let Some(pid) = stdout_of(ss, args, 2).await.as_deref().and_then(parse_ss_pid) {
            return Some(pid);
        }
    }
    lsof_pid(port).await
}

#[cfg(windows)]
async fn listening_pid(port: u16) -> Option<u32> {
    let out = stdout_of(system32("NETSTAT.EXE"), ["-ano"], 3).await?;
    parse_netstat_pid(&out, port)
}

#[cfg(not(any(unix, windows)))]
async fn listening_pid(_port: u16) -> Option<u32> {
    None
}

#[cfg(windows)]
fn system32(exe: &str) -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    std::path::PathBuf::from(root).join("System32").join(exe)
}

/// `ss -ltnp` → `LISTEN 0 244 127.0.0.1:5432 0.0.0.0:* users:(("postgres",pid=1234,fd=6))`
#[cfg(any(target_os = "linux", test))]
fn parse_ss_pid(out: &str) -> Option<u32> {
    let rest = &out[out.find("pid=")? + 4..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `netstat -ano` rows look like `TCP  0.0.0.0:5432  0.0.0.0:0  LISTENING  4567`.
/// The state column is localized, so a listening socket is recognised by its
/// foreign address ending in `:0` and the PID is always the last column.
#[cfg(any(windows, test))]
fn parse_netstat_pid(out: &str, port: u16) -> Option<u32> {
    let local_suffix = format!(":{port}");
    out.lines().find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || !cols[0].eq_ignore_ascii_case("tcp") {
            return None;
        }
        let listening = cols[2].ends_with(":0") || cols[3].eq_ignore_ascii_case("listening");
        if !listening || !cols[1].ends_with(&local_suffix) {
            return None;
        }
        cols.last()?.parse().ok().filter(|pid| *pid != 0)
    })
}

// ---------------------------------------------------------------- process path

#[cfg(all(unix, not(target_os = "linux")))]
async fn process_path(pid: u32) -> Option<String> {
    stdout_of("/bin/ps", ["-o".to_string(), "comm=".into(), "-p".into(), pid.to_string()], 2).await
}

#[cfg(target_os = "linux")]
async fn process_path(pid: u32) -> Option<String> {
    let dir = std::path::PathBuf::from(format!("/proc/{pid}"));
    // `exe` needs ptrace access (own processes); `cmdline` and `comm` are world-readable.
    if let Ok(exe) = std::fs::read_link(dir.join("exe")) {
        return Some(exe.to_string_lossy().into_owned());
    }
    if let Ok(cmdline) = std::fs::read(dir.join("cmdline")) {
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        if !argv0.is_empty() {
            return Some(String::from_utf8_lossy(argv0).into_owned());
        }
    }
    std::fs::read_to_string(dir.join("comm")).ok()
}

#[cfg(windows)]
async fn process_path(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: plain Win32 calls. The handle is checked for null and closed
    // before returning, and `len` holds the real buffer size on input.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[cfg(not(any(unix, windows)))]
async fn process_path(_pid: u32) -> Option<String> {
    None
}

// ---------------------------------------------------------------- device name / OS version

/// Human-readable name of this computer, shown in the license's device list.
pub fn device_name() -> String {
    native_device_name()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "My computer".into())
}

/// OS version for telemetry, e.g. `15.1` (macOS), `10.0.22631` (Windows) or
/// `Ubuntu 24.04.1 LTS` (Linux). Empty if unknown.
pub fn os_version() -> String {
    native_os_version().unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn run_sync(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn native_device_name() -> Option<String> {
    run_sync("/usr/sbin/scutil", &["--get", "ComputerName"])
}

#[cfg(target_os = "macos")]
fn native_os_version() -> Option<String> {
    run_sync("/usr/bin/sw_vers", &["-productVersion"])
}

#[cfg(target_os = "linux")]
fn native_device_name() -> Option<String> {
    // systemd's "pretty" hostname (set in GNOME/KDE settings), else the kernel hostname.
    std::fs::read_to_string("/etc/machine-info")
        .ok()
        .and_then(|s| kv_value(&s, "PRETTY_HOSTNAME"))
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "linux")]
fn native_os_version() -> Option<String> {
    ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| parse_os_release(&s))
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .ok()
                .map(|s| format!("Linux {}", s.trim()))
        })
}

#[cfg(windows)]
fn native_device_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(windows)]
fn native_os_version() -> Option<String> {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    // GetVersionEx lies to un-manifested apps; RtlGetVersion always reports the real version.
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a correctly sized OSVERSIONINFOW that RtlGetVersion only writes into.
    let status = unsafe { RtlGetVersion(&mut info) };
    (status == 0).then(|| format!("{}.{}.{}", info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn native_device_name() -> Option<String> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn native_os_version() -> Option<String> {
    None
}

/// Value of `KEY=value` / `KEY="value"` in an env-style file (os-release, machine-info).
#[cfg(any(target_os = "linux", test))]
fn kv_value(content: &str, key: &str) -> Option<String> {
    content
        .lines()
        .find_map(|line| {
            let (k, v) = line.trim().split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        })
        .filter(|v| !v.is_empty())
}

#[cfg(any(target_os = "linux", test))]
fn parse_os_release(content: &str) -> Option<String> {
    kv_value(content, "PRETTY_NAME").or_else(|| {
        let name = kv_value(content, "NAME")?;
        Some(match kv_value(content, "VERSION_ID") {
            Some(v) => format!("{name} {v}"),
            None => name,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ss_output() {
        let out = "State  Recv-Q Send-Q Local Address:Port Peer Address:Port Process\n\
                   LISTEN 0      244        127.0.0.1:5432      0.0.0.0:*    users:((\"postgres\",pid=1234,fd=6))\n";
        assert_eq!(parse_ss_pid(out), Some(1234));
        // Another user's process: ss lists the socket but not the owner.
        assert_eq!(parse_ss_pid("LISTEN 0 244 127.0.0.1:5432 0.0.0.0:*\n"), None);
        assert_eq!(parse_ss_pid(""), None);
    }

    #[test]
    fn parses_netstat_output() {
        let out = "\r\nActive Connections\r\n\r\n  Proto  Local Address          Foreign Address        State           PID\r\n\
                   \x20 TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1100\r\n\
                   \x20 TCP    127.0.0.1:15432        0.0.0.0:0              LISTENING       77\r\n\
                   \x20 TCP    127.0.0.1:50000        127.0.0.1:5432         ESTABLISHED     9999\r\n\
                   \x20 TCP    0.0.0.0:5432           0.0.0.0:0              LISTENING       4567\r\n\
                   \x20 TCP    [::]:3306              [::]:0                 ABHÖREN         2222\r\n\
                   \x20 UDP    0.0.0.0:5432           *:*                                    3333\r\n";
        assert_eq!(parse_netstat_pid(out, 5432), Some(4567));
        assert_eq!(parse_netstat_pid(out, 15432), Some(77));
        assert_eq!(parse_netstat_pid(out, 3306), Some(2222)); // localized state, IPv6
        assert_eq!(parse_netstat_pid(out, 3307), None);
    }

    #[test]
    fn parses_env_files() {
        let os = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n";
        assert_eq!(parse_os_release(os).as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(parse_os_release("NAME=Arch Linux\n").as_deref(), Some("Arch Linux"));
        assert_eq!(parse_os_release("NAME=Fedora\nVERSION_ID=41\n").as_deref(), Some("Fedora 41"));
        assert_eq!(parse_os_release(""), None);
        assert_eq!(kv_value("PRETTY_HOSTNAME='Ada’s laptop'\n", "PRETTY_HOSTNAME").as_deref(), Some("Ada’s laptop"));
        assert_eq!(kv_value("PRETTY_HOSTNAME=\n", "PRETTY_HOSTNAME"), None);
    }

    #[test]
    fn device_name_is_never_empty() {
        assert!(!device_name().is_empty());
    }

    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn os_version_is_known() {
        assert!(!os_version().is_empty());
    }

    /// End-to-end on the host OS (CI runs it on macOS, Windows and Linux):
    /// listening PID lookup + process path must find this test binary.
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[tokio::test]
    async fn finds_owner_of_own_listening_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let path = port_owner_path(port).await.expect("no owner found for our own port");
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().to_lowercase();
        assert!(path.to_lowercase().contains(&stem), "{path:?} should name {stem:?}");
        drop(listener);
    }
}
