use clap::Parser;
use jetdb::{
    find_table, read_catalog, read_table_def, read_table_rows, table_names, PageReader, Value,
};
use std::path::PathBuf;
use std::process;

/// 免驱动 Access (.mdb / .accdb) 数据库命令行检索工具
#[derive(Parser, Debug)]
#[command(name = "mdb-search", version, about)]
struct Cli {
    /// Access 数据库文件路径 (.mdb 或 .accdb)
    database: PathBuf,

    /// 要查找的数值或字符串
    #[arg(short = 'q', long = "query")]
    query: String,

    /// 指定要搜索的字段名（可省略，支持多次指定或逗号分隔，如: -f Code -f DE 或 -f Code,DE）
    #[arg(short = 'f', long = "field", value_delimiter = ',')]
    fields: Vec<String>,

    /// 仅在指定的数据表中搜索（可省略，支持多次指定或逗号分隔）
    #[arg(short = 't', long = "table", value_delimiter = ',')]
    tables: Vec<String>,

    /// 启用全匹配（默认为包含匹配）
    #[arg(short = 'e', long = "exact")]
    exact: bool,

    /// 忽略大小写（仅针对查找内容；字段名和表名默认不区分大小写）
    #[arg(short = 'i', long = "ignore-case")]
    ignore_case: bool,

    /// 打印命中记录的整行非空字段
    #[arg(long = "full")]
    full: bool,

    /// 限制最多输出的命中条数（0 表示不限制）
    #[arg(short = 'n', long = "limit", default_value_t = 0)]
    limit: usize,

    /// 加密数据库密码（如有）
    #[arg(short = 'p', long = "password")]
    password: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    if let Err(err) = run(&cli) {
        eprintln!("错误: {err}");
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
                eprintln!("警告: 跳过无法解析结构的表 [{}]: {}", entry.name, e);
                continue;
            }
        };

        // 确定需要扫描的列索引（0 个字段参数时扫描全部列）
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

        // 若指定了字段名且当前表不包含任意一个指定字段，直接跳过读行
        if col_indices.is_empty() {
            continue;
        }

        scanned_tables += 1;

        let result = match read_table_rows(&mut reader, &table_def) {
            Ok(res) => res,
            Err(e) => {
                eprintln!("警告: 读取表 [{}] 数据失败: {}", entry.name, e);
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
                print!("[{}] 行 #{}: ", entry.name, row_idx + 1);
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
                    println!("    整行: {}", full_cols.join(" | "));
                }

                if cli.limit > 0 && total_matches >= cli.limit {
                    println!(
                        "\n--- 已达输出上限 ({})，共扫描 {} 张表 ---",
                        cli.limit, scanned_tables
                    );
                    return Ok(());
                }
            }
        }
    }

    println!(
        "\n--- 搜索完成：共扫描 {} 张表，命中 {} 条记录 ---",
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

/// 将 OLE Automation Date (1899-12-30 起的天数) 转为可读日期时间
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

    // 1899-12-30 到 1970-01-01 相差 25569 天
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