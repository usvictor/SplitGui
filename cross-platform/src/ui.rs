use anyhow::Result;
use eframe::egui;
use std::{
    path::PathBuf,
    sync::mpsc::{channel, Receiver},
    time::{Duration, Instant},
};
use tailscale_splitgui::{config::Config, platform, routes::Network, runtime, service};

pub fn launch() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([940., 760.])
            .with_min_inner_size([760., 580.]),
        ..Default::default()
    };
    eframe::run_native(
        "Tailscale SplitGUI · 2.0 Alpha",
        options,
        Box::new(|cc| {
            load_font(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(App::new()))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}

fn load_font(ctx: &egui::Context) {
    let paths: Vec<PathBuf> = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    #[cfg(windows)]
    let paths = {
        let mut paths = paths;
        if let Ok(system) = tailscale_splitgui::win_paths::system_directory() {
            if let Some(windows) = system.parent() {
                paths.insert(0, windows.join("Fonts").join("msyh.ttc"));
            }
        }
        paths
    };
    for p in paths {
        if let Ok(bytes) = std::fs::read(p) {
            let mut f = egui::FontDefinitions::default();
            f.font_data
                .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            f.families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "cjk".into());
            f.families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .push("cjk".into());
            ctx.set_fonts(f);
            break;
        }
    }
}

fn user_dir() -> PathBuf {
    #[cfg(windows)]
    {
        return tailscale_splitgui::win_paths::known_folder(
            &windows_sys::Win32::UI::Shell::FOLDERID_LocalAppData,
        )
        .expect("Windows LocalAppData unavailable")
        .join("TailscaleSplitGUI-v2");
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("Library/Application Support/TailscaleSplitGUI-v2")
    }
}

struct App {
    domains: String,
    dns: String,
    cidrs: String,
    interface: Option<String>,
    networks: Vec<Network>,
    status: Option<runtime::Status>,
    message: String,
    pending: Option<Receiver<Result<String, String>>>,
    discovery: Option<Receiver<Result<Vec<Network>, String>>>,
    last_read: Instant,
}
impl App {
    fn new() -> Self {
        let config = runtime::read_config(&user_dir().join("config.json"))
            .or_else(|_| runtime::read_config(&platform::data_dir().join("config.json")))
            .unwrap_or_default();
        let mut app=Self {domains:config.domestic_domains.join("\n"),dns:config.domestic_dns.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"),cidrs:config.china_routes.join("\n"),interface:config.physical_interface,networks:vec![],status:None,message:"首次运行请检查网卡和国内域名，然后启用。需要 Tailscale 已连接出口并允许局域网访问。".into(),pending:None,discovery:None,last_read:Instant::now()-Duration::from_secs(3)};
        app.discover();
        app
    }
    fn discover(&mut self) {
        let (tx, rx) = channel();
        self.discovery = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(platform::networks().map_err(|e| format!("{e:#}")));
        });
    }
    fn config(&self) -> Result<Config> {
        let lines = |s: &str| {
            s.lines()
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.starts_with('#'))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let mut c = Config {
            version: 2,
            physical_interface: self.interface.clone(),
            domestic_domains: lines(&self.domains),
            domestic_dns: lines(&self.dns)
                .iter()
                .map(|s| s.parse())
                .collect::<std::result::Result<_, _>>()?,
            china_routes: lines(&self.cidrs),
        };
        c.validate()?;
        Ok(c)
    }
    fn start(&mut self) {
        let result = (|| -> Result<PathBuf> {
            let c = self.config()?;
            let dir = user_dir();
            std::fs::create_dir_all(&dir)?;
            let p = dir.join("config.json");
            std::fs::write(&p, serde_json::to_vec_pretty(&c)?)?;
            Ok(p)
        })();
        match result {
            Ok(path) => self.action("install", Some(path)),
            Err(e) => self.message = format!("配置无效：{e:#}"),
        }
    }
    fn action(&mut self, action: &'static str, path: Option<PathBuf>) {
        let (tx, rx) = channel();
        self.pending = Some(rx);
        self.message = "请完成系统管理员授权；正在执行…".into();
        std::thread::spawn(move || {
            let r = service::elevate(action, path.as_deref())
                .map(|_| {
                    if action == "install" {
                        "后台已启动，等待实际验收结果".into()
                    } else {
                        "停止 / 恢复操作已完成".into()
                    }
                })
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
    }
    fn import_legacy(&mut self) {
        #[cfg(not(windows))]
        let path = PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("Library/Application Support/TailscaleSplitGUI/direct-domains.txt");
        #[cfg(windows)]
        let path = user_dir().join("direct-domains.txt");
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let mut c = Config {
                    domestic_domains: s.lines().map(str::to_owned).collect(),
                    ..Config::default()
                };
                match c.validate() {
                    Ok(()) => {
                        self.domains = c.domestic_domains.join("\n");
                        self.message =
                            "已导入旧域名列表。新规则覆盖该名称及其子域名；检查后再启用。".into()
                    }
                    Err(e) => self.message = e.to_string(),
                }
            }
            Err(e) => self.message = format!("未找到旧域名文件：{e}"),
        }
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.pending {
            if let Ok(r) = rx.try_recv() {
                self.message = r.unwrap_or_else(|e| e);
                self.pending = None;
            }
        }
        if let Some(rx) = &self.discovery {
            if let Ok(r) = rx.try_recv() {
                match r {
                    Ok(n) => self.networks = n,
                    Err(e) => self.message = e,
                };
                self.discovery = None;
            }
        }
        if self.last_read.elapsed() > Duration::from_secs(2) {
            self.status = runtime::read_status().ok();
            self.last_read = Instant::now();
        }
        egui::CentralPanel::default().show(ctx,|ui|{
            ui.heading("Tailscale SplitGUI");ui.label("Windows / macOS · Rust 共享核心 · 2.0 Alpha");ui.add_space(10.);
            egui::Frame::group(ui.style()).show(ui,|ui|{
                let(state,text)=self.status.as_ref().map(|s|(s.state.as_str(),s.message.as_str())).unwrap_or(("stopped","尚未启用"));
                let color=match state{"active"=>egui::Color32::LIGHT_GREEN,"error"|"degraded"|"unknown"=>egui::Color32::YELLOW,_=>egui::Color32::LIGHT_BLUE};
                ui.colored_label(color,format!("状态：{state}"));ui.label(text);
                if let Some(s)=&self.status{ui.label(format!("验证通过的 DNS 回答：{}　失败：{}",s.answers,s.failures));if let Some(e)=&s.last_dns_error{ui.colored_label(egui::Color32::YELLOW,e);}}
            });
            ui.add_space(8.);
            ui.horizontal(|ui|{
                if ui.add_enabled(self.pending.is_none(),egui::Button::new("启用 / 应用设置")).clicked(){self.start();}
                if ui.add_enabled(self.pending.is_none(),egui::Button::new("停止 / 恢复原网络")).clicked(){self.action("disable",None);}
                if self.pending.is_some(){ui.spinner();}
            });ui.label(&self.message);ui.separator();
            egui::ScrollArea::vertical().show(ui,|ui|{
                ui.strong("本地网络");
                egui::ComboBox::from_label("物理网卡").selected_text(self.interface.as_deref().unwrap_or("自动（仅一个候选时）")).show_ui(ui,|ui|{
                    ui.selectable_value(&mut self.interface,None,"自动");
                    for n in &self.networks{ui.selectable_value(&mut self.interface,Some(n.interface.clone()),format!("{} · {} → {}",n.interface,n.address,n.gateway));}
                });if ui.button("刷新网卡").clicked(){self.discover();}
                ui.add_space(8.);ui.strong("国内域名（每行一个，包含其子域名）");
                ui.label("只管理这里的域名。未列出的海外域名继续使用当前系统 / Tailscale DNS。局域网和 Tailnet 保留名称不允许加入。");
                ui.add(egui::TextEdit::multiline(&mut self.domains).desired_rows(10).desired_width(f32::INFINITY));
                if ui.button("导入旧版域名列表").clicked(){self.import_legacy();}
                ui.add_space(8.);ui.strong("国内 DNS（每行一个 IPv4）");
                ui.add(egui::TextEdit::multiline(&mut self.dns).desired_rows(2).desired_width(f32::INFINITY));
                ui.label("只在国内 DNS 之间重试。超时 / 空响应 / 异常 IP 返回失败，不回退到海外 DNS。");
                ui.collapsing("中国 IPv4 路由（可选，粘贴经核对的 CIDR 列表）",|ui|{
                    ui.label("保留系统原有中国路由。Windows 新安装如需 IP 级国内分流，可导入完整列表；未导入时提供上述域名的动态直连。大量路由首次启用较慢。");
                    ui.add(egui::TextEdit::multiline(&mut self.cidrs).desired_rows(6).desired_width(f32::INFINITY));
                });
                ui.collapsing("运行范围与自检说明",|ui|{
                    ui.label("启用后安装后台任务，重启时会重新验收；关闭窗口不停止分流。停止按钮同时取消自动启动。请先停止旧版 SplitGUI，避免双写路由。");
                    ui.label("国内域名使用 IPv4；AAAA / HTTPS / SVCB 返回无数据，其他 DNS 类型不接管。IPv6 全分流不在此 Alpha 范围。");
                    ui.label("浏览器自带 DoH、应用代理或独立 DNS 可能绕过系统规则。系统自检通过不代表这些应用已通过；请关闭其独立 DNS 后再验收。");
                    ui.label("本版使用明文国内 UDP/TCP DNS；只对明确配置的域名发送请求，不能称为加密 DNS 或零泄漏方案。");
                    ui.label("实机发布前需在 Windows / macOS 完成断网、切换 Wi-Fi、休眠恢复和卸载验收。");
                });
            });
        });
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}
