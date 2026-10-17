#[cfg(feature = "gui")]
mod ui;
use anyhow::{bail, Result};
use tailscale_splitgui::{runtime, service};

fn main() {
    if let Err(e) = entry() {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
fn entry() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("supervise") => {
            service::require_admin()?;
            service::supervisor()
        }
        Some("worker") => {
            service::require_admin()?;
            runtime::worker()
        }
        Some("install") if args.len() == 3 => service::install(std::path::Path::new(&args[2])),
        Some("disable") => service::disable(),
        Some("probe") if args.len() == 3 => runtime::system_probe(&args[2]),
        Some("check-config") if args.len() == 3 => {
            runtime::read_config(std::path::Path::new(&args[2]))?;
            println!("Config valid");
            Ok(())
        }
        Some("status") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&runtime::read_status()?)?
            );
            Ok(())
        }
        None => {
            #[cfg(feature = "gui")]
            {
                ui::launch()
            }
            #[cfg(not(feature = "gui"))]
            {
                bail!("GUI feature disabled")
            }
        }
        _ => {
            bail!("Usage: tailscale-splitgui [status | check-config FILE | install FILE | disable]")
        }
    }
}
