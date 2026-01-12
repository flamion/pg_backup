use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::{error, info, warn};

/// Configuration defaults
struct Config;

impl Config {
    const OUTPUT_DIR: &'static str = "./pg_backups";
    const PG_HOST: &'static str = "localhost";
    const PG_PORT: &'static str = "5432";
    const PG_USER: &'static str = "postgres";
    const ZSTD_COMPRESSION_LEVEL: u8 = 18;
    const ZSTD_THREADS: u8 = 0; // 0 = use all available threads
    const DATETIME_FORMAT: &'static str = "%Y-%m-%d_%H-%M-%S";
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Specific database to backup (optional, backs up all non-system DBs if omitted)
    #[arg(short, long)]
    database: Option<String>,

    #[arg(short, long, default_value = Config::OUTPUT_DIR)]
    output_dir: PathBuf,

    /// PostgreSQL host
    #[arg(short = 'H', long, default_value = Config::PG_HOST)]
    host: String,

    /// PostgreSQL port
    #[arg(short, long, default_value = Config::PG_PORT)]
    port: String,

    /// PostgreSQL username
    #[arg(short = 'U', long, default_value = Config::PG_USER)]
    username: String,

    /// Compress backups using zstd
    #[arg(long, short, default_value_t = false, default_missing_value = "true", num_args = 0..=1)]
    compress: bool,
}

fn generate_timestamp_string() -> String {
    let now = Local::now();
    format!(
        "{}",
        now.format(Config::DATETIME_FORMAT),
    )
}

fn check_zstd_available() -> bool {
    Command::new("zstd")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn should_add_host_arg(host: &str) -> bool {
    host != "localhost" 
    && host != "127.0.0.1" 
    && host != "::1"
    && !host.is_empty()
}

/// Gets list of non-system databases
fn get_databases(host: &str, port: &str, user: &str) -> Result<Vec<String>> {
    let query = "SELECT datname FROM pg_database \
                 WHERE datistemplate = false \
                 AND datname NOT IN ('postgres');";

    info!("Fetching list of databases...");

    let mut cmd = Command::new("psql");

    if should_add_host_arg(host) {
        cmd.args(["-h", host]);
    }

    cmd.args(["-p", port, "-U", user, "-t", "-c", query]);

    let output = cmd
        .output()
        .context("Failed to execute psql command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to fetch database list: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let databases: Vec<String> = stdout
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    Ok(databases)
}

fn validate_or_create_output_dir(output_dir: &Path) -> Result<()> {
    if !output_dir.exists() {
        info!("Creating output directory: {}", output_dir.display());
        std::fs::create_dir_all(output_dir).context("Failed to create output directory")?;
    } else if output_dir.read_dir()?.next().is_some() {
        // Directory exists and is not empty
        let non_backup_files: Vec<_> = std::fs::read_dir(output_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.path().is_file()
                    && !matches!(
                        entry.path().extension().and_then(|s| s.to_str()),
                        Some("sql") | Some("zst")
                    )
            })
            .collect();

        if !non_backup_files.is_empty() {
            error!(
                "Output directory '{}' contains non-backup files.",
                output_dir.display()
            );
            error!("Please use an empty directory or one containing only .sql or .zst files.");
            anyhow::bail!("Invalid output directory");
        }
        info!("Output directory exists and contains only backup files. Proceeding...");
    }

    Ok(())
}

/// Backup a single database with optional compression
fn backup_database(
    db_name: &str,
    output_dir: &Path,
    timestamp: &str,
    host: &str,
    port: &str,
    user: &str,
    compress: bool,
) -> Result<()> {
    // Determine output filename
    let extension = if compress { "sql.zst" } else { "sql" };
    let output_file = output_dir.join(format!("{}-{}.{}", db_name, timestamp, extension));

    if compress {
        info!("Backing up database: {} (with zstd compression)", db_name);
    } else {
        info!("Backing up database: {}", db_name);
    }

    // Base pg_dump command
    let mut pg_dump_cmd = Command::new("pg_dump");

    if should_add_host_arg(host) {
        pg_dump_cmd.args(["-h", host]);
    }

    pg_dump_cmd.args(["-p", port, "-U", user, "-d", db_name, "-F", "p"]);

    if compress {
        // Pipe pg_dump output to zstd
        pg_dump_cmd.stdout(Stdio::piped());

        let mut pg_dump_child = pg_dump_cmd
            .spawn()
            .context("Failed to spawn pg_dump process")?;

        let pg_dump_stdout = pg_dump_child
            .stdout
            .take()
            .context("Failed to capture pg_dump stdout")?;

        let zstd_cmd = Command::new("zstd")
            .args([
                "-",
                "-o",
                output_file.to_str().unwrap(),
                &format!("-T{}", Config::ZSTD_THREADS),
                "--ultra",
                &format!("-{}", Config::ZSTD_COMPRESSION_LEVEL),
            ])
            .stdin(Stdio::from(pg_dump_stdout))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn zstd process")?;

        let pg_dump_status = pg_dump_child.wait().context("Failed to wait for pg_dump")?;
        let zstd_output = zstd_cmd
            .wait_with_output()
            .context("Failed to wait for zstd")?;

        if !pg_dump_status.success() {
            anyhow::bail!("pg_dump failed for database: {}", db_name);
        }

        if !zstd_output.status.success() {
            let stderr = String::from_utf8_lossy(&zstd_output.stderr);
            anyhow::bail!("zstd compression failed: {}", stderr);
        }
    } else {
        // Standard uncompressed backup
        pg_dump_cmd.args(["-f", output_file.to_str().unwrap()]);

        let output = pg_dump_cmd.output().context("Failed to execute pg_dump")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("pg_dump failed: {}", stderr);
        }
    }

    // Log success with file size
    let metadata =
        std::fs::metadata(&output_file).context("Failed to read output file metadata")?;
    let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);

    info!(
        "Successfully backed up {} to {}",
        db_name,
        output_file.file_name().unwrap().to_str().unwrap()
    );
    info!("Size: {:.2} MB", size_mb);

    Ok(())
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    let args = Args::parse();

    // Check for zstd if compression is requested
    if args.compress {
        if !check_zstd_available() {
            error!("zstd is not available. Please install zstd or run without --compress flag.");
            anyhow::bail!("zstd not found");
        }
        info!("zstd compression enabled");
    }

    let timestamp = generate_timestamp_string();

    validate_or_create_output_dir(&args.output_dir)?;

    let mut success_count = 0;
    let mut fail_count = 0;

    if let Some(db_name) = args.database {
        // Backup specific database
        info!("Backing up specific database: {}", db_name);
        match backup_database(
            &db_name,
            &args.output_dir,
            &timestamp,
            &args.host,
            &args.port,
            &args.username,
            args.compress,
        ) {
            Ok(_) => success_count += 1,
            Err(e) => {
                error!("Failed to backup {}: {}", db_name, e);
                fail_count += 1;
            }
        }
    } else {
        // Backup all non-system databases
        let databases = get_databases(&args.host, &args.port, &args.username)?;

        if databases.is_empty() {
            warn!("No user databases found to backup.");
            return Ok(());
        }

        info!("Found {} database(s) to backup:", databases.len());
        for db in &databases {
            info!("  - {}", db);
        }
        println!();

        // Backup each database
        for db in databases {
            match backup_database(
                &db,
                &args.output_dir,
                &timestamp,
                &args.host,
                &args.port,
                &args.username,
                args.compress,
            ) {
                Ok(_) => success_count += 1,
                Err(e) => {
                    error!("Failed to backup {}: {}", db, e);
                    fail_count += 1;
                }
            }
            println!();
        }
    }

    // Print summary
    info!("{}", "=".repeat(50));
    info!("Backup Summary:");
    info!("  Successful: {}", success_count);
    info!("  Failed: {}", fail_count);
    info!(
        "  Output directory: {}",
        args.output_dir.canonicalize()?.display()
    );
    if args.compress {
        info!(
            "  Compression: zstd (level {})",
            Config::ZSTD_COMPRESSION_LEVEL
        );
    }
    info!("{}", "=".repeat(50));

    // Exit with error code if any backups failed
    if fail_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}
