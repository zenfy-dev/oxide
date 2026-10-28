//! `oxide` — command-line archive manager.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use chrono::{Local, TimeZone};
use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use oxide_core::{
    ArchiveFormat, CancelToken, CompressionMethod, CreateOptions, EncryptionMethod, EntryKind, ExtractLimits,
    ExtractOptions, OperationSummary, OverwritePolicy, OxideError, Password, Progress, ReadOptions, SymlinkPolicy,
};
use serde_json::json;

/// Fast, safe archive manager for ZIP, 7z, TAR (gz/xz/zst/bz2) and more.
#[derive(Parser)]
#[command(name = "oxide", version, arg_required_else_help = true)]
#[command(after_help = "Exit codes: 0 ok, 1 error, 2 usage, 3 corrupt archive, 4 password, \
                        5 unsafe entry or limit exceeded, 6 destination exists, 130 cancelled")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// More log output (repeat for debug logs).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Print nothing except errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Print machine-readable JSON on stdout.
    #[arg(long, global = true)]
    json: bool,

    /// Never show a progress bar.
    #[arg(long, global = true)]
    no_progress: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Create an archive from files and folders.
    #[command(visible_alias = "c")]
    Create(CreateArgs),
    /// Extract an archive.
    #[command(visible_alias = "x")]
    Extract(ExtractArgs),
    /// List an archive's contents.
    #[command(visible_alias = "l", visible_alias = "ls")]
    List(ListArgs),
    /// Verify an archive's integrity without extracting it.
    #[command(visible_alias = "t")]
    Test(ReadArgs),
    /// Add files to an existing ZIP or TAR archive.
    #[command(visible_alias = "a")]
    Add(AddArgs),
    /// Show supported formats and their capabilities.
    Formats,
}

#[derive(Args)]
struct PasswordArgs {
    /// Archive password. Visible to other local users via the process list;
    /// prefer --password-env in shared environments.
    #[arg(short = 'p', long, value_name = "PASSWORD")]
    password: Option<String>,

    /// Read the password from this environment variable.
    #[arg(long, value_name = "VAR", conflicts_with = "password")]
    password_env: Option<String>,
}

impl PasswordArgs {
    fn get(&self) -> anyhow::Result<Option<Password>> {
        match (&self.password, &self.password_env) {
            (Some(p), _) => Ok(Some(Password::new(p.clone()))),
            (None, Some(var)) => std::env::var(var)
                .map(|p| Some(Password::new(p)))
                .with_context(|| format!("environment variable {var} is not set")),
            (None, None) => Ok(None),
        }
    }
}

#[derive(Args)]
struct CompressArgs {
    /// Compression level (range depends on the method; see `oxide formats`).
    #[arg(short, long, allow_negative_numbers = false)]
    level: Option<i32>,

    /// Compression method: store, deflate, bzip2, zstd, xz, lzma2.
    #[arg(short, long, value_parser = parse_method)]
    method: Option<CompressionMethod>,

    #[command(flatten)]
    password: PasswordArgs,

    /// Encryption scheme when a password is given.
    #[arg(long, value_enum)]
    encryption: Option<EncryptionArg>,

    /// Store files at the archive root instead of keeping folder structure.
    #[arg(long)]
    flatten: bool,

    /// Skip hidden files and folders.
    #[arg(long)]
    exclude_hidden: bool,

    /// Archive what symbolic links point to instead of the links.
    #[arg(long)]
    follow_symlinks: bool,

    /// Compression threads for formats that support them (0 = all cores).
    #[arg(short, long, default_value_t = 0)]
    threads: u32,
}

#[derive(Args)]
struct CreateArgs {
    /// Files and folders to archive.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Archive to write; the format is inferred from the extension.
    #[arg(short, long)]
    output: PathBuf,

    /// Force a format (zip, 7z, tar, tar.gz, tar.xz, tar.zst, tar.bz2, gz, bz2, xz, zst).
    #[arg(short, long, value_parser = parse_format)]
    format: Option<ArchiveFormat>,

    /// Replace the output file if it already exists.
    #[arg(long)]
    force: bool,

    #[command(flatten)]
    compress: CompressArgs,
}

#[derive(Args)]
struct AddArgs {
    /// Existing archive to modify.
    archive: PathBuf,

    /// Files and folders to add. Existing entries with the same name are replaced.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    #[command(flatten)]
    compress: CompressArgs,
}

#[derive(Args)]
struct ReadArgs {
    /// The archive.
    archive: PathBuf,

    /// Treat the file as this format instead of detecting it.
    #[arg(short, long, value_parser = parse_format)]
    format: Option<ArchiveFormat>,

    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Args)]
struct ListArgs {
    #[command(flatten)]
    read: ReadArgs,

    /// Print only entry paths, one per line.
    #[arg(short = '1', long)]
    names_only: bool,
}

#[derive(Args)]
struct ExtractArgs {
    #[command(flatten)]
    read: ReadArgs,

    /// Destination folder (created if missing). Defaults to the current folder.
    #[arg(short, long, default_value = ".")]
    output: PathBuf,

    /// What to do when a file already exists.
    #[arg(long, value_enum, default_value_t = OverwriteArg::Error)]
    overwrite: OverwriteArg,

    /// Extract only these archive paths (folders include their contents).
    #[arg(long = "only", value_name = "PATH")]
    only: Vec<String>,

    /// Remove N leading folders from extracted paths (selection uses original paths).
    #[arg(long, value_name = "N", default_value_t = 0)]
    strip_components: usize,

    /// Do not create symbolic links.
    #[arg(long)]
    no_symlinks: bool,

    /// Abort if more than this much data would be written (e.g. 500M, 20G).
    #[arg(long, value_parser = parse_size, default_value = "64G")]
    max_size: u64,

    /// Abort if the archive has more entries than this.
    #[arg(long, default_value_t = 1_000_000)]
    max_entries: u64,

    /// Abort if output exceeds this multiple of the archive size (bomb protection).
    #[arg(long, default_value_t = 1000)]
    max_ratio: u64,

    /// Disable all size, count and ratio limits. Only for trusted archives.
    #[arg(long)]
    no_limits: bool,

    /// Do not restore permissions or modification times.
    #[arg(long)]
    no_preserve: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum OverwriteArg {
    Error,
    Skip,
    Overwrite,
    Rename,
}

#[derive(Clone, Copy, ValueEnum)]
enum EncryptionArg {
    Aes256,
    Aes128,
}

fn parse_format(s: &str) -> Result<ArchiveFormat, String> {
    ArchiveFormat::from_name(s).ok_or_else(|| format!("unknown format {s:?}; run `oxide formats`"))
}

fn parse_method(s: &str) -> Result<CompressionMethod, String> {
    CompressionMethod::from_name(s).ok_or_else(|| format!("unknown method {s:?}"))
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    let mult: u64 = match unit.trim().to_ascii_uppercase().trim_end_matches("IB").trim_end_matches('B') {
        "" => 1,
        "K" => 1 << 10,
        "M" => 1 << 20,
        "G" => 1 << 30,
        "T" => 1 << 40,
        _ => return Err(format!("invalid size unit in {s:?}")),
    };
    Ok((value * mult as f64) as u64)
}

/// Shared output settings.
struct Ui {
    quiet: bool,
    json: bool,
    progress: bool,
}

impl Ui {
    fn say(&self, msg: impl std::fmt::Display) {
        if !self.quiet && !self.json {
            println!("{msg}");
        }
    }

    fn warn(&self, msg: impl std::fmt::Display) {
        if !self.quiet {
            eprintln!("warning: {msg}");
        }
    }

    /// Builds a progress handle wired to a terminal bar and Ctrl+C.
    fn progress(&self, cancel: &CancelToken, verb: &'static str) -> (Progress, Option<ProgressBar>) {
        let base = Progress::none().cancel_with(cancel.clone());
        if !self.progress {
            return (base, None);
        }
        let bar = ProgressBar::with_draw_target(Some(10_000), ProgressDrawTarget::stderr());
        bar.set_style(
            ProgressStyle::with_template("{prefix:.bold} [{bar:30.cyan/blue}] {percent:>3}% {msg}")
                .expect("valid template")
                .progress_chars("=> "),
        );
        bar.set_prefix(verb);
        let b = bar.clone();
        let progress = Progress::with_callback(move |u| {
            if let Some(f) = u.fraction {
                b.set_position((f * 10_000.0) as u64);
            }
            let mut msg = format!("{} · {}/s", human_bytes(u.bytes_done), human_bytes(u.bytes_per_second() as u64));
            if let Some(eta) = u.eta() {
                msg.push_str(&format!(" · {} left", human_duration(eta)));
            }
            if let Some(current) = &u.current {
                msg.push_str(" · ");
                msg.push_str(&truncate_middle(current, 40));
            }
            b.set_message(msg);
        })
        .cancel_with(cancel.clone())
        .throttle(Duration::from_millis(100));
        (progress, Some(bar))
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

fn truncate_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_owned();
    }
    let half = (max - 1) / 2;
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!("{head}…{tail}")
}

fn format_time(secs: Option<i64>) -> String {
    secs.and_then(|s| Local.timestamp_opt(s, 0).single())
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".repeat(16))
}

fn compress_options(args: &CompressArgs) -> anyhow::Result<CreateOptions> {
    Ok(CreateOptions {
        method: args.method,
        level: args.level,
        password: args.password.get()?,
        encryption: args.encryption.map(|e| match e {
            EncryptionArg::Aes256 => EncryptionMethod::Aes256,
            EncryptionArg::Aes128 => EncryptionMethod::Aes128,
        }),
        preserve_paths: !args.flatten,
        include_hidden: !args.exclude_hidden,
        follow_symlinks: args.follow_symlinks,
        threads: args.threads,
        ..CreateOptions::default()
    })
}

fn report_summary(ui: &Ui, action: &str, target: &Path, s: &OperationSummary) {
    for w in &s.warnings {
        ui.warn(w);
    }
    if ui.json {
        let mut value = serde_json::to_value(s).unwrap_or_default();
        value["ok"] = json!(true);
        value["path"] = json!(target);
        println!("{value}");
        return;
    }
    let mut line = format!(
        "{action} {} ({} files, {} folders, {}) in {}",
        target.display(),
        s.files,
        s.directories,
        human_bytes(s.bytes),
        human_duration(s.elapsed)
    );
    if s.links > 0 {
        line.push_str(&format!(", {} links", s.links));
    }
    if s.skipped > 0 {
        line.push_str(&format!(", {} skipped", s.skipped));
    }
    ui.say(line);
}

fn run(cli: Cli, cancel: &CancelToken) -> anyhow::Result<()> {
    let ui = Ui {
        quiet: cli.quiet,
        json: cli.json,
        progress: !cli.quiet && !cli.json && !cli.no_progress && std::io::stderr().is_terminal(),
    };

    match cli.command {
        Command::Create(args) => {
            let mut options = compress_options(&args.compress)?;
            options.format = args.format;
            options.overwrite = args.force;
            let (progress, bar) = ui.progress(cancel, "Compressing");
            let result = oxide_core::create(&args.inputs, &args.output, &options, &progress);
            if let Some(bar) = bar {
                bar.finish_and_clear();
            }
            let summary = result?;
            report_summary(&ui, "Created", &args.output, &summary);
        }
        Command::Add(args) => {
            let options = compress_options(&args.compress)?;
            let (progress, bar) = ui.progress(cancel, "Adding");
            let result = oxide_core::add(&args.archive, &args.inputs, &options, &progress);
            if let Some(bar) = bar {
                bar.finish_and_clear();
            }
            let summary = result?;
            report_summary(&ui, "Updated", &args.archive, &summary);
        }
        Command::Extract(args) => {
            let limits = if args.no_limits {
                ExtractLimits::unlimited()
            } else {
                ExtractLimits {
                    max_total_bytes: Some(args.max_size),
                    max_entry_bytes: None,
                    max_entries: Some(args.max_entries),
                    max_ratio: Some(args.max_ratio),
                }
            };
            let options = ExtractOptions {
                overwrite: match args.overwrite {
                    OverwriteArg::Error => OverwritePolicy::Error,
                    OverwriteArg::Skip => OverwritePolicy::Skip,
                    OverwriteArg::Overwrite => OverwritePolicy::Overwrite,
                    OverwriteArg::Rename => OverwritePolicy::Rename,
                },
                symlinks: if args.no_symlinks { SymlinkPolicy::Skip } else { SymlinkPolicy::Contained },
                limits,
                selection: (!args.only.is_empty()).then_some(args.only),
                strip_components: args.strip_components,
                password: args.read.password.get()?,
                format: args.read.format,
                preserve_permissions: !args.no_preserve,
                preserve_mtime: !args.no_preserve,
            };
            let (progress, bar) = ui.progress(cancel, "Extracting");
            let result = oxide_core::extract(&args.read.archive, &args.output, &options, &progress);
            if let Some(bar) = bar {
                bar.finish_and_clear();
            }
            let summary = result?;
            report_summary(&ui, "Extracted to", &args.output, &summary);
        }
        Command::Test(args) => {
            let read = ReadOptions { password: args.password.get()?, format: args.format };
            let (progress, bar) = ui.progress(cancel, "Testing");
            let result = oxide_core::test(&args.archive, &read, &progress);
            if let Some(bar) = bar {
                bar.finish_and_clear();
            }
            let summary = result?;
            if ui.json {
                report_summary(&ui, "Tested", &args.archive, &summary);
            } else {
                ui.say(format!(
                    "OK {}: {} files, {} verified in {}",
                    args.archive.display(),
                    summary.files,
                    human_bytes(summary.bytes),
                    human_duration(summary.elapsed)
                ));
            }
        }
        Command::List(args) => {
            let read = ReadOptions { password: args.read.password.get()?, format: args.read.format };
            let info = oxide_core::list(&args.read.archive, &read)?;
            if ui.json {
                let mut value = serde_json::to_value(&info)?;
                value["ok"] = json!(true);
                value["file_count"] = json!(info.file_count());
                value["total_size"] = json!(info.total_size());
                value["compression_ratio"] = json!(info.compression_ratio());
                println!("{value}");
                return Ok(());
            }
            let mut out = std::io::stdout().lock();
            if args.names_only {
                for e in &info.entries {
                    writeln!(out, "{}", e.path)?;
                }
                return Ok(());
            }
            writeln!(out, "{:>10}  {:>10}  {:<16}  Name", "Size", "Packed", "Modified")?;
            for e in &info.entries {
                let size = e.size.map(human_bytes).unwrap_or_else(|| "?".into());
                let packed = e.compressed_size.map(human_bytes).unwrap_or_default();
                let mut name = e.path.clone();
                if let (EntryKind::Symlink | EntryKind::Hardlink, Some(t)) = (e.kind, &e.link_target) {
                    name.push_str(&format!(" -> {t}"));
                }
                if e.encrypted {
                    name.push_str("  [encrypted]");
                }
                let size = if e.is_dir() { String::new() } else { size };
                writeln!(out, "{size:>10}  {packed:>10}  {:<16}  {name}", format_time(e.modified))?;
            }
            let ratio =
                info.compression_ratio().map(|r| format!(", {:.1}% of original", r * 100.0)).unwrap_or_default();
            writeln!(
                out,
                "\n{} · {} files, {} folders · {} uncompressed · {} on disk{ratio}",
                info.format,
                info.file_count(),
                info.dir_count(),
                human_bytes(info.total_size()),
                human_bytes(info.archive_size)
            )?;
        }
        Command::Formats => {
            let formats = oxide_core::supported_formats();
            if ui.json {
                println!("{}", serde_json::to_string(&formats)?);
                return Ok(());
            }
            let mark = |b: bool| if b { "yes" } else { "no" };
            println!(
                "{:<9} {:<22} {:<8} {:<7} {:<16} {:<30} Notes",
                "Format", "Extensions", "Extract", "Create", "Encryption", "Methods (levels)"
            );
            for f in formats {
                let enc = f.encryption.iter().map(|e| e.name()).collect::<Vec<_>>().join(", ");
                let methods = f
                    .methods
                    .iter()
                    .map(|m| match m.level_range() {
                        Some(r) => format!("{m} {}-{}", r.min, r.max),
                        None => m.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "{:<9} {:<22} {:<8} {:<7} {:<16} {:<30} {}",
                    f.name,
                    f.extensions.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(" "),
                    mark(f.can_extract),
                    mark(f.can_create),
                    if enc.is_empty() { "-".into() } else { enc },
                    methods,
                    f.notes
                );
            }
        }
    }
    Ok(())
}

fn exit_code(err: &anyhow::Error) -> (u8, &'static str) {
    match err.downcast_ref::<OxideError>() {
        Some(OxideError::Corrupt(_)) => (3, "corrupt"),
        Some(OxideError::PasswordRequired) => (4, "password-required"),
        Some(OxideError::WrongPassword) => (4, "wrong-password"),
        Some(OxideError::UnsafePath { .. }) => (5, "unsafe-path"),
        Some(OxideError::LimitExceeded(_)) => (5, "limit-exceeded"),
        Some(OxideError::AlreadyExists(_)) => (6, "already-exists"),
        Some(OxideError::Cancelled) => (130, "cancelled"),
        Some(OxideError::UnknownFormat(_)) => (1, "unknown-format"),
        Some(OxideError::Unsupported { .. } | OxideError::FeatureDisabled { .. }) => (1, "unsupported"),
        Some(OxideError::InvalidOption(_)) => (2, "invalid-option"),
        Some(OxideError::Io { .. }) => (1, "io"),
        _ => (1, "error"),
    }
}

fn hint(err: &anyhow::Error) -> Option<&'static str> {
    match err.downcast_ref::<OxideError>()? {
        OxideError::AlreadyExists(_) => Some("use --force (create) or --overwrite skip|overwrite|rename (extract)"),
        OxideError::PasswordRequired => Some("pass --password or --password-env"),
        OxideError::LimitExceeded(_) => {
            Some("raise --max-size/--max-entries/--max-ratio, or --no-limits for trusted archives")
        }
        _ => None,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match (cli.quiet, cli.verbose) {
        (true, _) => "error",
        (_, 0) => "warn",
        (_, 1) => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("OXIDE_LOG").unwrap_or_else(|_| level.into()))
        .with_writer(std::io::stderr)
        .without_time()
        .init();

    let cancel = CancelToken::new();
    let c = cancel.clone();
    // First Ctrl+C cancels cleanly (temporary files are removed); a second one exits immediately.
    let _ = ctrlc::set_handler(move || {
        if c.is_cancelled() {
            std::process::exit(130);
        }
        c.cancel();
    });

    let json = cli.json;
    match run(cli, &cancel) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let (code, kind) = exit_code(&err);
            if json {
                println!("{}", json!({ "ok": false, "error": format!("{err:#}"), "kind": kind }));
            } else {
                eprintln!("error: {err:#}");
                if let Some(h) = hint(&err) {
                    eprintln!("hint: {h}");
                }
            }
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("10K"), Ok(10 * 1024));
        assert_eq!(parse_size("1.5G"), Ok(3 << 29));
        assert_eq!(parse_size("2MiB"), Ok(2 << 20));
        assert!(parse_size("12Q").is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn errors_map_to_exit_codes() {
        assert_eq!(exit_code(&anyhow!(OxideError::Cancelled)).0, 130);
        assert_eq!(exit_code(&anyhow!(OxideError::Corrupt("x".into()))).0, 3);
        assert_eq!(exit_code(&anyhow!("plain")).0, 1);
    }
}
