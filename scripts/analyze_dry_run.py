#!/usr/bin/env python3
"""
Phase 7.4 — Analyse offline des données dry-run collectées.

Usage:
    python scripts/analyze_dry_run.py [--data-dir ./data] [--date 2026-04-01]
                                      [--signals-dir ./data/signals]
                                      [--output report.txt]

Ce script charge les fichiers JSONL enregistrés pendant un dry-run et produit :
  1. Distribution des features (spread, OFI, aggression, vol_ratio, toxicity)
  2. Corrélation Spearman feature × mid_move à +5s/+10s/+30s
  3. Taux d'adverse selection (mid move contre la direction dans les N secondes post-signal)
  4. Sensibilité SL/TP (replay paramétrique sur les signaux)
  5. Performance par coin (hit rate, P&L net simulé)
  6. Performance par heure UTC
"""

import argparse
import glob
import json
import math
import os
import sys
from collections import defaultdict
from datetime import datetime, timezone


# ─────────────────────────────────────────────────────────────────────────────
# Helpers
# ─────────────────────────────────────────────────────────────────────────────

def load_jsonl(path):
    records = []
    with open(path, "r") as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return records


def spearman_correlation(xs, ys):
    """Rank-based Spearman correlation (no scipy dependency)."""
    n = len(xs)
    if n < 3:
        return float("nan")

    def ranks(vals):
        sorted_idx = sorted(range(n), key=lambda i: vals[i])
        r = [0.0] * n
        i = 0
        while i < n:
            j = i
            while j < n and vals[sorted_idx[j]] == vals[sorted_idx[i]]:
                j += 1
            avg_rank = (i + j - 1) / 2.0
            for k in range(i, j):
                r[sorted_idx[k]] = avg_rank
            i = j
        return r

    rx = ranks(xs)
    ry = ranks(ys)
    mean_rx = sum(rx) / n
    mean_ry = sum(ry) / n
    num = sum((rx[i] - mean_rx) * (ry[i] - mean_ry) for i in range(n))
    den_x = math.sqrt(sum((rx[i] - mean_rx) ** 2 for i in range(n)))
    den_y = math.sqrt(sum((ry[i] - mean_ry) ** 2 for i in range(n)))
    if den_x == 0 or den_y == 0:
        return float("nan")
    return num / (den_x * den_y)


def percentile(data, p):
    if not data:
        return float("nan")
    s = sorted(data)
    idx = (p / 100.0) * (len(s) - 1)
    lo = int(idx)
    hi = min(lo + 1, len(s) - 1)
    return s[lo] + (idx - lo) * (s[hi] - s[lo])


def stats_summary(data, label, unit=""):
    if not data:
        print(f"  {label}: no data")
        return
    mean = sum(data) / len(data)
    p10 = percentile(data, 10)
    p50 = percentile(data, 50)
    p90 = percentile(data, 90)
    print(f"  {label}: mean={mean:.3f}{unit}  p10={p10:.3f}{unit}  p50={p50:.3f}{unit}  p90={p90:.3f}{unit}  n={len(data)}")


def sep(char="─", width=72):
    print(char * width)


# ─────────────────────────────────────────────────────────────────────────────
# Data Loading
# ─────────────────────────────────────────────────────────────────────────────

def load_signals(signals_dir, date_filter=None):
    """Load signal records from data/signals/*.jsonl"""
    pattern = os.path.join(signals_dir, "*.jsonl")
    files = sorted(glob.glob(pattern))
    if date_filter:
        files = [f for f in files if date_filter in os.path.basename(f)]
    records = []
    for f in files:
        records.extend(load_jsonl(f))
    return records


def load_l2_index(data_dir, coins, date_filter=None):
    """
    Load L2 mid prices into a per-coin sorted list of (timestamp_ms, mid).
    Returns dict: coin → [(ts_ms, mid), ...]
    """
    index = {}
    for coin in coins:
        coin_dir = os.path.join(data_dir, "l2", coin)
        if not os.path.isdir(coin_dir):
            continue
        pattern = os.path.join(coin_dir, "*.jsonl")
        files = sorted(glob.glob(pattern))
        if date_filter:
            files = [f for f in files if date_filter in os.path.basename(f)]
        entries = []
        for f in files:
            for rec in load_jsonl(f):
                ts = rec.get("timestamp")
                mid = rec.get("mid")
                if ts is not None and mid and mid > 0:
                    entries.append((ts, mid))
        entries.sort(key=lambda x: x[0])
        if entries:
            index[coin] = entries
    return index


def find_mid_at(l2_index, coin, ts_ms, offset_s):
    """Find mid price for coin at ts_ms + offset_s*1000 (binary search)."""
    entries = l2_index.get(coin)
    if not entries:
        return None
    target = ts_ms + offset_s * 1000
    lo, hi = 0, len(entries) - 1
    while lo < hi:
        mid_idx = (lo + hi) // 2
        if entries[mid_idx][0] < target:
            lo = mid_idx + 1
        else:
            hi = mid_idx
    # Return closest within 5s
    idx = lo
    for i in [max(0, idx - 1), idx, min(len(entries) - 1, idx + 1)]:
        if abs(entries[i][0] - target) <= 5000:
            return entries[i][1]
    return None


# ─────────────────────────────────────────────────────────────────────────────
# Section 1 — Feature distributions
# ─────────────────────────────────────────────────────────────────────────────

def section_feature_distributions(signals):
    placed = [s for s in signals if s.get("action") == "placed"]
    all_sig = signals

    print()
    sep("═")
    print("1. DISTRIBUTION DES FEATURES")
    sep("═")

    print(f"\n  Total signaux : {len(all_sig)}  (placés: {len(placed)}, rejetés: {len(all_sig)-len(placed)})")
    print()

    features_def = [
        ("spread_bps",           "Spread (bps)",          "bps"),
        ("ofi_10s",              "OFI 10s",               ""),
        ("aggression",           "Aggression persistence", ""),
        ("vol_ratio",            "Vol ratio (10s/30s)",    "×"),
        ("toxicity",             "Toxicité instantanée",  ""),
        ("micro_price_vs_mid_bps","Micro price vs mid",   "bps"),
        ("vamp_signal_bps",      "VAMP signal",           "bps"),
        ("dir_score",            "Direction score",        ""),
        ("queue_score",          "Queue score",           ""),
    ]

    for key, label, unit in features_def:
        vals = [s[key] for s in all_sig if key in s and s[key] is not None]
        stats_summary(vals, label, unit)

    # Negative spread check
    neg_spread = [s for s in all_sig if s.get("spread_bps", 0) <= 0]
    print(f"\n  ⚠ Signaux avec spread ≤ 0 : {len(neg_spread)}/{len(all_sig)} ({100*len(neg_spread)/max(1,len(all_sig)):.1f}%)")
    # OFI saturation
    sat = [s for s in all_sig if abs(s.get("ofi_10s", 0)) >= 0.95]
    print(f"  ⚠ Signaux OFI saturé (|ofi|≥0.95) : {len(sat)}/{len(all_sig)} ({100*len(sat)/max(1,len(all_sig)):.1f}%)")
    # vol_ratio zero
    vol_zero = [s for s in all_sig if s.get("vol_ratio", 0) == 0.0]
    print(f"  ⚠ Signaux vol_ratio=0 : {len(vol_zero)}/{len(all_sig)} ({100*len(vol_zero)/max(1,len(all_sig)):.1f}%)")


# ─────────────────────────────────────────────────────────────────────────────
# Section 2 — Corrélation feature × mid_move
# ─────────────────────────────────────────────────────────────────────────────

def section_feature_correlations(signals, l2_index, horizons_s=(5, 10, 30)):
    placed = [s for s in signals if s.get("action") == "placed"]
    if not placed:
        print("\n  Pas de signaux placés — section corrélations ignorée.")
        return

    print()
    sep("═")
    print("2. CORRÉLATION FEATURE × MID_MOVE (Spearman)")
    sep("═")

    feature_keys = ["dir_score", "ofi_10s", "aggression", "micro_price_vs_mid_bps",
                    "vamp_signal_bps", "vol_ratio", "toxicity", "spread_bps", "queue_score"]

    # Build mid_move series for each horizon
    move_series = {h: [] for h in horizons_s}
    valid_signals = []

    for sig in placed:
        coin = sig.get("coin")
        ts = sig.get("ts")
        direction = sig.get("direction", "Long")
        mid0 = find_mid_at(l2_index, coin, ts, 0)
        if mid0 is None or mid0 == 0:
            continue

        moves = {}
        for h in horizons_s:
            mid_h = find_mid_at(l2_index, coin, ts, h)
            if mid_h is None:
                moves[h] = None
            else:
                raw_move = (mid_h - mid0) / mid0 * 10_000  # bps
                signed_move = raw_move if direction == "Long" else -raw_move
                moves[h] = signed_move

        if any(v is None for v in moves.values()):
            continue

        valid_signals.append(sig)
        for h in horizons_s:
            move_series[h].append(moves[h])

    print(f"\n  Signaux avec mid-price disponible: {len(valid_signals)}/{len(placed)}")

    if len(valid_signals) < 5:
        print("  ⚠ Pas assez de données pour calculer les corrélations (< 5 signaux valides).")
        return

    print(f"\n  {'Feature':<28} " + "  ".join(f"+{h:2d}s" for h in horizons_s))
    sep()
    for key in feature_keys:
        vals = [s.get(key, float("nan")) for s in valid_signals]
        row = f"  {key:<28}"
        for h in horizons_s:
            corr = spearman_correlation(vals, move_series[h])
            if math.isnan(corr):
                row += "   n/a"
            else:
                marker = " ←" if abs(corr) >= 0.15 else ""
                row += f"  {corr:+.3f}{marker}"
        print(row)
    sep()
    print("  ← = |corr| ≥ 0.15 (potentiellement discriminant)")

    # Print mean mid_move per horizon
    print("\n  Mid-move moyen (bps, signé) après signal placé:")
    for h in horizons_s:
        m = move_series[h]
        if m:
            avg = sum(m) / len(m)
            pos = sum(1 for v in m if v > 0)
            print(f"    +{h:2d}s : avg={avg:+.2f} bps  positif={100*pos/len(m):.0f}%  n={len(m)}")


# ─────────────────────────────────────────────────────────────────────────────
# Section 3 — Adverse selection
# ─────────────────────────────────────────────────────────────────────────────

def section_adverse_selection(signals, l2_index, horizons_s=(5, 10, 30)):
    placed = [s for s in signals if s.get("action") == "placed"]
    if not placed:
        print("\n  Pas de signaux placés — section adverse selection ignorée.")
        return

    print()
    sep("═")
    print("3. ADVERSE SELECTION")
    sep("═")
    print()

    for h in horizons_s:
        adverse = []
        for sig in placed:
            coin = sig.get("coin")
            ts = sig.get("ts")
            direction = sig.get("direction", "Long")
            mid0 = find_mid_at(l2_index, coin, ts, 0)
            mid_h = find_mid_at(l2_index, coin, ts, h)
            if mid0 is None or mid_h is None or mid0 == 0:
                continue
            if direction == "Long":
                adverse.append(mid_h < mid0)
            else:
                adverse.append(mid_h > mid0)

        if adverse:
            rate = sum(adverse) / len(adverse)
            print(f"  +{h:2d}s : adverse={100*rate:.1f}%  favorable={100*(1-rate):.1f}%  n={len(adverse)}")
            if rate > 0.6:
                print(f"         ⚠ taux adverse > 60% — le signal n'a probablement pas d'edge à {h}s")

    print()
    print("  Interprétation : > 50% adverse = on entre systématiquement au mauvais moment.")
    print("  > 60% = l'edge est négatif — la stratégie entière est à revoir.")


# ─────────────────────────────────────────────────────────────────────────────
# Section 4 — Sensibilité SL/TP (replay paramétrique)
# ─────────────────────────────────────────────────────────────────────────────

def section_sl_tp_sensitivity(signals, l2_index):
    placed = [s for s in signals if s.get("action") == "placed"]
    if not placed:
        print("\n  Pas de signaux placés — section SL/TP ignorée.")
        return

    print()
    sep("═")
    print("4. SENSIBILITÉ SL/TP (replay paramétrique, exit simulé à la mid)")
    sep("═")

    sl_options_bps = [10, 15, 20, 30, 50]
    rr_options = [1.5, 2.0, 2.5, 3.0]

    fee_rt_bps = 3.0  # 1.5bps maker entry + taker exit avg

    # Build per-signal mid time series for simulation
    sims = []
    for sig in placed:
        coin = sig.get("coin")
        ts = sig.get("ts")
        direction = sig.get("direction", "Long")
        mid0 = find_mid_at(l2_index, coin, ts, 0)
        if mid0 is None or mid0 == 0:
            continue
        # Sample at 5s intervals up to 300s (5 min timeout)
        mids = []
        for offset in range(0, 305, 5):
            m = find_mid_at(l2_index, coin, ts, offset)
            mids.append(m)
        sims.append((direction, mid0, mids))

    if not sims:
        print("  ⚠ Aucun signal avec données de prix disponibles.")
        return

    print(f"\n  Signaux simulés: {len(sims)}")
    print()
    print(f"  {'SL':>6} {'R:R':>5}  {'Hit%':>6}  {'AvgPnL':>8}  {'Expect':>8}")
    sep()

    best_expect = float("-inf")
    best_params = None

    for sl_bps in sl_options_bps:
        for rr in rr_options:
            tp_bps = sl_bps * rr
            sl_pct = sl_bps / 10_000.0
            tp_pct = tp_bps / 10_000.0
            fee_pct = fee_rt_bps / 10_000.0

            results = []
            for direction, entry, mids in sims:
                if direction == "Long":
                    sl_price = entry * (1 - sl_pct)
                    tp_price = entry * (1 + tp_pct)
                else:
                    sl_price = entry * (1 + sl_pct)
                    tp_price = entry * (1 - tp_pct)

                outcome = None
                for m in mids:
                    if m is None:
                        continue
                    if direction == "Long":
                        if m <= sl_price:
                            outcome = -sl_bps - fee_rt_bps
                            break
                        elif m >= tp_price:
                            outcome = tp_bps - fee_rt_bps
                            break
                    else:
                        if m >= sl_price:
                            outcome = -sl_bps - fee_rt_bps
                            break
                        elif m <= tp_price:
                            outcome = tp_bps - fee_rt_bps
                            break

                if outcome is None:
                    # Timeout at last available mid
                    last_m = next((m for m in reversed(mids) if m is not None), None)
                    if last_m and entry > 0:
                        raw = (last_m - entry) / entry * 10_000
                        if direction == "Short":
                            raw = -raw
                        outcome = raw - fee_rt_bps
                    else:
                        outcome = -fee_rt_bps

                results.append(outcome)

            if not results:
                continue

            hit_rate = sum(1 for r in results if r > 0) / len(results)
            avg_pnl = sum(results) / len(results)
            expect = hit_rate * tp_bps - (1 - hit_rate) * sl_bps - fee_rt_bps

            marker = ""
            if expect > best_expect:
                best_expect = expect
                best_params = (sl_bps, rr)
                marker = " ← best"

            print(f"  {sl_bps:>4}bps {rr:>4.1f}×  {100*hit_rate:>5.1f}%  {avg_pnl:>+7.2f}bps  {expect:>+7.2f}bps{marker}")
        print()

    if best_params:
        print(f"  Meilleurs paramètres: sl_min_bps={best_params[0]}, target_rr={best_params[1]}")
        print(f"  Expectancy nette estimée: {best_expect:+.2f} bps/trade")


# ─────────────────────────────────────────────────────────────────────────────
# Section 5 — Performance par coin
# ─────────────────────────────────────────────────────────────────────────────

def section_per_coin(signals, l2_index):
    placed = [s for s in signals if s.get("action") == "placed"]

    print()
    sep("═")
    print("5. PERFORMANCE PAR COIN")
    sep("═")

    coin_stats = defaultdict(lambda: {"signals": 0, "wins": 0, "pnl_bps": []})
    fee_rt_bps = 3.0

    # Use actual SL/TP from the signal if available, else use defaults
    for sig in placed:
        coin = sig.get("coin")
        ts = sig.get("ts")
        direction = sig.get("direction", "Long")
        coin_stats[coin]["signals"] += 1

        mid0 = find_mid_at(l2_index, coin, ts, 0)
        if mid0 is None or mid0 == 0:
            continue

        try:
            entry = float(sig.get("entry_price", mid0))
            sl = float(sig.get("stop_loss", 0))
            tp = float(sig.get("take_profit", 0))
        except (ValueError, TypeError):
            continue

        if entry == 0:
            continue
        sl_bps = abs(entry - sl) / entry * 10_000 if sl else 20.0
        tp_bps = abs(tp - entry) / entry * 10_000 if tp else 40.0

        outcome = None
        for offset in range(0, 305, 5):
            m = find_mid_at(l2_index, coin, ts, offset)
            if m is None:
                continue
            if direction == "Long":
                if m <= sl:
                    outcome = -sl_bps - fee_rt_bps
                    break
                elif m >= tp:
                    outcome = tp_bps - fee_rt_bps
                    break
            else:
                if m >= sl:
                    outcome = -sl_bps - fee_rt_bps
                    break
                elif m <= tp:
                    outcome = tp_bps - fee_rt_bps
                    break

        if outcome is None:
            last_m = find_mid_at(l2_index, coin, ts, 300)
            if last_m and entry > 0:
                raw = (last_m - entry) / entry * 10_000
                outcome = (raw if direction == "Long" else -raw) - fee_rt_bps
            else:
                outcome = -fee_rt_bps

        coin_stats[coin]["pnl_bps"].append(outcome)
        if outcome > 0:
            coin_stats[coin]["wins"] += 1

    print()
    print(f"  {'Coin':<12}  {'Signaux':>8}  {'Simulés':>8}  {'Hit%':>6}  {'AvgPnL':>8}  {'TotalPnL':>9}")
    sep()
    for coin, st in sorted(coin_stats.items()):
        n = len(st["pnl_bps"])
        if n == 0:
            print(f"  {coin:<12}  {st['signals']:>8}  {'—':>8}  {'—':>6}  {'—':>8}  {'—':>9}")
            continue
        hit = st["wins"] / n
        avg = sum(st["pnl_bps"]) / n
        total = sum(st["pnl_bps"])
        flag = "  ⚠ weak" if hit < 0.25 else ""
        print(f"  {coin:<12}  {st['signals']:>8}  {n:>8}  {100*hit:>5.1f}%  {avg:>+7.2f}bps  {total:>+8.2f}bps{flag}")


# ─────────────────────────────────────────────────────────────────────────────
# Section 6 — Performance par heure UTC
# ─────────────────────────────────────────────────────────────────────────────

def section_per_hour(signals, l2_index):
    placed = [s for s in signals if s.get("action") == "placed"]

    print()
    sep("═")
    print("6. PERFORMANCE PAR HEURE UTC")
    sep("═")

    hour_stats = defaultdict(lambda: {"n": 0, "wins": 0, "pnl_bps": []})
    fee_rt_bps = 3.0

    for sig in placed:
        coin = sig.get("coin")
        ts = sig.get("ts")
        if ts is None:
            continue
        hour = datetime.fromtimestamp(ts / 1000, tz=timezone.utc).hour
        direction = sig.get("direction", "Long")

        mid0 = find_mid_at(l2_index, coin, ts, 0)
        if mid0 is None or mid0 == 0:
            hour_stats[hour]["n"] += 1
            continue

        try:
            entry = float(sig.get("entry_price", mid0))
            sl = float(sig.get("stop_loss", 0))
            tp = float(sig.get("take_profit", 0))
        except (ValueError, TypeError):
            hour_stats[hour]["n"] += 1
            continue

        if entry == 0:
            hour_stats[hour]["n"] += 1
            continue

        sl_bps = abs(entry - sl) / entry * 10_000 if sl else 20.0
        tp_bps = abs(tp - entry) / entry * 10_000 if tp else 40.0

        outcome = None
        for offset in range(0, 305, 5):
            m = find_mid_at(l2_index, coin, ts, offset)
            if m is None:
                continue
            if direction == "Long":
                if m <= sl:
                    outcome = -sl_bps - fee_rt_bps
                    break
                elif m >= tp:
                    outcome = tp_bps - fee_rt_bps
                    break
            else:
                if m >= sl:
                    outcome = -sl_bps - fee_rt_bps
                    break
                elif m <= tp:
                    outcome = tp_bps - fee_rt_bps
                    break

        if outcome is None:
            last_m = find_mid_at(l2_index, coin, ts, 300)
            if last_m and entry > 0:
                raw = (last_m - entry) / entry * 10_000
                outcome = (raw if direction == "Long" else -raw) - fee_rt_bps
            else:
                outcome = -fee_rt_bps

        hour_stats[hour]["n"] += 1
        hour_stats[hour]["pnl_bps"].append(outcome)
        if outcome > 0:
            hour_stats[hour]["wins"] += 1

    print()
    print(f"  {'Heure UTC':>10}  {'Signaux':>8}  {'Simulés':>8}  {'Hit%':>6}  {'AvgPnL':>8}")
    sep()
    for hour in range(24):
        st = hour_stats.get(hour)
        if st is None or st["n"] == 0:
            continue
        n = len(st["pnl_bps"])
        if n == 0:
            print(f"  {hour:02d}:00 UTC  {st['n']:>8}  {'—':>8}  {'—':>6}  {'—':>8}")
            continue
        hit = st["wins"] / n
        avg = sum(st["pnl_bps"]) / n
        bar = "█" * int(hit * 10) + "░" * (10 - int(hit * 10))
        print(f"  {hour:02d}:00 UTC  {st['n']:>8}  {n:>8}  {100*hit:>5.1f}%  {avg:>+7.2f}bps  {bar}")

    print()
    print("  Sessions de référence : Asia 01-08h  |  London 07-16h  |  NY 13-21h UTC")


# ─────────────────────────────────────────────────────────────────────────────
# Section 7 — Recommandations de calibration
# ─────────────────────────────────────────────────────────────────────────────

def section_recommendations(signals):
    placed = [s for s in signals if s.get("action") == "placed"]
    rejected = [s for s in signals if s.get("action") != "placed"]

    print()
    sep("═")
    print("7. RECOMMANDATIONS DE CALIBRATION")
    sep("═")
    print()

    n = len(signals)
    n_placed = len(placed)
    n_rejected = len(rejected)

    skip_rate = n_rejected / n if n else 0
    print(f"  Taux de filtrage: {100*skip_rate:.1f}% rejetés ({n_rejected}/{n})")
    if skip_rate > 0.9:
        print("  ⚠ Trop de signaux rejetés — envisager d'assouplir les guards ou le seuil")
    elif skip_rate < 0.3:
        print("  ⚠ Trop peu de signaux filtrés — les guards sont peut-être trop permissifs")

    # spread check
    neg_spread = sum(1 for s in signals if s.get("spread_bps", 0) <= 0)
    if neg_spread == 0:
        print("  ✓ Aucun signal avec spread ≤ 0 (book sanitization OK)")
    else:
        print(f"  ✗ {neg_spread} signaux avec spread ≤ 0 — vérifier book sanitization")

    # OFI saturation
    sat = sum(1 for s in signals if abs(s.get("ofi_10s", 0)) >= 0.95)
    if sat == 0:
        print("  ✓ Aucune saturation OFI (confidence scaling OK)")
    else:
        print(f"  ✗ {sat} signaux avec OFI saturé ({100*sat/n:.1f}%) — ajuster MIN_OFI_TRADES")

    # vol_ratio zero
    vol_zero = sum(1 for s in signals if s.get("vol_ratio", 0) == 0.0)
    if vol_zero == 0:
        print("  ✓ Aucun signal avec vol_ratio=0 (maturity guard OK)")
    else:
        print(f"  ✗ {vol_zero} signaux avec vol_ratio=0 ({100*vol_zero/n:.1f}%) — maturity guard insuffisant")

    # dir_score avg
    dir_scores = [s.get("dir_score", 0) for s in placed if "dir_score" in s]
    if dir_scores:
        avg_dir = sum(dir_scores) / len(dir_scores)
        if avg_dir < 0.55:
            print(f"  ⚠ dir_score moyen = {avg_dir:.3f} (< 0.55) — signals à faible conviction")
            print("    Suggestion: augmenter direction_threshold de 0.50 → 0.55 ou 0.60")
        else:
            print(f"  ✓ dir_score moyen = {avg_dir:.3f} (conviction correcte)")

    print()
    print("  Paramètres à vérifier dans tbot.properties / default.toml :")
    print("    sl_min_bps, sl_max_bps, sl_vol_multiplier, target_rr")
    print("    min_direction_confirmations, direction_threshold_long/short")
    print("    pullback_retrace_pct, pullback_max_wait_s, pullback_min_move_bps")
    print()


# ─────────────────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Analyse offline données dry-run gbot")
    parser.add_argument("--data-dir", default="./data", help="Répertoire data/ (défaut: ./data)")
    parser.add_argument("--signals-dir", default=None, help="Répertoire des signaux (défaut: data-dir/signals)")
    parser.add_argument("--date", default=None, help="Filtre de date YYYY-MM-DD (optionnel)")
    parser.add_argument("--output", default=None, help="Fichier de sortie (défaut: stdout)")
    args = parser.parse_args()

    signals_dir = args.signals_dir or os.path.join(args.data_dir, "signals")

    if args.output:
        import io
        buf = io.StringIO()
        old_stdout = sys.stdout
        sys.stdout = buf

    print()
    sep("═", 72)
    print("GBOT — ANALYSE OFFLINE DRY-RUN")
    now_str = datetime.now(tz=timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    print(f"Généré le {now_str}")
    if args.date:
        print(f"Filtre date: {args.date}")
    sep("═", 72)

    # Load signals
    if not os.path.isdir(signals_dir):
        print(f"\nErreur: répertoire signals introuvable: {signals_dir}")
        sys.exit(1)

    signals = load_signals(signals_dir, args.date)
    if not signals:
        print(f"\nAucun signal trouvé dans {signals_dir}")
        sys.exit(1)

    print(f"\nSignaux chargés: {len(signals)}")

    # Discover coins
    coins = list(set(s.get("coin", "") for s in signals if s.get("coin")))
    coins.sort()
    print(f"Coins: {', '.join(coins)}")

    # Load L2 index
    l2_index = load_l2_index(args.data_dir, coins, args.date)
    l2_coins = list(l2_index.keys())
    print(f"Données L2 disponibles pour: {', '.join(l2_coins) if l2_coins else '(aucun)'}")

    # Run sections
    section_feature_distributions(signals)
    section_feature_correlations(signals, l2_index)
    section_adverse_selection(signals, l2_index)
    section_sl_tp_sensitivity(signals, l2_index)
    section_per_coin(signals, l2_index)
    section_per_hour(signals, l2_index)
    section_recommendations(signals)

    print()
    sep("═", 72)
    print("FIN DU RAPPORT")
    sep("═", 72)
    print()

    if args.output:
        sys.stdout = old_stdout
        with open(args.output, "w") as f:
            f.write(buf.getvalue())
        print(f"Rapport écrit dans {args.output}")
        print(buf.getvalue())


if __name__ == "__main__":
    main()
