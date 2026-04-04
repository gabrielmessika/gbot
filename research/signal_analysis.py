#!/usr/bin/env python3
"""
Signal Analysis: Find predictive features in L2 book + trade data.

Computes candidate microstructure features and measures their correlation
with forward returns at multiple horizons. Goal: find anything with |corr| > 0.05.
"""

import argparse
import json
import os
import sys
import time
from collections import defaultdict
from pathlib import Path

try:
    import numpy as np
    import pandas as pd
    from scipy import stats as scipy_stats
except ImportError as e:
    print(f"Missing dependency: {e}")
    print("Run: pip install numpy pandas scipy")
    sys.exit(1)


# ---------------------------------------------------------------------------
# Data loading
# ---------------------------------------------------------------------------

def load_jsonl(path: Path) -> pd.DataFrame:
    """Stream-load a JSONL file into a DataFrame."""
    rows = []
    with open(path) as f:
        for line in f:
            if line.strip():
                rows.append(json.loads(line))
    if not rows:
        return pd.DataFrame()
    df = pd.DataFrame(rows)
    return df


def load_l2(data_dir: Path, coin: str, dates: list[str]) -> pd.DataFrame:
    frames = []
    for date in dates:
        p = data_dir / "l2" / coin / f"{date}.jsonl"
        if p.exists():
            df = load_jsonl(p)
            frames.append(df)
            print(f"  L2  {coin}/{date}: {len(df):>8,} rows")
        else:
            print(f"  L2  {coin}/{date}: MISSING")
    if not frames:
        return pd.DataFrame()
    df = pd.concat(frames, ignore_index=True)
    df.sort_values("timestamp", inplace=True)
    df.reset_index(drop=True, inplace=True)
    return df


def load_trades(data_dir: Path, coin: str, dates: list[str]) -> pd.DataFrame:
    frames = []
    for date in dates:
        p = data_dir / "trades" / coin / f"{date}.jsonl"
        if p.exists():
            df = load_jsonl(p)
            frames.append(df)
            print(f"  TRD {coin}/{date}: {len(df):>8,} rows")
        else:
            print(f"  TRD {coin}/{date}: MISSING")
    if not frames:
        return pd.DataFrame()
    df = pd.concat(frames, ignore_index=True)
    df.sort_values("timestamp", inplace=True)
    df.reset_index(drop=True, inplace=True)
    return df


# ---------------------------------------------------------------------------
# Feature computation
# ---------------------------------------------------------------------------

def compute_trade_features_at_l2(l2: pd.DataFrame, trades: pd.DataFrame) -> pd.DataFrame:
    """
    For each L2 snapshot, compute rolling trade-based features.
    Uses vectorized merging via merge_asof for speed.
    """
    ts = l2["timestamp"].values  # ms timestamps
    n = len(ts)

    # Pre-compute trade arrays for fast windowed lookups
    t_ts = trades["timestamp"].values
    t_price = trades["price"].values
    t_size = trades["size"].values
    t_buy = trades["is_buy"].values.astype(np.float64)
    t_notional = t_price * t_size
    t_signed_vol = t_size * np.where(t_buy, 1.0, -1.0)
    t_signed_notional = t_notional * np.where(t_buy, 1.0, -1.0)

    # Median trade size for "large trade" detection
    median_size = np.median(t_size) if len(t_size) > 0 else 1.0
    large_threshold = 5.0 * median_size

    # Windows in ms
    windows_ms = {
        "3s": 3000,
        "5s": 5000,
        "10s": 10000,
        "30s": 30000,
        "60s": 60000,
    }

    # Use searchsorted for efficient window lookups
    # For each L2 timestamp, find the range of trades in [ts - window, ts]
    results = {}

    for wname, wms in windows_ms.items():
        left_idx = np.searchsorted(t_ts, ts - wms, side="left")
        right_idx = np.searchsorted(t_ts, ts, side="right")

        buy_vol = np.zeros(n)
        sell_vol = np.zeros(n)
        trade_count = np.zeros(n)
        net_notional = np.zeros(n)
        vwap = np.zeros(n)
        large_count = np.zeros(n)
        large_net = np.zeros(n)

        for i in range(n):
            li, ri = left_idx[i], right_idx[i]
            if li >= ri:
                continue
            slc_size = t_size[li:ri]
            slc_buy = t_buy[li:ri]
            slc_notional = t_notional[li:ri]
            slc_signed_notional = t_signed_notional[li:ri]
            slc_price = t_price[li:ri]

            bv = np.sum(slc_size * slc_buy)
            sv = np.sum(slc_size * (1 - slc_buy))
            buy_vol[i] = bv
            sell_vol[i] = sv
            trade_count[i] = ri - li
            net_notional[i] = np.sum(slc_signed_notional)
            total_vol = np.sum(slc_size)
            if total_vol > 0:
                vwap[i] = np.sum(slc_price * slc_size) / total_vol
            else:
                vwap[i] = np.nan

            # Large trades
            large_mask = slc_size > large_threshold
            large_count[i] = np.sum(large_mask)
            if np.any(large_mask):
                large_net[i] = np.sum(
                    slc_size[large_mask] * np.where(slc_buy[large_mask], 1.0, -1.0)
                )

        total_vol_arr = buy_vol + sell_vol
        imbalance = np.where(
            total_vol_arr > 0,
            (buy_vol - sell_vol) / total_vol_arr,
            0.0,
        )
        results[f"trade_imb_{wname}"] = imbalance
        results[f"trade_count_{wname}"] = trade_count
        results[f"net_notional_{wname}"] = net_notional

        if wname in ("10s",):
            results["large_count_10s"] = large_count
            results["large_net_10s"] = large_net

        if wname in ("30s", "60s"):
            mid = l2["mid"].values
            results[f"vwap_dev_{wname}"] = np.where(
                ~np.isnan(vwap) & (vwap > 0),
                (mid - vwap) / mid * 10000,  # bps
                0.0,
            )

    return pd.DataFrame(results, index=l2.index)


def compute_l2_features(l2: pd.DataFrame) -> pd.DataFrame:
    """Compute features from L2 book data alone."""
    ts = l2["timestamp"].values
    mid = l2["mid"].values
    bid_depth = l2["bid_depth_10bps"].values
    ask_depth = l2["ask_depth_10bps"].values
    spread = l2["spread_bps"].values

    n = len(ts)

    # Book imbalance
    total_depth = bid_depth + ask_depth
    book_imb = np.where(total_depth > 0, (bid_depth - ask_depth) / total_depth, 0.0)

    results = {
        "book_imb": book_imb,
        "spread_bps": spread,
    }

    # Lookback features: changes over 5s window
    # Find index offset for ~5s ago using searchsorted
    idx_5s = np.searchsorted(ts, ts - 5000, side="left")
    idx_10s = np.searchsorted(ts, ts - 10000, side="left")

    # Book imbalance acceleration (change in book_imb over 5s)
    results["book_imb_accel_5s"] = book_imb - book_imb[idx_5s]

    # Spread change over 5s
    results["spread_change_5s"] = spread - spread[idx_5s]

    # OFI delta: change in (bid_depth - ask_depth) as proxy for order flow
    ofi = bid_depth - ask_depth
    results["ofi_delta_5s"] = ofi - ofi[idx_5s]
    results["ofi_delta_10s"] = ofi - ofi[idx_10s]

    # Mid price momentum (in bps)
    results["mid_mom_5s"] = np.where(
        mid[idx_5s] > 0, (mid - mid[idx_5s]) / mid[idx_5s] * 10000, 0.0
    )
    results["mid_mom_10s"] = np.where(
        mid[idx_10s] > 0, (mid - mid[idx_10s]) / mid[idx_10s] * 10000, 0.0
    )

    return pd.DataFrame(results, index=l2.index)


def compute_trade_intensity_features(l2: pd.DataFrame, trade_feats: pd.DataFrame) -> pd.DataFrame:
    """Trade intensity: trades/sec over 5s vs 30s."""
    tc5 = trade_feats.get("trade_count_5s")
    tc30 = trade_feats.get("trade_count_30s")
    if tc5 is None or tc30 is None:
        return pd.DataFrame(index=l2.index)

    intensity_5s = tc5 / 5.0
    intensity_30s = tc30 / 30.0
    intensity_change = np.where(
        intensity_30s > 0,
        (intensity_5s - intensity_30s) / intensity_30s,
        0.0,
    )
    return pd.DataFrame({"trade_intensity_accel": intensity_change}, index=l2.index)


# ---------------------------------------------------------------------------
# Forward returns
# ---------------------------------------------------------------------------

def compute_forward_returns(l2: pd.DataFrame) -> pd.DataFrame:
    """Compute forward mid-price returns at various horizons."""
    ts = l2["timestamp"].values
    mid = l2["mid"].values
    n = len(ts)

    horizons_ms = {
        "fwd_5s": 5000,
        "fwd_10s": 10000,
        "fwd_30s": 30000,
        "fwd_60s": 60000,
        "fwd_120s": 120000,
        "fwd_300s": 300000,
    }

    results = {}
    for name, hms in horizons_ms.items():
        fwd_idx = np.searchsorted(ts, ts + hms, side="left")
        fwd_idx = np.clip(fwd_idx, 0, n - 1)
        fwd_mid = mid[fwd_idx]
        # Mark as NaN if we couldn't find a future point (within 20% tolerance)
        actual_dt = ts[fwd_idx] - ts
        valid = (actual_dt >= hms * 0.8) & (actual_dt <= hms * 1.5)
        ret = np.where(
            valid & (mid > 0),
            (fwd_mid - mid) / mid * 10000,  # bps
            np.nan,
        )
        results[name] = ret

    return pd.DataFrame(results, index=l2.index)


# ---------------------------------------------------------------------------
# Analysis
# ---------------------------------------------------------------------------

def correlation_analysis(features: pd.DataFrame, fwd_returns: pd.DataFrame):
    """Spearman rank correlation between each feature and each forward return."""
    feat_cols = features.columns.tolist()
    ret_cols = fwd_returns.columns.tolist()

    corr_matrix = pd.DataFrame(index=feat_cols, columns=ret_cols, dtype=float)

    for fc in feat_cols:
        for rc in ret_cols:
            mask = features[fc].notna() & fwd_returns[rc].notna()
            if mask.sum() < 100:
                corr_matrix.loc[fc, rc] = np.nan
                continue
            c, _ = scipy_stats.spearmanr(features.loc[mask, fc], fwd_returns.loc[mask, rc])
            corr_matrix.loc[fc, rc] = round(c, 6)

    return corr_matrix


def win_rate_analysis(features: pd.DataFrame, fwd_returns: pd.DataFrame):
    """For top/bottom decile of each feature, compute directional win rate."""
    feat_cols = features.columns.tolist()
    ret_cols = fwd_returns.columns.tolist()
    rows = []

    for fc in feat_cols:
        valid = features[fc].notna() & ~features[fc].isin([0.0])
        if valid.sum() < 100:
            continue
        vals = features.loc[valid, fc]
        q10 = vals.quantile(0.1)
        q90 = vals.quantile(0.9)
        top_mask = valid & (features[fc] >= q90)
        bot_mask = valid & (features[fc] <= q10)

        for rc in ret_cols:
            rv = fwd_returns[rc]
            # Top decile: expect positive return if feature is bullish signal
            top_valid = top_mask & rv.notna()
            bot_valid = bot_mask & rv.notna()

            if top_valid.sum() < 30 or bot_valid.sum() < 30:
                continue

            top_wr = (rv[top_valid] > 0).mean()
            bot_wr = (rv[bot_valid] < 0).mean()  # expect negative when feature is low

            rows.append({
                "feature": fc,
                "horizon": rc,
                "top_decile_bullish_wr": round(top_wr, 4),
                "bot_decile_bearish_wr": round(bot_wr, 4),
                "top_n": int(top_valid.sum()),
                "bot_n": int(bot_valid.sum()),
                "edge_bps": round((top_wr + bot_wr - 1.0) * 100, 2),  # excess over random
            })

    return pd.DataFrame(rows)


def cross_coin_leadlag(l2_dict: dict[str, pd.DataFrame], base: str = "BTC"):
    """Compute lead-lag correlations between base coin and others."""
    if base not in l2_dict or len(l2_dict) < 2:
        print("\n  Skipping cross-coin analysis (need BTC + at least one other coin)")
        return

    lags_sec = [0, 1, 2, 5, 10, 30]
    base_df = l2_dict[base][["timestamp", "mid"]].copy()
    base_df["ts_sec"] = (base_df["timestamp"] / 1000).astype(int)
    base_agg = base_df.groupby("ts_sec")["mid"].last()
    base_ret = base_agg.pct_change().dropna() * 10000  # bps

    print(f"\n{'='*70}")
    print("CROSS-COIN LEAD-LAG ANALYSIS")
    print(f"{'='*70}")
    print(f"Base coin: {base} ({len(base_ret)} 1-sec return observations)")

    for coin, l2 in l2_dict.items():
        if coin == base:
            continue
        other_df = l2[["timestamp", "mid"]].copy()
        other_df["ts_sec"] = (other_df["timestamp"] / 1000).astype(int)
        other_agg = other_df.groupby("ts_sec")["mid"].last()
        other_ret = other_agg.pct_change().dropna() * 10000

        print(f"\n  {base} -> {coin} lead-lag:")
        print(f"  {'Lag (sec)':>10}  {'Corr':>8}  {'Interpretation'}")
        print(f"  {'-'*50}")

        best_lag = 0
        best_corr = 0

        for lag in lags_sec:
            # Positive lag = base leads (base return at t correlates with other at t+lag)
            if lag == 0:
                common = base_ret.index.intersection(other_ret.index)
                if len(common) < 100:
                    continue
                c, _ = scipy_stats.spearmanr(base_ret[common], other_ret[common])
            else:
                shifted_idx = base_ret.index + lag
                common = shifted_idx[shifted_idx.isin(other_ret.index)]
                orig_idx = common - lag
                if len(common) < 100:
                    continue
                c, _ = scipy_stats.spearmanr(
                    base_ret[orig_idx].values, other_ret[common].values
                )

            label = "concurrent" if lag == 0 else f"{base} leads by {lag}s"
            marker = " ***" if abs(c) > 0.05 else ""
            print(f"  {lag:>10}  {c:>8.4f}  {label}{marker}")

            if abs(c) > abs(best_corr):
                best_corr = c
                best_lag = lag

        if abs(best_corr) > 0.03:
            print(f"\n  => Best: lag={best_lag}s, corr={best_corr:.4f}")
            if best_lag > 0:
                print(f"  => {base} LEADS {coin} by ~{best_lag}s (potentially exploitable)")
            else:
                print(f"  => Concurrent movement (not directly exploitable as lead-lag)")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def process_coin(data_dir: Path, coin: str, dates: list[str]):
    """Load data and compute all features + forward returns for one coin."""
    print(f"\n--- Loading {coin} ---")
    l2 = load_l2(data_dir, coin, dates)
    trades = load_trades(data_dir, coin, dates)

    if l2.empty:
        print(f"  No L2 data for {coin}, skipping")
        return None, None, None

    print(f"  Computing L2 features...")
    l2_feats = compute_l2_features(l2)

    print(f"  Computing trade features (this takes a moment)...")
    t0 = time.time()
    # Subsample L2 to every ~1s for speed (original is ~500ms intervals)
    step = max(1, len(l2) // (len(l2) // 2))  # keep every other row
    l2_sub = l2.iloc[::2].copy().reset_index(drop=True)
    trades_sub = trades  # keep all trades

    trade_feats = compute_trade_features_at_l2(l2_sub, trades_sub)
    l2_feats_sub = compute_l2_features(l2_sub)
    intensity_feats = compute_trade_intensity_features(l2_sub, trade_feats)
    print(f"  Trade features computed in {time.time()-t0:.1f}s")

    print(f"  Computing forward returns...")
    fwd_ret = compute_forward_returns(l2_sub)

    # Combine all features
    all_feats = pd.concat([l2_feats_sub, trade_feats, intensity_feats], axis=1)

    return l2_sub, all_feats, fwd_ret


def main():
    parser = argparse.ArgumentParser(description="Signal analysis for L2 + trade data")
    parser.add_argument("--coins", default="BTC,ETH,SOL", help="Comma-separated coins")
    parser.add_argument(
        "--dates",
        default="2026-04-01,2026-04-02,2026-04-03,2026-04-04",
        help="Comma-separated dates",
    )
    parser.add_argument("--data-dir", default="./server-data", help="Data directory")
    args = parser.parse_args()

    coins = [c.strip() for c in args.coins.split(",")]
    dates = [d.strip() for d in args.dates.split(",")]
    data_dir = Path(args.data_dir)

    print("=" * 70)
    print("SIGNAL ANALYSIS")
    print(f"Coins: {coins}")
    print(f"Dates: {dates}")
    print(f"Data dir: {data_dir.resolve()}")
    print("=" * 70)

    # Process each coin
    all_features = []
    all_fwd_ret = []
    l2_dict = {}

    for coin in coins:
        l2_sub, feats, fwd_ret = process_coin(data_dir, coin, dates)
        if feats is not None:
            all_features.append(feats)
            all_fwd_ret.append(fwd_ret)
            # Store full L2 for cross-coin analysis
            l2_full = load_l2(data_dir, coin, dates) if coin in ("BTC", "ETH", "SOL") else None
            # Actually re-use the subsampled version to save memory
            l2_dict[coin] = load_l2(data_dir, coin, dates)

    if not all_features:
        print("No data loaded!")
        sys.exit(1)

    # Combine across coins
    features = pd.concat(all_features, ignore_index=True)
    fwd_returns = pd.concat(all_fwd_ret, ignore_index=True)

    print(f"\n{'='*70}")
    print(f"TOTAL: {len(features):,} feature rows across {len(coins)} coins")
    print(f"Features: {list(features.columns)}")
    print(f"{'='*70}")

    # --- 1. Correlation analysis ---
    print(f"\n{'='*70}")
    print("SPEARMAN RANK CORRELATION: Features vs Forward Returns")
    print(f"{'='*70}")

    corr = correlation_analysis(features, fwd_returns)
    # Print with formatting
    pd.set_option("display.max_columns", 20)
    pd.set_option("display.width", 140)
    pd.set_option("display.float_format", lambda x: f"{x:.4f}" if not np.isnan(x) else "NaN")
    print(corr.to_string())

    # Highlight significant correlations
    print(f"\n--- Correlations with |corr| > 0.05 ---")
    significant = []
    for fc in corr.index:
        for rc in corr.columns:
            v = corr.loc[fc, rc]
            if pd.notna(v) and abs(float(v)) > 0.05:
                significant.append((fc, rc, float(v)))

    if significant:
        significant.sort(key=lambda x: abs(x[2]), reverse=True)
        for fc, rc, v in significant:
            print(f"  {fc:>30s}  x  {rc:<12s}  corr = {v:+.4f}  {'***' if abs(v)>0.1 else '**' if abs(v)>0.07 else '*'}")
    else:
        print("  None found. All correlations are below 0.05 threshold.")

    # --- 2. Win rate analysis ---
    print(f"\n{'='*70}")
    print("WIN RATE ANALYSIS: Top/Bottom Decile Directional Win Rate")
    print(f"{'='*70}")

    wr = win_rate_analysis(features, fwd_returns)
    if not wr.empty:
        # Sort by edge
        wr_sorted = wr.sort_values("edge_bps", ascending=False)
        print("\nTop 20 feature-horizon combos by edge over random:")
        print(wr_sorted.head(20).to_string(index=False))

        print("\nBottom 20 (strongest negative-signal features):")
        print(wr_sorted.tail(20).to_string(index=False))
    else:
        print("  Not enough data for win rate analysis.")

    # --- 3. Top 5 features per horizon ---
    print(f"\n{'='*70}")
    print("TOP 5 FEATURES BY |CORRELATION| PER HORIZON")
    print(f"{'='*70}")

    best_horizon = None
    best_horizon_max_corr = 0

    for rc in corr.columns:
        col = corr[rc].astype(float).abs().sort_values(ascending=False)
        print(f"\n  {rc}:")
        for i, (feat, val) in enumerate(col.head(5).items()):
            print(f"    {i+1}. {feat:>30s}  |corr| = {val:.4f}")
        top_val = col.iloc[0] if len(col) > 0 else 0
        if top_val > best_horizon_max_corr:
            best_horizon_max_corr = top_val
            best_horizon = rc

    print(f"\n  => RECOMMENDED HORIZON: {best_horizon} (max |corr| = {best_horizon_max_corr:.4f})")

    # --- 4. Cross-coin lead-lag ---
    cross_coin_leadlag(l2_dict, base="BTC")

    # --- 5. Summary ---
    print(f"\n{'='*70}")
    print("SUMMARY & RECOMMENDATIONS")
    print(f"{'='*70}")

    if significant:
        print(f"\nFound {len(significant)} feature-horizon pairs with |corr| > 0.05:")
        for fc, rc, v in significant[:10]:
            print(f"  - {fc} -> {rc}: {v:+.4f}")
        print(f"\nStrongest signal: {significant[0][0]} -> {significant[0][1]}: {significant[0][2]:+.4f}")
        print(f"Recommended horizon: {best_horizon}")
    else:
        print("\nNO features found with |corr| > 0.05 at any horizon.")
        print("Consider:")
        print("  1. Adding more features (volatility regime, time-of-day)")
        print("  2. Non-linear feature transformations")
        print("  3. Longer lookback windows")
        print("  4. Cross-asset features (BTC leading others)")

    if not wr.empty:
        best_wr = wr.sort_values("edge_bps", ascending=False).iloc[0]
        print(f"\nBest win-rate edge: {best_wr['feature']} at {best_wr['horizon']}")
        print(f"  Top decile bullish WR: {best_wr['top_decile_bullish_wr']:.1%}")
        print(f"  Bot decile bearish WR: {best_wr['bot_decile_bearish_wr']:.1%}")
        print(f"  Combined edge: {best_wr['edge_bps']:.1f} bps over random")


if __name__ == "__main__":
    main()
