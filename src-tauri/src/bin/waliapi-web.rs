//! waliapi-web：headless 服务模式（无桌面窗口），供 Docker / 无显示 Linux 部署。
//!
//! 用法：
//!   waliapi-web [--host 0.0.0.0] [--port 8777] [--data-dir /data]   （默认即启动服务）
//!   waliapi-web start [...同上参数...]
//!   waliapi-web repair-stream-logs [--data-dir <目录>] [--apply] [--limit <行>]
//!
//! 环境变量：WALIAPI_SERVER_HOST / WALIAPI_SERVER_PORT / WALIAPI_DATA_DIR / XDG_DATA_HOME

use waliapi_lib::web_server::{resolve_data_dir, run, WebServerConfig};
use tracing_subscriber::prelude::*;

fn print_usage() {
    println!(
        "waliapi-web — WaLiAPI headless 服务模式（LLM 网关 + Web 管理面板）

用法:
  waliapi-web [start] [--host <地址>] [--port <端口>] [--data-dir <目录>]
  waliapi-web repair-stream-logs [--data-dir <目录>] [--apply] [--limit <行>]

说明:
  不带任何参数直接启动服务（start 为可选子命令，语义相同）。

选项:
  --host       监听地址（默认读取 WALIAPI_SERVER_HOST 或设置，缺省 127.0.0.1）
  --port       监听端口（默认读取 WALIAPI_SERVER_PORT 或设置，缺省 8777）
  --data-dir   数据目录（默认读取 WALIAPI_DATA_DIR / XDG_DATA_HOME，再缺省为平台应用数据目录）
  -h, --help   显示帮助

子命令 repair-stream-logs:
  一次性修复历史上被误记为 499 / client_cancelled 且 token 用量为 0 的流式日志。
  默认 dry-run 只报告不改库，加 --apply 才写入。
    --apply         真正写库（缺省为 dry-run）
    --limit <行>    每批处理行数（缺省 50，命令内部自动分批直到处理完）
    --status-only   只改判状态（499→200），不补 token。只做消息前缀比对，秒级完成
    --usage-only    只补 token 用量与响应内容，保留 499 状态。需跑 BPE，大库分钟级
  缺省两者都做。--status-only 与 --usage-only 互斥。

  ⚠ 请先停止正在使用同一数据目录的实例再执行。WaLiAPI 的 SQLite 以默认
    journal_mode=delete（回滚日志）打开，写事务提交需要 EXCLUSIVE 锁；本命令
    会连续发起数百个写事务，与仍在服务的网关（每个请求至少一次读 + 一次写）
    争锁，两侧都会遇到 SQLITE_BUSY 停顿。首次执行还会触发 schema 迁移，
    迁移前的 VACUUM INTO 备份需要整库一致性快照，在 GB 级库上尤其不适合与
    在线写入并行。
"
    );
}

/// `repair-stream-logs` 子命令：打开数据目录 → 跑迁移 → 分批修复直到收敛。
async fn run_repair_cli(
    rest: &[String],
    apply: bool,
    limit: i64,
    reclassify: bool,
    backfill_usage: bool,
) -> Result<(), String> {
    let data_dir = resolve_data_dir(
        rest.iter()
            .position(|a| a == "--data-dir")
            .and_then(|i| rest.get(i + 1).cloned()),
    );
    println!("数据目录: {}", data_dir.display());
    let db = waliapi_lib::db::Database::new_with_path(&data_dir).await;
    let pool = db.pool.clone();

    let mut total_scanned = 0i64;
    let mut total_reclassified = 0i64;
    let mut total_kept = 0i64;
    let mut total_prompt = 0i64;
    let mut total_completion = 0i64;
    let mut rounds = 0i64;
    loop {
        let report = waliapi_lib::commands::log_repair::repair_pool(
            &pool,
            waliapi_lib::commands::log_repair::RepairInput {
                apply: Some(apply),
                limit: Some(limit),
                reclassify: Some(reclassify),
                backfill_usage: Some(backfill_usage),
            },
        )
        .await?;
        rounds += 1;
        total_scanned += report.scanned;
        total_reclassified += report.reclassified;
        total_kept += report.kept_cancelled;
        total_prompt += report.prompt_tokens_added;
        total_completion += report.completion_tokens_added;
        println!(
            "[第 {rounds} 批] 扫描={} 改判200={} 保持499={} prompt+={} completion+={} \
             恢复响应={} 剩余={}",
            report.scanned,
            report.reclassified,
            report.kept_cancelled,
            report.prompt_tokens_added,
            report.completion_tokens_added,
            report.response_choices_restored,
            report.remaining
        );
        for sample in report.samples.iter().take(3) {
            println!(
                "    seq={} delivered={} prompt={}({}) completion={} resp_chars={}",
                sample.seq,
                sample.delivered,
                sample.prompt_tokens,
                serde_json::to_string(&sample.prompt_method).unwrap_or_default(),
                sample.completion_tokens,
                sample.restored_response_chars
            );
        }
        // dry-run 不写库，remaining 永远不会收敛；只跑一批给人看影响面，
        // 否则这里会死循环。apply 才按批推进直到处理完。
        if !apply || report.scanned == 0 || report.remaining == 0 {
            break;
        }
    }

    println!(
        "\n{}：共扫描 {} 行（{} 批），改判 200 共 {} 行，保持 499 共 {} 行，\n补记 prompt {} tokens、completion {} tokens。",
        if apply { "已应用" } else { "dry-run（未写库）" },
        total_scanned,
        rounds,
        total_reclassified,
        total_kept,
        total_prompt,
        total_completion
    );
    if !apply {
        println!("确认无误后加 --apply 真正写入。");
    }
    pool.close().await;
    Ok(())
}

fn parse_args(args: &[String]) -> Result<WebServerConfig, String> {
    let mut host = None;
    let mut port = None;
    let mut data_dir = None;
    let mut i = 0;
    while i < args.len() {
        let value_of = |i: &mut usize, name: &str| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{name} 缺少参数值"))
        };
        match args[i].as_str() {
            "--host" => host = Some(value_of(&mut i, "--host")?),
            "--port" => {
                let raw = value_of(&mut i, "--port")?;
                port = Some(
                    raw.trim()
                        .parse::<u16>()
                        .map_err(|_| format!("--port 无效: {raw}"))?,
                );
            }
            "--data-dir" => data_dir = Some(value_of(&mut i, "--data-dir")?),
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    Ok(WebServerConfig {
        host,
        port,
        data_dir: resolve_data_dir(data_dir),
    })
}

#[tokio::main]
async fn main() {
    // 日志目录：优先数据目录下（容器内 /data/logs，waliapi 用户有写权限），
    // 回退到可执行文件同级 logs/。
    let data_log_dir = std::env::var("WALIAPI_DATA_DIR")
        .ok()
        .map(|d| std::path::PathBuf::from(d.trim()).join("logs"));
    let exe_log_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|parent| parent.join("logs")))
        .unwrap_or_else(|| std::path::PathBuf::from("logs"));
    let log_dir = data_log_dir.unwrap_or_else(|| exe_log_dir.clone());
    std::fs::create_dir_all(&log_dir).ok();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);
    // 维护子命令要给人读结论，不能被 sqlx 的 DEBUG 语句淹掉；服务模式日志级别保持不变。
    let maintenance = first == Some("repair-stream-logs");
    let max_level = if maintenance {
        tracing::level_filters::LevelFilter::WARN
    } else {
        tracing::level_filters::LevelFilter::DEBUG
    };

    // 按天滚动日志文件（如 waliapi.log.2026-08-25），最多保留 7 个文件
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("waliapi.log")
        .max_log_files(7)
        .build(&log_dir)
        .ok();

    // 同时输出到 stdout（docker logs 可见）和日志文件。
    // 文件写入失败不影响 stdout，保证容器日志始终可见。
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_filter(max_level);
    let file_layer = file_appender.map(|w| {
        tracing_subscriber::fmt::layer()
            .with_writer(w)
            .with_filter(max_level)
    });
    tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .init();

    // 帮助优先于参数路由：-h/--help（含 start 子命令后）打印用法并正常退出
    let help_requested = matches!(first, Some("-h") | Some("--help"))
        || (first == Some("start")
            && matches!(args.get(1).map(String::as_str), Some("-h") | Some("--help")));
    if help_requested {
        print_usage();
        return;
    }
    // repair-stream-logs 是离线维护子命令：不开服务、不需要管理员会话，跑完即退出
    if first == Some("repair-stream-logs") {
        let rest = &args[1..];
        let apply = rest.iter().any(|a| a == "--apply");
        let limit = match rest.iter().position(|a| a == "--limit") {
            Some(idx) => match rest.get(idx + 1).map(|raw| raw.trim().parse::<i64>()) {
                Some(Ok(v)) if v > 0 => Ok(v),
                Some(Ok(_)) => Err("--limit 必须为正整数".to_string()),
                Some(Err(e)) => Err(format!("--limit 无效: {e}")),
                None => Err("--limit 缺少参数值".to_string()),
            },
            None => Ok(50),
        };
        let limit = match limit {
            Ok(v) => v,
            Err(e) => {
                eprintln!("参数错误: {e}\n");
                print_usage();
                std::process::exit(2);
            }
        };
        let scope = match (
            rest.iter().any(|a| a == "--status-only"),
            rest.iter().any(|a| a == "--usage-only"),
        ) {
            (true, true) => Err("--status-only 与 --usage-only 互斥".to_string()),
            (true, false) => Ok((true, false)),
            (false, true) => Ok((false, true)),
            (false, false) => Ok((true, true)),
        };
        let (reclassify, backfill_usage) = match scope {
            Ok(v) => v,
            Err(e) => {
                eprintln!("参数错误: {e}\n");
                print_usage();
                std::process::exit(2);
            }
        };
        if let Err(e) = run_repair_cli(rest, apply, limit, reclassify, backfill_usage).await {
            eprintln!("修复失败: {e}");
            std::process::exit(1);
        }
        return;
    }
    // 不带参数、直接带选项（waliapi-web --port 9000）、或显式 start 子命令，均启动服务
    let start_args: Option<&[String]> = match first {
        None => Some(&[]),
        Some("start") => Some(&args[1..]),
        Some(flag) if flag.starts_with("--") => Some(&args[..]),
        _ => None,
    };
    match start_args {
        Some(rest) => {
            let cfg = match parse_args(rest) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("参数错误: {e}\n");
                    print_usage();
                    std::process::exit(2);
                }
            };
            tracing::info!("数据目录: {}", cfg.data_dir.display());
            if let Err(e) = run(cfg).await {
                eprintln!("服务异常退出: {e}");
                std::process::exit(1);
            }
        }
        None => {
            // 帮助已在上方拦截；到这里的只会是非选项的未知子命令
            let other = first.expect("start_args 为 None 时必存在首参数");
            eprintln!("未知命令: {other}\n");
            print_usage();
            std::process::exit(2);
        }
    }
}
