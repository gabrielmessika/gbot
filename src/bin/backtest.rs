//! Binary CLI pour lancer un backtest depuis les données enregistrées.
//!
//! Usage :
//!   cargo run --bin backtest -- [OPTIONS]
//!
//! Options :
//!   --date YYYY-MM-DD       Date à rejouer (défaut: aujourd'hui)
//!   --data-dir PATH         Répertoire data/ (défaut: ./data)
//!   --equity F64            Capital de départ simulé (défaut: 10000)
//!   --compare BPS           Si spécifié, lance aussi un run SL fixe à N bps et affiche la comparaison
//!   --coins COIN,COIN,...   Liste de coins à rejouer (défaut: tous ceux disponibles)
//!
//! Exemples :
//!   cargo run --bin backtest
//!   cargo run --bin backtest -- --date 2026-04-01
//!   cargo run --bin backtest -- --date 2026-04-01 --compare 30
//!   cargo run --bin backtest -- --coins BTC,ETH,SOL --equity 5000

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use gbot::backtest::runner::BacktestRunner;
use gbot::config::settings::Settings;
use gbot::strategy::mfdp::MfdpStrategy;

fn main() -> Result<()> {
    // ── Parse args ──────────────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    let mut opts: HashMap<&str, String> = HashMap::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--date"     => { opts.insert("date",     args.get(i+1).cloned().unwrap_or_default()); i += 2; }
            "--data-dir" => { opts.insert("data_dir", args.get(i+1).cloned().unwrap_or_default()); i += 2; }
            "--equity"   => { opts.insert("equity",   args.get(i+1).cloned().unwrap_or_default()); i += 2; }
            "--compare"  => { opts.insert("compare",  args.get(i+1).cloned().unwrap_or_default()); i += 2; }
            "--coins"    => { opts.insert("coins",    args.get(i+1).cloned().unwrap_or_default()); i += 2; }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            _ => { i += 1; }
        }
    }

    // ── Load config ──────────────────────────────────────────────────────────
    let settings = Settings::load().context("Failed to load settings")?;

    let data_dir = opts.get("data_dir").cloned()
        .unwrap_or_else(|| settings.general.data_dir.clone());

    let equity: f64 = opts.get("equity")
        .and_then(|s| s.parse().ok())
        .unwrap_or(settings.general.simulated_equity);

    // ── Discover coins ────────────────────────────────────────────────────────
    let coins: Vec<String> = if let Some(coin_list) = opts.get("coins") {
        coin_list.split(',').map(|s| s.trim().to_uppercase()).collect()
    } else {
        // Auto-discover coin dirs from data/l2/
        let l2_dir = Path::new(&data_dir).join("l2");
        if l2_dir.is_dir() {
            let mut found: Vec<String> = std::fs::read_dir(&l2_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            found.sort();
            found
        } else {
            settings.coins.active.clone()
        }
    };

    // ── Resolve dates ─────────────────────────────────────────────────────────
    // --date → single date. No --date → all available dates (chronological).
    let dates: Vec<String> = if let Some(d) = opts.get("date") {
        vec![d.clone()]
    } else {
        BacktestRunner::discover_dates(&data_dir, &coins)
    };

    if coins.is_empty() || dates.is_empty() {
        eprintln!("No data found in {}/l2/", data_dir);
        eprintln!("Run the bot in observation or dry-run mode first to collect data.");
        std::process::exit(1);
    }

    // ── Print header ──────────────────────────────────────────────────────────
    let date_range = if dates.len() == 1 {
        dates[0].clone()
    } else {
        format!("{} → {}  ({} jours)", dates.first().unwrap(), dates.last().unwrap(), dates.len())
    };

    println!();
    println!("═══════════════════════════════════════════════════════════════════");
    println!("  GBOT BACKTEST");
    println!("═══════════════════════════════════════════════════════════════════");
    println!("  Période  : {}", date_range);
    println!("  Data dir : {}", data_dir);
    println!("  Equity   : ${:.2}", equity);
    println!("  Coins    : {}", coins.join(", "));
    if let Some(bps) = opts.get("compare") {
        println!("  Compare  : SL dynamique vs SL fixe {}bps", bps);
    }
    println!("═══════════════════════════════════════════════════════════════════");
    println!();

    // ── Data availability summary ─────────────────────────────────────────────
    for coin in &coins {
        let total_l2: usize = dates.iter().map(|d| {
            count_lines(&Path::new(&data_dir).join("l2").join(coin).join(format!("{}.jsonl", d)))
        }).sum();
        let total_trades: usize = dates.iter().map(|d| {
            count_lines(&Path::new(&data_dir).join("trades").join(coin).join(format!("{}.jsonl", d)))
        }).sum();
        if total_l2 > 0 {
            println!("  {:6}  L2={:7} snaps   trades={:7}", coin, total_l2, total_trades);
        }
    }
    println!();

    // ── Build runner ──────────────────────────────────────────────────────────
    let strategy = MfdpStrategy::new(settings.strategy.clone());
    let mut runner = BacktestRunner::new(strategy, equity, &settings);

    // ── Run ───────────────────────────────────────────────────────────────────
    if let Some(bps_str) = opts.get("compare") {
        let fixed_bps: f64 = bps_str.parse().context("--compare must be a number (bps)")?;
        let result = runner.run_comparison(&data_dir, &coins, &dates, &settings, fixed_bps)?;

        println!("┌─────────────────────────────────────────────────────────────┐");
        println!("│  COMPARAISON SL DYNAMIQUE vs FIXE {}bps — {}", fixed_bps, date_range);
        println!("└─────────────────────────────────────────────────────────────┘");
        println!();
        print_summary_table("SL Dynamique (volatility-based)", &result.dynamic_sl);
        println!();
        print_summary_table(&format!("SL Fixe {}bps", fixed_bps), &result.fixed_sl);
        println!();
        println!("  Delta dynamique vs fixe :");
        let pnl_sign = if result.pnl_delta >= 0.0 { "+" } else { "" };
        let wr_sign  = if result.hit_rate_delta >= 0.0 { "+" } else { "" };
        let mae_sign = if result.avg_mae_delta <= 0.0 { "" } else { "+" };
        println!("    P&L net   : {}{:.2}$", pnl_sign, result.pnl_delta);
        println!("    Win Rate  : {}{:.1}%", wr_sign, result.hit_rate_delta);
        println!("    MAE moyen : {}{:.2}bps (négatif = moins d'adversité)", mae_sign, -result.avg_mae_delta);
        println!("    Verdict   : {}", if result.pnl_delta > 0.0 { "✓ SL dynamique MEILLEUR" } else { "✗ SL fixe meilleur" });
    } else {
        let summary = runner.run_from_files(&data_dir, &coins, &dates, &settings)?;
        println!("┌─────────────────────────────────────────────────────────────┐");
        println!("│  RÉSULTATS BACKTEST — {}", date_range);
        println!("└─────────────────────────────────────────────────────────────┘");
        println!();
        print_summary_table("SL Dynamique", &summary);
        println!();
        print_per_coin_breakdown(&summary);
    }

    println!();
    Ok(())
}

fn print_summary_table(label: &str, s: &gbot::backtest::replay_engine::BacktestSummary) {
    // Compute avg position size for context
    let avg_size_usd = if s.total_trades > 0 {
        s.trades.iter().map(|t| t.size_usd).sum::<f64>() / s.total_trades as f64
    } else {
        0.0
    };
    let avg_lev = if s.total_trades > 0 {
        s.trades.iter().map(|t| t.leverage as f64).sum::<f64>() / s.total_trades as f64
    } else {
        0.0
    };

    println!("  [{}]", label);
    println!("  ─────────────────────────────────────────────────────────");
    println!("  Trades      : {}  (winners: {}  losers: {})", s.total_trades, s.winners, s.losers);
    println!("  Win Rate    : {:.1}%", s.hit_rate);
    println!("  P&L net     : {:+.2}$  (avg {:+.2}$/trade)", s.total_pnl_net, s.avg_pnl_net);
    println!("  Avg winner  : {:+.2}$   avg loser: {:+.2}$", s.avg_winner, s.avg_loser);
    println!("  Avg size    : {:.0}$ notional  avg lev: {:.1}×", avg_size_usd, avg_lev);
    println!("  Max DD      : {:.2}%", s.max_drawdown_pct);
    println!("  Maker fill  : {:.1}%", s.maker_fill_rate);
    println!("  Adverse sel.: {:.1}%  (mid contre direction à +5s)", s.adverse_selection_rate);
    println!("  Fee drag    : {:.2}%", s.fee_drag_pct);
    println!("  Avg MAE     : {:.2}bps   avg MFE: {:.2}bps", s.avg_mae_bps, s.avg_mfe_bps);
    if s.mae_to_sl_ratio > 1.0 {
        println!("  ⚠ MAE/SL   : {:.2} (> 1.0 → SL trop serré vs bruit, envisager sl_min_bps↑)", s.mae_to_sl_ratio);
    } else {
        println!("  ✓ MAE/SL   : {:.2} (SL correctement calibré)", s.mae_to_sl_ratio);
    }
}

fn print_per_coin_breakdown(s: &gbot::backtest::replay_engine::BacktestSummary) {
    use std::collections::HashMap;
    // (trades, wins, pnl_net, mae_sum, mfe_sum, size_usd_sum, adv_count)
    let mut per_coin: HashMap<&str, (usize, usize, f64, f64, f64, f64, usize)> = HashMap::new();
    for t in &s.trades {
        let e = per_coin.entry(t.coin.as_str()).or_insert((0, 0, 0.0, 0.0, 0.0, 0.0, 0));
        e.0 += 1;
        if t.pnl_net > 0.0 { e.1 += 1; }
        e.2 += t.pnl_net;
        e.3 += t.mae_bps;
        e.4 += t.mfe_bps;
        e.5 += t.size_usd;
        if t.adverse_5s { e.6 += 1; }
    }
    if per_coin.is_empty() { return; }

    println!("  [Breakdown par coin]");
    println!("  {:8}  {:>6}  {:>6}  {:>8}  {:>9}  {:>8}  {:>7}", "Coin", "Trades", "Win%", "P&L net$", "AvgSize$", "MAEmoy", "Adv5s%");
    println!("  ────────────────────────────────────────────────────────────────");
    let mut coins: Vec<_> = per_coin.keys().collect();
    coins.sort();
    for coin in coins {
        let (n, w, pnl, mae_sum, _mfe_sum, sz_sum, adv) = per_coin[coin];
        let wr    = if n > 0 { 100.0 * w as f64 / n as f64 } else { 0.0 };
        let avg_mae = if n > 0 { mae_sum / n as f64 } else { 0.0 };
        let avg_sz  = if n > 0 { sz_sum / n as f64 } else { 0.0 };
        let adv_pct = if n > 0 { 100.0 * adv as f64 / n as f64 } else { 0.0 };
        let flag = if wr < 25.0 { "  ⚠" } else { "" };
        println!("  {:8}  {:>6}  {:>5.1}%  {:>+8.2}  {:>9.0}  {:>7.2}bps  {:>6.1}%{}", coin, n, wr, pnl, avg_sz, avg_mae, adv_pct, flag);
    }
}

fn count_lines(path: &Path) -> usize {
    if !path.exists() { return 0; }
    std::fs::read_to_string(path)
        .map(|s| s.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}

fn print_usage() {
    println!("gbot backtest — rejoue les données L2/trades enregistrées

USAGE:
  cargo run --bin backtest [-- OPTIONS]

OPTIONS:
  --date YYYY-MM-DD    Date à rejouer (défaut: aujourd'hui)
  --data-dir PATH      Répertoire data/ (défaut: depuis config)
  --equity FLOAT       Capital de départ simulé en $ (défaut: 10000)
  --coins COIN,...     Coins à inclure, séparés par virgule (défaut: auto-détect)
  --compare BPS        Active le mode comparaison : dynamic SL vs SL fixe à N bps
  --help               Affiche ce message

EXEMPLES:
  cargo run --bin backtest -- --date 2026-04-01
  cargo run --bin backtest -- --date 2026-04-01 --compare 30
  cargo run --bin backtest -- --coins BTC,ETH --equity 5000

FICHIERS REQUIS (enregistrés par le bot en mode dry-run/observation):
  data/l2/BTC/2026-04-01.jsonl      — snapshots top-of-book (~1/sec)
  data/trades/BTC/2026-04-01.jsonl  — trades exécutés sur l'exchange
");
}
