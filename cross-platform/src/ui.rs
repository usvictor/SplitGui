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
            .with_inner_size([1020., 780.])
            .with_min_inner_size([800., 620.]),
        ..Default::default()
    };
    eframe::run_native(
        "SplitGUI · 2.0 Alpha 2",
        options,
        Box::new(|cc| {
            load_font(&cc.egui_ctx);
            apply_theme(&cc.egui_ctx);
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
                for name in ["simsun.ttc", "simhei.ttf", "msyh.ttc"] {
                    paths.insert(0, windows.join("Fonts").join(name));
                }
            }
        }
        paths
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "splitgui-cjk".into(),
        egui::FontData::from_static(include_bytes!("../assets/SplitGUISans-Regular.ttf")).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .get_mut(&family)
            .unwrap()
            .insert(0, "splitgui-cjk".into());
    }
    // The bundled font covers common Chinese. System fonts supply rarer glyphs.
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "system-cjk".into(),
                egui::FontData::from_owned(bytes).into(),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .get_mut(&family)
                    .unwrap()
                    .insert(1, "system-cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
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
    tab: usize,
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
        let mut app=Self {tab:0,domains:config.domestic_domains.join("\n"),dns:config.domestic_dns.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"),cidrs:config.china_routes.join("\n"),interface:config.physical_interface,networks:vec![],status:None,message:"首次运行请检查网卡和国内域名，然后启用。需要 Tailscale 已连接出口并允许局域网访问。".into(),pending:None,discovery:None,last_read:Instant::now()-Duration::from_secs(3)};
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
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .inner_margin(20),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("SplitGUI")
                            .size(28.)
                            .strong()
                            .color(egui::Color32::from_rgb(23, 44, 78)),
                    );
                    ui.add_space(12.);
                    ui.label(
                        egui::RichText::new("让国内直连，让海外保持原出口")
                            .color(egui::Color32::from_rgb(92, 107, 128)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.small("2.0 · Alpha 2");
                    });
                });
            });
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.small("关闭窗口后后台继续运行；需要退出分流时请点击“停止并恢复”。");
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                card(ui, |ui| {
                    let (state, message) = self.status.as_ref().map(|s| (s.state.as_str(), s.message.as_str())).unwrap_or(("stopped", "选择本地网卡，确认 Tailscale 出口后即可启用。"));
                    let message = message.to_owned();
                    let (label, color) = match state {
                        "active" => ("分流已启用", egui::Color32::from_rgb(21, 128, 88)),
                        "starting" => ("正在验证网络", egui::Color32::from_rgb(37, 99, 235)),
                        "error" => ("启用失败", egui::Color32::from_rgb(185, 50, 50)),
                        "degraded" => ("部分请求异常", egui::Color32::from_rgb(170, 105, 20)),
                        "unknown" => ("状态待确认", egui::Color32::from_rgb(170, 105, 20)),
                        _ => ("尚未启用", egui::Color32::from_rgb(92, 107, 128)),
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(label).size(23.).strong().color(color));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(self.pending.is_none(), egui::Button::new("停止并恢复").min_size(egui::vec2(116., 38.))).clicked() { self.action("disable", None); }
                            if ui.add_enabled(self.pending.is_none(), egui::Button::new(egui::RichText::new("启用 / 应用设置").color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(37, 99, 235)).min_size(egui::vec2(148., 38.))).clicked() { self.start(); }
                        });
                    });
                    ui.add_space(8.);
                    // Clone before interaction so action handlers can update App safely.
                    ui.label(&message);
                    if let Some(status) = &self.status {
                        ui.add_space(10.);
                        ui.horizontal(|ui| {
                            ui.label(format!("已验证回答  {}", status.answers)); ui.separator();
                            ui.label(format!("失败请求  {}", status.failures));
                        });
                        if let Some(error) = &status.last_dns_error { ui.colored_label(egui::Color32::from_rgb(185, 50, 50), error); }
                    }
                });
                ui.add_space(12.);
                card(ui, |ui| {
                    ui.horizontal(|ui| { if self.pending.is_some() { ui.spinner(); } ui.strong("操作反馈"); });
                    ui.add_space(6.);
                    ui.label(&self.message);
                    if ui.small_button("复制诊断信息").clicked() {
                        let state = self.status.as_ref().map(|s| format!("{}: {}", s.state, s.message)).unwrap_or_else(|| "尚无后台状态".into());
                        ctx.copy_text(format!("SplitGUI 2.0 Alpha 2\n系统：{}\n操作：{}\n后台：{}", std::env::consts::OS, self.message, state));
                    }
                });
                ui.add_space(16.);
                ui.horizontal(|ui| {
                    for (index, label) in ["连接设置", "国内域名", "高级与帮助"].iter().enumerate() { ui.selectable_value(&mut self.tab, index, *label); }
                });
                ui.add_space(10.);
                card(ui, |ui| {
                    ui.add_enabled_ui(self.pending.is_none(), |ui| {
                        match self.tab {
                            0 => {
                                section(ui, "本地网络", "国内连接通过这张网卡直连。多张网卡同时在线时，请手动选择。");
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt("network").width(480.).selected_text(self.interface.as_deref().unwrap_or("自动选择（仅一个候选时）")).show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.interface, None, "自动选择");
                                        for n in &self.networks { ui.selectable_value(&mut self.interface, Some(n.interface.clone()), format!("网卡 {}   ·   本机 {}   ·   网关 {}", n.interface, n.address, n.gateway)); }
                                    });
                                    if ui.add_enabled(self.discovery.is_none(), egui::Button::new("刷新网卡")).clicked() { self.discover(); }
                                    if self.discovery.is_some() { ui.spinner(); }
                                });
                                ui.add_space(24.);
                                section(ui, "国内 DNS", "每行一个 IPv4 地址，仅用于“国内域名”列表中的请求。");
                                ui.add(egui::TextEdit::multiline(&mut self.dns).desired_rows(3).desired_width(f32::INFINITY).margin(12.));
                                ui.add_space(20.);
                                section(ui, "启用前确认", "1. Tailscale 已连接并选中出口节点。\n2. 已开启 Allow Local Network Access（允许局域网访问）。\n3. 已停止旧版 SplitGUI，避免同时修改路由。");
                                ui.small("海外域名保留原解析路径；局域网设备继续走本地网络。");
                            }
                            1 => {
                                let count = self.domains.lines().filter(|s| !s.trim().is_empty() && !s.trim().starts_with('#')).count();
                                section(ui, &format!("国内域名 · {count} 条"), "每行一个域名，例如 qq.com；自动包含子域名，无需填写 https://。");
                                ui.add(egui::TextEdit::multiline(&mut self.domains).desired_rows(14).desired_width(f32::INFINITY).margin(12.));
                                ui.add_space(10.);
                                if ui.button("导入旧版域名列表").clicked() { self.import_legacy(); }
                                ui.small("修改后点击顶部“启用 / 应用设置”生效。局域网和 Tailnet 保留名称不会被接管。");
                            }
                            _ => {
                                section(ui, "中国 IPv4 路由（可选）", "可粘贴经过核对的 CIDR 列表，每行一个。不填写时按国内域名动态直连。大量路由首次启用较慢。");
                                ui.add(egui::TextEdit::multiline(&mut self.cidrs).desired_rows(5).desired_width(f32::INFINITY).margin(12.));
                                ui.add_space(18.);
                                ui.collapsing("DNS 与浏览器设置", |ui| {
                                    ui.label("国内 DNS 失败时不会回退到海外 DNS，也不会更换出口。浏览器安全 DNS（DoH）、独立代理和应用缓存可能绕过系统分流，需要单独检查。");
                                    ui.label("当前国内域名采用 IPv4 策略；使用明文 UDP/TCP DNS，尚不支持完整 IPv6 分流。");
                                });
                                ui.collapsing("状态与恢复", |ui| {
                                    ui.label("只有 DNS 和路由验证通过后才显示已启用。停止按钮撤销本程序规则并取消自动启动，保留原出口节点。");
                                    ui.label(format!("后台记录目录：{}", platform::data_dir().display()));
                                    ui.label("此版本已提供自动测试，真实网络仍需验证国内直连、海外出口及局域网连通性。");
                                });
                            }
                        }
                    });
                });
            });
        });
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = egui::Color32::from_rgb(242, 245, 250);
    visuals.override_text_color = Some(egui::Color32::from_rgb(35, 49, 70));
    visuals.selection.bg_fill = egui::Color32::from_rgb(210, 227, 255);
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10., 10.);
    style.spacing.button_padding = egui::vec2(14., 9.);
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(16.));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(16.));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(13.));
    ctx.set_style(style);
}
fn card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(egui::Color32::WHITE)
        .corner_radius(12)
        .stroke(egui::Stroke::new(
            1_f32,
            egui::Color32::from_rgb(223, 230, 240),
        ))
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
}
fn section(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.label(egui::RichText::new(title).size(19.).strong());
    ui.label(egui::RichText::new(description).color(egui::Color32::from_rgb(94, 109, 131)));
    ui.add_space(8.);
}
