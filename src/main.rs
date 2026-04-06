use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime, TimeZone};
use clap::{Parser, Subcommand, ValueEnum};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;

// ── CLI Definition ─────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "tt", about = "A simple time tracker", version)]
struct Cli {
    /// Path to the SQLite database file (default: ~/.timetrack.db)
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a new time tracking session
    Start,

    /// Stop the current session and print elapsed time
    Stop {
        /// Optional end time (format: hh:mm)
        #[arg(long)]
        time: Option<NaiveTime>,
    },

    /// Show elapsed time (defaults to current session, or today if none active)
    Status {
        #[arg(value_enum, default_value = "auto")]
        scope: StatusScope,
    },

    /// Show history between two dates (format: YYYY-MM-DD)
    History {
        /// Start date (inclusive), e.g. 2025-01-01
        from: NaiveDate,
        /// End date (inclusive), e.g. 2025-01-31
        to: NaiveDate,
    },
}

#[derive(ValueEnum, Clone, Debug)]
enum StatusScope {
    /// Current session if active, otherwise today
    Auto,
    /// Current running session
    Session,
    /// All sessions today
    Today,
    /// All sessions this week (Mon–Sun)
    Week,
    /// All sessions this month
    Month,
}

// ── Database ───────────────────────────────────────────────────────────────────

fn open_db(path: &PathBuf) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("Cannot open database at {}", path.display()))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at INTEGER NOT NULL,
            stopped_at INTEGER
        );",
    )?;

    Ok(conn)
}

fn default_db_path() -> PathBuf {
    let mut p = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    p.push(".timetrack.db");
    p
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn now_unix() -> i64 {
    Local::now().timestamp()
}

fn format_duration(secs: i64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{}h {:02}m {:02}s", h, m, s)
    } else if m > 0 {
        format!("{}m {:02}s", m, s)
    } else {
        format!("{}s", s)
    }
}

fn decimal_hours(secs: i64) -> f64 {
    secs as f64 / 3600.0
}

fn day_bounds(date: NaiveDate) -> (i64, i64) {
    let start = Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .unwrap()
        .timestamp();
    let end = Local
        .from_local_datetime(&date.and_hms_opt(23, 59, 59).unwrap())
        .unwrap()
        .timestamp();
    (start, end)
}

fn local_from_unix(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).unwrap()
}

// ── Commands ───────────────────────────────────────────────────────────────────

fn cmd_start(conn: &Connection) -> Result<()> {
    let active: Option<i64> = conn
        .query_row(
            "SELECT id FROM sessions WHERE stopped_at IS NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = active {
        let started: i64 = conn.query_row(
            "SELECT started_at FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        let dt = local_from_unix(started);
        bail!(
            "A session is already running (started at {}). Run `tt stop` first.",
            dt.format("%H:%M:%S")
        );
    }

    let ts = now_unix();
    conn.execute("INSERT INTO sessions (started_at) VALUES (?1)", params![ts])?;

    println!(
        "▶  Session started at {}",
        local_from_unix(ts).format("%H:%M:%S")
    );
    Ok(())
}

fn cmd_stop(conn: &Connection, time: Option<NaiveTime>) -> Result<()> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, started_at FROM sessions WHERE stopped_at IS NULL LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let (id, started_at) = match row {
        Some(r) => r,
        None => bail!("No active session. Run `tt start` to begin tracking."),
    };

    let ts = match time {
        Some(p_time) => {
            let datetime = Local::now().date_naive().and_time(p_time);
            Local
                .from_local_datetime(&datetime)
                .single()
                .unwrap()
                .timestamp()
        }
        None => now_unix(),
    };

    conn.execute(
        "UPDATE sessions SET stopped_at = ?1 WHERE id = ?2",
        params![ts, id],
    )?;

    let elapsed = ts - started_at;
    println!("■  Session stopped.");
    println!(
        "   Started:  {}",
        local_from_unix(started_at).format("%H:%M:%S")
    );
    println!("   Stopped:  {}", local_from_unix(ts).format("%H:%M:%S"));
    println!(
        "   Duration: {} ({:.2}h)",
        format_duration(elapsed),
        decimal_hours(elapsed)
    );
    Ok(())
}

fn cmd_status(conn: &Connection, scope: StatusScope) -> Result<()> {
    let now = Local::now();
    let today = now.date_naive();

    let effective_scope = match scope {
        StatusScope::Auto => {
            let has_active: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE stopped_at IS NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)?;
            if has_active {
                StatusScope::Session
            } else {
                StatusScope::Today
            }
        }
        other => other,
    };

    match effective_scope {
        StatusScope::Session => {
            let row: Option<(i64, i64)> = conn
                .query_row(
                    "SELECT id, started_at FROM sessions WHERE stopped_at IS NULL LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;

            match row {
                None => println!("No active session."),
                Some((_id, started_at)) => {
                    let elapsed = now.timestamp() - started_at;
                    println!(
                        "● Active session — started at {}",
                        local_from_unix(started_at).format("%H:%M:%S")
                    );
                    println!(
                        "  Elapsed: {} ({:.2}h)",
                        format_duration(elapsed),
                        decimal_hours(elapsed)
                    );
                }
            }
        }

        StatusScope::Today => {
            let (start_ts, end_ts) = day_bounds(today);
            print_range_summary(conn, "Today", start_ts, end_ts, now.timestamp())?;
        }

        StatusScope::Week => {
            let weekday_from_mon = today.weekday().num_days_from_monday() as i64;
            let monday = today - chrono::Duration::days(weekday_from_mon);
            let (start_ts, _) = day_bounds(monday);
            let (_, end_ts) = day_bounds(today);
            print_range_summary(conn, "This week", start_ts, end_ts, now.timestamp())?;
        }

        StatusScope::Month => {
            let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
            let (start_ts, _) = day_bounds(first);
            let (_, end_ts) = day_bounds(today);
            print_range_summary(conn, "This month", start_ts, end_ts, now.timestamp())?;
        }

        StatusScope::Auto => unreachable!(),
    }

    Ok(())
}

fn cmd_history(conn: &Connection, from: NaiveDate, to: NaiveDate) -> Result<()> {
    if from > to {
        bail!("Start date must be before or equal to end date.");
    }
    let (start_ts, _) = day_bounds(from);
    let (_, end_ts) = day_bounds(to);
    let label = format!("{} → {}", from.format("%Y-%m-%d"), to.format("%Y-%m-%d"));
    print_range_summary(conn, &label, start_ts, end_ts, Local::now().timestamp())?;
    Ok(())
}

fn print_range_summary(
    conn: &Connection,
    label: &str,
    start_ts: i64,
    end_ts: i64,
    now_ts: i64,
) -> Result<()> {
    let completed_secs: i64 = conn.query_row(
        "SELECT COALESCE(SUM(
            MIN(stopped_at, ?2) - MAX(started_at, ?1)
         ), 0)
         FROM sessions
         WHERE stopped_at IS NOT NULL
           AND started_at <= ?2
           AND stopped_at  >= ?1",
        params![start_ts, end_ts],
        |row| row.get(0),
    )?;

    let running_secs: i64 = conn.query_row(
        "SELECT COALESCE(SUM(
            MIN(?3, ?2) - MAX(started_at, ?1)
         ), 0)
         FROM sessions
         WHERE stopped_at IS NULL
           AND started_at <= ?2",
        params![start_ts, end_ts, now_ts],
        |row| row.get(0),
    )?;

    let total = completed_secs + running_secs;

    println!(
        "{}  —  {:.2}h  ({})",
        label,
        decimal_hours(total),
        format_duration(total)
    );

    if running_secs > 0 {
        println!("  ● Includes a currently active session.");
    }

    Ok(())
}

// ── Entry point ────────────────────────────────────────────────────────────────

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db_path);
    let conn = open_db(&db_path)?;

    match cli.command {
        Command::Start => cmd_start(&conn),
        Command::Stop { time } => cmd_stop(&conn, time),
        Command::Status { scope } => cmd_status(&conn, scope),
        Command::History { from, to } => cmd_history(&conn, from, to),
    }
}
