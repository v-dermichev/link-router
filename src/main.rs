mod client;
mod config;
mod daemon;
mod desktop;
mod hypr;
mod intercept;
mod mpv;
mod paths;
mod resolve;

use std::path::Path;

const USAGE: &str = "usage:
  link-router enable [--yes]   shadow the default browser entries and start intercepting
  link-router disable          stop the daemon, remove shadows, restore originals
  link-router sentinel         check and repair shadows once
  link-router doctor           show interception status
  link-router stop             stop the daemon (the next link starts it again)
  link-router version          print the version
  link-router open URL...      route links from a terminal
  link-router daemon [--resident]
                               run the daemon (started on demand by the client;
                               --resident, for service managers, never idles out)
  link-router resolve URL      print what the mpv-video resolvers return, without playing";

fn main() {
    let mut args = std::env::args();
    let argv0 = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    let path = Path::new(&argv0);
    if path.parent().and_then(|p| p.file_name()).map(|n| n == "by-id").unwrap_or(false) {
        let id = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        client::run_by_id(&id, rest);
    }

    let result = match rest.first().map(String::as_str) {
        Some("open") => client::run_open(rest[1..].to_vec()),
        Some("daemon") => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(daemon::run(rest.iter().any(|a| a == "--resident")))),
        Some("enable") => intercept::enable(rest.iter().any(|a| a == "--yes")),
        Some("disable") => intercept::disable(),
        Some("sentinel") => {
            let mut state = intercept::State::load();
            intercept::sentinel(&mut state, intercept::UserOverride::Leave).map(|lines| lines.iter().for_each(|l| println!("{l}")))
        }
        Some("doctor") => intercept::doctor(),
        Some("stop") => {
            client::shutdown_daemon();
            Ok(())
        }
        Some("version" | "--version" | "-V") => {
            println!("link-router {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("resolve") => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(daemon::resolve_only(rest[1..].to_vec()))),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("link-router: {e:#}");
        std::process::exit(1);
    }
}
