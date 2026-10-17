#[cfg(windows)]
use crate::platform::ps_quote;
#[cfg(target_os = "macos")]
use crate::platform::sh_quote;
use crate::{
    platform::{self, data_dir},
    routes::atomic_json,
    runtime::{self, Status},
};
#[cfg(not(any(windows, target_os = "macos")))]
use anyhow::bail;
use anyhow::{ensure, Result};
use std::{
    net::{Ipv4Addr, TcpListener},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(any(windows, target_os = "macos"))]
const LABEL: &str = "com.aibalabala.tailscalesplit.v2";
const LOCK_PORT: u16 = 53539;

pub fn supervisor() -> Result<()> {
    let _lock = TcpListener::bind((Ipv4Addr::LOCALHOST, LOCK_PORT))?;
    let dir = data_dir();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(dir.join("worker.log"))?;
    let child = Command::new(std::env::current_exe()?)
        .arg("worker")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn();
    let result = match child {
        Ok(mut c) => c.wait().map_err(anyhow::Error::from),
        Err(e) => Err(e.into()),
    };
    // Parent survives a worker panic, SIGKILL, or abnormal return, and restores owned state.
    if let Err(e) = runtime::recover() {
        runtime::write_error(&format!("恢复未完成: {e:#}；请使用停止 / 恢复重试"));
        return Err(e);
    }
    let stopped = dir.join("stop").exists();
    if stopped || result.as_ref().is_ok_and(|s| s.success()) {
        Status::new("stopped", "已撤销本程序 DNS 规则和路由；出口节点保持原设置").save()?;
    } else {
        let log = std::fs::read_to_string(dir.join("worker.log")).unwrap_or_default();
        runtime::write_error(&format!("启用 / 运行失败，已恢复本程序的改动：{log}"));
    }
    // A worker failure is terminal for this activation, not an infinite restart loop.
    Ok(())
}

fn running() -> bool {
    TcpListener::bind((Ipv4Addr::LOCALHOST, LOCK_PORT)).is_err()
}

fn stop_worker() -> Result<()> {
    let dir = data_dir();
    if !dir.exists() {
        return Ok(());
    }
    std::fs::write(dir.join("stop"), b"stop")?;
    let start = Instant::now();
    while running() {
        ensure!(
            start.elapsed() < Duration::from_secs(60),
            "后台仍在完成请求 / 恢复；未强杀、未覆盖状态，请稍后重试"
        );
        thread::sleep(Duration::from_millis(200));
    }
    runtime::recover()?;
    Ok(())
}

pub fn disable() -> Result<()> {
    require_admin()?;
    if data_dir().exists() {
        secure_dir(&data_dir())?;
    }
    // Remove boot trigger only after the running worker has safely rolled back.
    stop_worker()?;
    #[cfg(target_os = "macos")]
    {
        let plist = Path::new("/Library/LaunchDaemons").join(format!("{LABEL}.plist"));
        if plist.exists() {
            let _ = platform::run(
                Path::new("/bin/launchctl"),
                &["bootout", "system", &plist.to_string_lossy()],
            );
            std::fs::remove_file(plist)?;
        }
    }
    #[cfg(windows)]
    {
        platform::powershell(&format!("Get-ScheduledTask -TaskName '{LABEL}' -ErrorAction SilentlyContinue | Unregister-ScheduledTask -Confirm:$false"))?;
    }
    if data_dir().exists() {
        Status::new("stopped", "已停止分流并移除自动启动；原出口节点未改变").save()?;
    }
    Ok(())
}

pub fn install(config_path: &Path) -> Result<()> {
    require_admin()?;
    let config = runtime::read_config(config_path)?;
    // Validate BEFORE disturbing a running session. Exit-node DNS remains untouched.
    platform::exit_node()?;
    platform::select_network(&config)?;
    disable()?;
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    secure_dir(&dir)?;
    let executable = dir.join(if cfg!(windows) {
        "splitgui-service.exe"
    } else {
        "splitgui-service"
    });
    let source = std::env::current_exe()?;
    if source != executable {
        std::fs::copy(source, &executable)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
    }
    atomic_json(&dir.join("config.json"), &config)?;
    if dir.join("stop").exists() {
        std::fs::remove_file(dir.join("stop"))?;
    }
    Status::new(
        "starting",
        "后台已安装，正在进行 DNS / 路由验收；此时不代表分流成功",
    )
    .save()?;
    #[cfg(target_os = "macos")]
    {
        let p = Path::new("/Library/LaunchDaemons").join(format!("{LABEL}.plist"));
        let xml=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{LABEL}</string><key>ProgramArguments</key><array><string>{}</string><string>supervise</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>30</integer></dict></plist>",executable.display());
        std::fs::write(&p, xml)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644))?;
        platform::run(
            Path::new("/bin/launchctl"),
            &["bootstrap", "system", &p.to_string_lossy()],
        )?;
    }
    #[cfg(windows)]
    {
        platform::powershell(&format!("$a=New-ScheduledTaskAction -Execute {} -Argument 'supervise'; $t=New-ScheduledTaskTrigger -AtStartup; $p=New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest; $s=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries; Register-ScheduledTask -TaskName '{LABEL}' -Action $a -Trigger $t -Principal $p -Settings $s -Force | Out-Null; Start-ScheduledTask -TaskName '{LABEL}'",ps_quote(&executable.to_string_lossy())))?;
    }
    Ok(())
}

pub fn require_admin() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        ensure!(
            platform::run(Path::new("/usr/bin/id"), &["-u"])? == "0",
            "此操作需要系统管理员授权"
        );
    }
    #[cfg(windows)]
    {
        ensure!(platform::powershell("([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)")?.eq_ignore_ascii_case("true"),"此操作需要管理员权限");
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    bail!("仅 Windows / macOS 支持应用网络设置");
    #[allow(unreachable_code)]
    Ok(())
}

fn secure_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
            "拒绝符号链接状态目录"
        );
        ensure!(
            std::fs::metadata(path)?.uid() == 0,
            "状态目录不是 root 所有，拒绝接管"
        );
        for file in std::fs::read_dir(path)? {
            let m = std::fs::symlink_metadata(file?.path())?;
            ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.uid() == 0,
                "状态目录包含非 root 文件 / 链接，拒绝写入"
            );
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    {
        platform::powershell(&format!("$p={}; $items=@(Get-Item -LiteralPath $p)+@(Get-ChildItem -LiteralPath $p -Force); foreach($item in $items){{if($item.Attributes -band [IO.FileAttributes]::ReparsePoint){{throw 'Reparse point not allowed'}}; $owner=(Get-Acl -LiteralPath $item.FullName).GetOwner([Security.Principal.SecurityIdentifier]).Value; if($owner -notin @('S-1-5-18','S-1-5-32-544')){{throw 'State must be owned by SYSTEM or Administrators'}}}}; $acl=New-Object Security.AccessControl.DirectorySecurity; $acl.SetAccessRuleProtection($true,$false); foreach($entry in @(@('S-1-5-18','FullControl'),@('S-1-5-32-544','FullControl'),@('S-1-5-32-545','ReadAndExecute'))){{$sid=New-Object Security.Principal.SecurityIdentifier($entry[0]); $rule=New-Object Security.AccessControl.FileSystemAccessRule($sid,$entry[1],'ContainerInherit,ObjectInherit','None','Allow'); $acl.AddAccessRule($rule)}}; Set-Acl -LiteralPath $p -AclObject $acl",ps_quote(&path.to_string_lossy())))?;
    }
    Ok(())
}

/// The GUI is unprivileged; only fixed subcommands are elevated by the OS.
pub fn elevate(action: &str, config: Option<&Path>) -> Result<()> {
    ensure!(["install", "disable"].contains(&action), "不允许的提权命令");
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "macos")]
    {
        let mut command = format!("{} {}", sh_quote(&exe.to_string_lossy()), action);
        if let Some(p) = config {
            command.push(' ');
            command.push_str(&sh_quote(&p.to_string_lossy()));
        }
        // Escape AppleScript string independently from POSIX shell quoting.
        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            command.replace('\\', "\\\\").replace('"', "\\\"")
        );
        // Authorization dialogs may take longer than a network command.
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(windows)]
    {
        // Windows argument quoting must survive Start-Process's ArgumentList join.
        let argument = if let Some(p) = config {
            format!("{action} \"{}\"", p.to_string_lossy())
        } else {
            action.into()
        };
        let script=format!("$ErrorActionPreference='Stop'; $p=Start-Process -FilePath {} -ArgumentList {} -Verb RunAs -Wait -PassThru; if($p.ExitCode -ne 0){{throw 'Administrator operation failed; see status/log'}}",ps_quote(&exe.to_string_lossy()),ps_quote(&argument));
        let output = Command::new(crate::win_paths::powershell()?)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (exe, config);
        bail!("此构建仅用于核心测试，应用设置需要 Windows / macOS");
    }
    #[allow(unreachable_code)]
    Ok(())
}
