use anyhow::{Context, Result, bail};
use shadowroom::{
    media::PreviewKind,
    remote::{Agent, AgentOptions, ConnectionSpec, RemoteHost, protocol::MAX_PAYLOAD},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn value(args: &[String], name: &str) -> Result<Option<String>> {
    if let Some(i) = args.iter().position(|s| s == name) {
        return Ok(Some(
            args.get(i + 1)
                .filter(|s| !s.starts_with("--"))
                .with_context(|| format!("Missing value for {name}"))?
                .clone(),
        ));
    }
    Ok(None)
}
fn validate_args(args: &[String], flags: &[&str], values: &[&str]) -> Result<()> {
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if flags.contains(&arg) {
            i += 1;
        } else if values.contains(&arg) {
            let _ = value(&args[i..], arg)?;
            i += 2;
        } else {
            bail!("Unknown argument: {arg}");
        }
    }
    Ok(())
}
fn default_state() -> Result<PathBuf> {
    Ok(
        directories::ProjectDirs::from("app", "shadowroom", "shadowroom-remote")
            .context("No user data directory; use --state-dir")?
            .data_local_dir()
            .to_path_buf(),
    )
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|s| s == "--version" || s == "version")
    {
        println!(
            "shadowroom-remote {} protocol {}",
            env!("CARGO_PKG_VERSION"),
            shadowroom::remote::protocol::PROTOCOL_VERSION
        );
        return Ok(());
    }
    match args.first().map(String::as_str) {
        Some("serve") => {
            validate_args(
                &args,
                &["--stdio", "--no-discover"],
                &[
                    "--state-dir",
                    "--root",
                    "--cache-budget-mb",
                    "--decode-budget-mb",
                ],
            )?;
            if !args.iter().any(|s| s == "--stdio") {
                bail!("serve requires --stdio");
            }
            let state = match value(&args, "--state-dir")? {
                Some(p) => PathBuf::from(p),
                None => default_state()?,
            };
            let mut options = AgentOptions::new(state);
            if let Some(mb) = value(&args, "--cache-budget-mb")? {
                options.jpeg_cache_bytes = mb.parse::<u64>()?.saturating_mul(1024 * 1024);
            }
            if let Some(mb) = value(&args, "--decode-budget-mb")? {
                let mb = mb.parse::<usize>()?;
                if !(64..=65_536).contains(&mb) {
                    bail!("--decode-budget-mb must be between 64 and 65536");
                }
                options.decode_budget_bytes = mb
                    .checked_mul(1024 * 1024)
                    .context("decode memory budget overflow")?;
            }
            options.discover_drives = !args.iter().any(|s| s == "--no-discover");
            for (i, arg) in args.iter().enumerate() {
                if arg == "--root" {
                    options.roots.push(PathBuf::from(
                        args.get(i + 1).context("--root needs a path")?,
                    ));
                }
            }
            let agent = Agent::new(options)?;
            agent.serve(std::io::stdin().lock(), std::io::stdout())
        }
        Some("probe") => {
            validate_args(
                &args,
                &[],
                &[
                    "--ssh",
                    "--remote-cmd",
                    "--cache-dir",
                    "--index",
                    "--fetch-thumbs",
                    "--out",
                ],
            )?;
            let host = value(&args, "--ssh")?.context("probe requires --ssh <alias>")?;
            let cmd = value(&args, "--remote-cmd")?
                .unwrap_or_else(|| "shadowroom-remote serve --stdio".into());
            let cache = match value(&args, "--cache-dir")? {
                Some(path) => PathBuf::from(path),
                None => default_state()?.join("probe-cache"),
            };
            let start = Instant::now();
            let remote = RemoteHost::connect(
                ConnectionSpec::ssh(host, cmd),
                cache,
                Duration::from_secs(300),
            )?;
            println!(
                "connect_ms={:.3} hello={}",
                start.elapsed().as_secs_f64() * 1000.0,
                serde_json::to_string(&remote.hello())?
            );
            let roots = remote.list_roots()?;
            println!("roots={}", serde_json::to_string(&roots)?);
            if let Some(root) = value(&args, "--index")? {
                let root = roots
                    .iter()
                    .find(|r| r.id == root || r.path.to_string_lossy() == root)
                    .context("--index must match a reported root id or path")?;
                let start = Instant::now();
                let summary = remote.index(&root.id, |_| {})?;
                println!(
                    "index_ms={:.3} summary={}",
                    start.elapsed().as_secs_f64() * 1000.0,
                    serde_json::to_string(&summary)?
                );
                let sync = remote.photos(&root.id, 0)?;
                let source_bytes: u64 = sync.records.iter().map(|r| r.key.size_bytes).sum();
                println!(
                    "photos={} source_bytes={source_bytes} revision={}",
                    sync.records.len(),
                    sync.revision
                );
                let n: usize = value(&args, "--fetch-thumbs")?
                    .unwrap_or_else(|| "0".into())
                    .parse()?;
                let out = value(&args, "--out")?.map(PathBuf::from);
                if let Some(out) = &out {
                    std::fs::create_dir_all(out)?;
                }
                for (i, record) in sync.records.iter().take(n).enumerate() {
                    let before = remote.stats();
                    let start = Instant::now();
                    let preview =
                        remote.preview(&record.key, PreviewKind::Thumbnail, MAX_PAYLOAD)?;
                    let after = remote.stats();
                    println!(
                        "thumbnail={} latency_ms={:.3} jpeg_bytes={} wire_received_bytes={} wire_sent_bytes={} source_bytes={} cache_hit={}",
                        i + 1,
                        start.elapsed().as_secs_f64() * 1000.0,
                        preview.jpeg.len(),
                        after.received_bytes - before.received_bytes,
                        after.sent_bytes - before.sent_bytes,
                        record.key.size_bytes,
                        preview.cache_hit
                    );
                    if let Some(out) = &out {
                        std::fs::write(out.join(format!("thumbnail-{i:04}.jpg")), &preview.jpeg)?;
                    }
                    let before = remote.stats();
                    let again = remote.preview(&record.key, PreviewKind::Thumbnail, MAX_PAYLOAD)?;
                    let after = remote.stats();
                    println!(
                        "repeat_thumbnail={} cache_hit={} wire_bytes={}",
                        i + 1,
                        again.cache_hit,
                        after.received_bytes - before.received_bytes + after.sent_bytes
                            - before.sent_bytes
                    );
                }
            }
            let stats = remote.stats();
            println!(
                "total_sent_bytes={} total_received_bytes={} total_payload_bytes={}",
                stats.sent_bytes, stats.received_bytes, stats.received_payload_bytes
            );
            remote.disconnect();
            Ok(())
        }
        _ => {
            eprintln!(
                "shadowroom-remote serve --stdio [--state-dir DIR] [--root DIR]... [--no-discover]\nshadowroom-remote probe --ssh HOST [--remote-cmd CMD] [--index ROOT] [--fetch-thumbs N --out DIR] [--cache-dir DIR]\nshadowroom-remote --version"
            );
            Ok(())
        }
    }
}
