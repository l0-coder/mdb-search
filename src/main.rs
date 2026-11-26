use clap::Parser;
use jetdb::{
    find_table, read_catalog, read_table_def, read_table_rows, table_names, PageReader, Value,
};
use std::path::PathBuf;
use std::process;

const EXAMPLES: &str = "\
Examples:
  # 1. Search all tables and all fields (substring match, case-insensitive)
  mdb-search SServda.accdb -q \"Fehler\" -i

  # 2. Search specific fields (comma-separated or repeated -f) with exact match
  mdb-search SServda.accdb -f Code,MNr -q 218 --exact
  mdb-search SServda.accdb -f Code -f MNr -q 218 -e --full

  # 3. Restrict search to specific table(s) and limit output to 20 matches
  mdb-search SServda.accdb -t MeldungsTexte -f DE,EN -q \"bruch\" -i -n 20

  # 4. Search a password-protected Access database
  mdb-search protected.accdb -p \"secret\" -q \"1001\" -e";

#[derive(Parser, Debug)]
#[command(
    name = "mdb-search",
    version,
    about = "Zero-dependency CLI tool to search values in Microsoft Access (.mdb / .accdb) databases",
    after_help = EXAMPLES,
    arg_required_else_help = true
)]
struct Cli {
    /// Path to the Access database file (.mdb or .accdb)
    #[arg(value_name = "FILE")]
    database: PathBuf,

    /// Value or string to search for
    #[arg(short = 'q', long = "query", value_name = "VALUE")]
    query: String,

    /// Field/column name(s) to search; omit to search all fields (repeatable or comma-separated)
    #[arg(short = 'f', long = "field", value_delimiter = ',', value_name = "FIELDS")]
    fields: Vec<String>,

    /// Restrict search to specific table(s); omit to search all user tables (repeatable or comma-separated)
    #[arg(short = 't', long = "table", value_delimiter = ',', value_name = "TABLES")]
    tables: Vec<String>,

    /// Match the entire field value exactly (default: substring/contains match)
    #[arg(short = 'e', long = "exact")]
    exact: bool,

    /// Perform case-insensitive value matching (field and table names are always case-insensitive)
    #[arg(short = 'i', long = "ignore-case")]
    ignore_case: bool,

    /// Print all non-empty columns of each matched row
    #[arg(long = "full")]
    full: bool,

    /// Maximum number of matched rows to display (0 = unlimited)
    #[arg(short = 'n', long = "limit", default_value_t = 0, value_name = "COUNT")]
    limit: usize,

    /// Password for encrypted Access databases
    #[arg(short = 'p', long = "password", value_name = "PASSWORD")]
    password: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    if let Err(err) = run(&cli) {
        eprintln!("Error: {err}");
        process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let mut reader = match &cli.password {
        Some(pw) => PageReader::open_with_password(&cli.database, Some(pw.as_str()))?,
        None => PageReader::open(&cli.database)?,
    };

    let all_tables = table_names(&mut reader)?;
    let catalog = read_catalog(&mut reader)?;

    let target_tables: Vec<String> = cli.tables.iter().map(|s| s.to_lowercase()).collect();
    let target_fields: Vec<String> = cli.fields.iter().map(|s| s.to_lowercase()).collect();
    let needle = if cli.ignore_case {
        cli.query.to_lowercase()
    } else {
        cli.query.clone()
    };

    let mut total_matches: usize = 0;
    let mut scanned_tables: usize = 0;

    for tbl_name in &all_tables {
        if !target_tables.is_empty() && !target_tables.contains(&tbl_name.to_lowercase()) {
            continue;
        }

        let entry = match find_table(&catalog, tbl_name) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let table_def = match read_table_def(&mut reader, &entry.name, entry.table_page) {
            Ok(def) => def,
            Err(e) => {
                eprintln!("Warning: skipped table [{}] (failed to read schema): {}", entry.name, e);
                continue;
            }
        };

        // Determine target column indices (0 fields specified -> scan all columns)
        let col_indices: Vec<usize> = if target_fields.is_empty() {
            (0..table_def.columns.len()).collect()
        } else {
            table_def
                .columns
                .iter()
                .enumerate()
                .filter(|(_, col)| target_fields.contains(&col.name.to_lowercase()))
                .map(|(idx, _)| idx)
                .collect()
        };

        // Skip reading rows if none of the requested fields exist in this table
        if col_indices.is_empty() {
            continue;
        }

        scanned_tables += 1;

        let result = match read_table_rows(&mut reader, &table_def) {
            Ok(res) => res,
            Err(e) => {
                eprintln!("Warning: failed to read rows from table [{}]: {}", entry.name, e);
                continue;
            }
        };

        for (row_idx, row) in result.rows.iter().enumerate() {
            let mut row_hits: Vec<(&str, String)> = Vec::new();

            for &col_idx in &col_indices {
                let Some(val) = row.get(col_idx) else {
                    continue;
                };
                let Some(val_str) = value_to_string(val) else {
                    continue;
                };

                if is_match(&val_str, &needle, cli.exact, cli.ignore_case) {
                    let col_name = table_def.columns[col_idx].name.as_str();
                    row_hits.push((col_name, val_str));
                }
            }

            if !row_hits.is_empty() {
                total_matches += 1;
                print!("[{}] Row #{}: ", entry.name, row_idx + 1);
                let hit_summary: Vec<String> = row_hits
                    .iter()
                    .map(|(k, v)| format!("{k} = {v}"))
                    .collect();
                println!("{}", hit_summary.join(" | "));

                if cli.full {
                    let full_cols: Vec<String> = table_def
                        .columns
                        .iter()
                        .enumerate()
                        .filter_map(|(i, col)| {
                            row.get(i)
                                .and_then(value_to_string)
                                .map(|v| format!("{}={}", col.name, v))
                        })
                        .collect();
                    println!("    Full row: {}", full_cols.join(" | "));
                }

                if cli.limit > 0 && total_matches >= cli.limit {
                    println!(
                        "\n--- Reached output limit ({}) after scanning {} table(s) ---",
                        cli.limit, scanned_tables
                    );
                    return Ok(());
                }
            }
        }
    }

    println!(
        "\n--- Search complete: scanned {} table(s), found {} match(es) ---",
        scanned_tables, total_matches
    );
    Ok(())
}

fn is_match(value: &str, needle: &str, exact: bool, ignore_case: bool) -> bool {
    if ignore_case {
        let lower = value.to_lowercase();
        if exact {
            lower == needle
        } else {
            lower.contains(needle)
        }
    } else if exact {
        value == needle
    } else {
        value.contains(needle)
    }
}

fn value_to_string(val: &Value) -> Option<String> {
    match val {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Byte(n) => Some(n.to_string()),
        Value::Int(n) => Some(n.to_string()),
        Value::Long(n) => Some(n.to_string()),
        Value::BigInt(n) => Some(n.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(f) => Some(f.to_string()),
        Value::Text(s) => Some(s.clone()),
        Value::Money(s) => Some(s.clone()),
        Value::Numeric(s) => Some(s.clone()),
        Value::Guid(s) => Some(s.clone()),
        Value::DateTimeExtended(s) => Some(s.clone()),
        Value::Timestamp(days) => Some(format_ole_date(*days)),
        Value::Binary(bytes) => Some(format!("<Binary: {} bytes>", bytes.len())),
        #[allow(unreachable_patterns)]
        other => Some(format!("{other:?}")),
    }
}

/// Format OLE Automation Date (days since 1899-12-30) into YYYY-MM-DD[ HH:MM:SS]
fn format_ole_date(days: f64) -> String {
    if !days.is_finite() {
        return days.to_string();
    }
    let mut whole_days = days.trunc() as i64;
    let mut total_secs = (days.fract().abs() * 86_400.0).round() as u32;
    if total_secs >= 86_400 {
        total_secs = 0;
        whole_days += if days >= 0.0 { 1 } else { -1 };
    }

    // Days between 1899-12-30 and Unix epoch 1970-01-01 = 25569
    let (year, month, day) = civil_from_days(whole_days - 25_569);
    if total_secs == 0 {
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        let h = total_secs / 3600;
        let m = (total_secs % 3600) / 60;
        let s = total_secs % 60;
        format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}")
    }
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}