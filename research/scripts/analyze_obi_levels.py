#!/usr/bin/env python3
"""
analyze_obi_levels.py — Multi-level Order Book Imbalance analysis.

Reads L2 book snapshots (JSONL from gbot recorder) and evaluates whether
depth levels L2-L10 add predictive power over L1-only OBI.

Decision criterion: OBI_L10 must have corr(obi_lN, ret_30s) >= 2× corr(obi_l1, ret_30s)
to justify the complexity of a multi-level feature.

Usage:
    python analyze_obi_levels.py --data-dir ./data/l2 --coin ETH --date 2026-04-01

Requirements:
    pip install pandas numpy scipy
"""

import argparse
import json
import os
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
import pandas as pd
from scipy import stats


def load_book_records(data_dir: str, coin: str, date: str) -> pd.DataFrame:
    path = Path(data_dir) / coin / f"{date}.jsonl"
    if not path.exists():
        # Try to find any available file
        coin_dir = Path(data_dir) / coin
        if not coin_dir.exists():
            print(f"ERROR: No data dir for {coin} at {coin_dir}")
            sys.exit(1)
        files = sorted(coin_dir.glob("*.jsonl"))
        if not files:
            print(f"ERROR: No JSONL files found in {coin_dir}")
            sys.exit(1)
        path = files[-1]
        print(f"[INFO] Using {path}")

    records = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                records.append(json.loads(line))

    df = pd.DataFrame(records)
    df["timestamp"] = pd.to_numeric(df["timestamp"])
    df = df.sort_values("timestamp").reset_index(drop=True)
    print(f"[INFO] Loaded {len(df)} book snapshots for {coin} from {path.name}")
    return df


def compute_obi_levels(row, n_levels: int) -> float:
    """Compute OBI using top-N bid and ask levels.

    OBI_N = (sum_N bid_size - sum_N ask_size) / (sum_N bid_size + sum_N ask_size)
    """
    bids = row.get("bid_levels", [])[:n_levels]
    asks = row.get("ask_levels", [])[:n_levels]

    bid_vol = sum(lvl[1] for lvl in bids if len(lvl) >= 2)
    ask_vol = sum(lvl[1] for lvl in asks if len(lvl) >= 2)
    total = bid_vol + ask_vol
    if total == 0:
        return 0.0
    return (bid_vol - ask_vol) / total


def compute_mid_return_30s(df: pd.DataFrame) -> pd.Series:
    """Compute forward 30s return from mid price."""
    ts = df["timestamp"].values
    mid = df["mid"].values
    returns = np.full(len(df), np.nan)

    j = 0
    for i in range(len(df)):
        # Advance j to first record >= ts[i] + 30_000ms
        while j < len(df) and ts[j] < ts[i] + 30_000:
            j += 1
        if j < len(df):
            if mid[i] > 0:
                returns[i] = (mid[j] - mid[i]) / mid[i] * 10_000  # bps
    return pd.Series(returns, index=df.index)


def analyze(df: pd.DataFrame, max_levels: int = 10) -> pd.DataFrame:
    """Compute OBI at each level depth and correlate with forward returns."""
    print(f"\n[ANALYSIS] Computing OBI for L1 to L{max_levels}...")

    # Check that bid_levels / ask_levels columns exist
    if "bid_levels" not in df.columns or "ask_levels" not in df.columns:
        print("ERROR: bid_levels / ask_levels not in data. Ensure gbot recorder version >= multi-level.")
        sys.exit(1)

    # Filter out rows without bid_levels (old recorder format)
    has_levels = df["bid_levels"].apply(lambda x: isinstance(x, list) and len(x) > 0)
    dropped = (~has_levels).sum()
    if dropped > 0:
        print(f"[INFO] Dropped {dropped} rows without bid_levels (old format)")
        df = df[has_levels].reset_index(drop=True)
    if len(df) == 0:
        print("ERROR: No rows with bid_levels found.")
        sys.exit(1)

    # Parse levels from JSON if stored as strings
    if isinstance(df["bid_levels"].iloc[0], str):
        df["bid_levels"] = df["bid_levels"].apply(json.loads)
        df["ask_levels"] = df["ask_levels"].apply(json.loads)

    # Check max available depth
    sample_bids = df["bid_levels"].iloc[0]
    if len(sample_bids) < 2:
        print(f"WARNING: Only {len(sample_bids)} bid levels available. Max analysis depth reduced.")
        max_levels = min(max_levels, len(sample_bids))

    # Compute OBI at each depth
    for n in range(1, max_levels + 1):
        df[f"obi_l{n}"] = df.apply(lambda row: compute_obi_levels(row, n), axis=1)

    # Forward 30s return
    df["ret_30s"] = compute_mid_return_30s(df)
    valid = df["ret_30s"].notna()
    print(f"[INFO] Valid rows for correlation: {valid.sum()} / {len(df)}")

    df_valid = df[valid].copy()

    # Correlation analysis
    results = []
    for n in range(1, max_levels + 1):
        col = f"obi_l{n}"
        corr, pval = stats.pearsonr(df_valid[col], df_valid["ret_30s"])
        results.append({
            "levels": n,
            "corr_ret30s": corr,
            "p_value": pval,
            "significant": pval < 0.05,
        })

    result_df = pd.DataFrame(results)
    return result_df, df_valid


def directional_accuracy(df_valid: pd.DataFrame, col: str, threshold: float = 0.0) -> float:
    """WR: fraction where sign(obi) matches sign(ret_30s), for |obi| > threshold."""
    mask = df_valid[col].abs() > threshold
    if mask.sum() == 0:
        return 0.0
    subset = df_valid[mask]
    agree = (np.sign(subset[col]) == np.sign(subset["ret_30s"])).sum()
    return agree / len(subset)


def print_report(result_df: pd.DataFrame, df_valid: pd.DataFrame, coin: str, max_levels: int):
    l1_corr = result_df[result_df["levels"] == 1]["corr_ret30s"].values[0]
    l1_acc = directional_accuracy(df_valid, "obi_l1", threshold=0.1)

    print(f"\n{'='*60}")
    print(f"MULTI-LEVEL OBI ANALYSIS — {coin}")
    print(f"{'='*60}")
    print(f"\nBaseline (L1): corr={l1_corr:.4f}  dir_acc={l1_acc:.1%}")
    print(f"\n{'Levels':<10} {'Corr(ret30s)':<15} {'vs L1 ratio':<15} {'p-value':<12} {'Dir Acc (>0.1)':<15} {'Pass 2× rule'}")
    print("-" * 75)

    for _, row in result_df.iterrows():
        n = int(row["levels"])
        corr = row["corr_ret30s"]
        ratio = corr / l1_corr if l1_corr != 0 else float("inf")
        pval = row["p_value"]
        acc = directional_accuracy(df_valid, f"obi_l{n}", threshold=0.1)
        passes = "YES ✓" if abs(ratio) >= 2.0 else "no"
        print(f"L{n:<9} {corr:<15.4f} {ratio:<15.2f} {pval:<12.4f} {acc:<15.1%} {passes}")

    print(f"\n{'='*60}")
    print("DECISION CRITERION:")
    print(f"  OBI_LN must have |corr| >= 2× |corr_L1| = {abs(l1_corr)*2:.4f}")

    best_row = result_df.iloc[result_df["corr_ret30s"].abs().argmax()]
    best_n = int(best_row["levels"])
    best_corr = best_row["corr_ret30s"]
    best_ratio = best_corr / l1_corr if l1_corr != 0 else 0

    print(f"\n  Best: L{best_n} corr={best_corr:.4f} (ratio={best_ratio:.2f}×)")
    if abs(best_ratio) >= 2.0:
        print(f"  → IMPLEMENT multi-level OBI up to L{best_n} in direction score")
    else:
        print(f"  → DO NOT implement multi-level OBI (insufficient improvement)")
    print(f"{'='*60}\n")


def main():
    parser = argparse.ArgumentParser(description="Multi-level OBI analysis for gbot")
    parser.add_argument("--data-dir", default="./data/l2", help="Path to L2 JSONL data directory")
    parser.add_argument("--coin", default="ETH", help="Coin to analyze")
    parser.add_argument("--date", default=None, help="Date (YYYY-MM-DD), defaults to latest file")
    parser.add_argument("--max-levels", type=int, default=10, help="Maximum OBI depth to analyze")
    args = parser.parse_args()

    date = args.date or ""
    df = load_book_records(args.data_dir, args.coin, date)

    result_df, df_valid = analyze(df, max_levels=args.max_levels)
    print_report(result_df, df_valid, args.coin, args.max_levels)

    # Save CSV for further analysis
    out_path = f"obi_analysis_{args.coin}_{args.date or 'latest'}.csv"
    result_df.to_csv(out_path, index=False)
    print(f"[INFO] Results saved to {out_path}")


if __name__ == "__main__":
    main()
