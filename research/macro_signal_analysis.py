#!/usr/bin/env python3
"""
Macro Signal Analysis — Funding rates, OI, basis, volume as predictive signals.

Fetches historical data from Hyperliquid API and cross-references with L2/trade
data to find alpha that microstructure L2 features couldn't provide.

Signals tested:
  1. Funding rate level & changes (mean-reversion: extreme funding -> price reversal)
  2. Funding rate vs premium divergence
  3. OI changes (rising OI + price = new money, rising OI - price = shorts piling in)
  4. Volume spikes relative to OI (high vol/OI = potential reversal)
  5. Mark-Oracle basis (premium/discount as directional signal)
  6. Cross-coin funding divergence (relative value)
  7. Candle momentum at longer timeframes (1h, 4h)

Usage:
  python3 research/macro_signal_analysis.py --coins BTC,ETH,SOL --days 7
"""

import argparse
import json
import os
import sys
import time
from datetime import datetime, timezone, timedelta
from collections import defaultdict

import numpy as np

try:
    import requests
except ImportError:
    print("Installing requests...")
    os.system(f"{sys.executable} -m pip install requests -q")
    import requests

try:
    from scipy.stats import spearmanr
except ImportError:
    print("Installing scipy...")
    os.system(f"{sys.executable} -m pip install scipy -q")
    from scipy.stats import spearmanr


API_URL = "https://api.hyperliquid.xyz/info"
HEADERS = {"Content-Type": "application/json"}


def api_post(body: dict, retries: int = 3) -> dict:
    for attempt in range(retries):
        try:
            r = requests.post(API_URL, json=body, headers=HEADERS, timeout=30)
            r.raise_for_status()
            return r.json()
        except Exception as e:
            if attempt < retries - 1:
                time.sleep(2 ** attempt)
            else:
                raise


# ─── Data fetchers ────────────────────────────────────────────────────────────

def fetch_funding_history(coin: str, start_ms: int, end_ms: int) -> list:
    """Fetch hourly funding rate history for a coin."""
    print(f"  Fetching funding history for {coin}...")
    data = api_post({
        "type": "fundingHistory",
        "coin": coin,
        "startTime": start_ms,
        "endTime": end_ms,
    })
    return sorted(data, key=lambda x: x["time"])


def fetch_candles(coin: str, interval: str, start_ms: int, end_ms: int) -> list:
    """Fetch OHLCV candles."""
    print(f"  Fetching {interval} candles for {coin}...")
    data = api_post({
        "type": "candleSnapshot",
        "req": {
            "coin": coin,
            "interval": interval,
            "startTime": start_ms,
            "endTime": end_ms,
        },
    })
    return sorted(data, key=lambda x: x["t"])


def fetch_meta_and_ctxs() -> tuple:
    """Fetch current metaAndAssetCtxs (OI, funding, volume, basis for all coins)."""
    print("  Fetching metaAndAssetCtxs (current snapshot)...")
    data = api_post({"type": "metaAndAssetCtxs"})
    meta = data[0]
    ctxs = data[1]
    result = {}
    for i, asset in enumerate(meta["universe"]):
        name = asset["name"]
        if i < len(ctxs):
            result[name] = ctxs[i]
    return meta, result


def fetch_predicted_fundings() -> dict:
    """Fetch predicted next funding rates for all coins."""
    print("  Fetching predicted fundings...")
    data = api_post({"type": "predictedFundings"})
    result = {}
    for item in data:
        coin = item[0]
        for venue_info in item[1]:
            if venue_info[0] == "Perp":
                result[coin] = {
                    "predicted_rate": float(venue_info[1].get("fundingRate", 0)),
                    "next_time": venue_info[1].get("nextFundingTime", 0),
                }
    return result


# ─── Analysis functions ───────────────────────────────────────────────────────

def analyze_funding_vs_returns(coin: str, funding_history: list, candles_1h: list):
    """
    Test: does extreme funding predict price reversal?
    Hypothesis: very positive funding -> longs pay shorts -> price tends to drop.
    """
    if len(funding_history) < 10 or len(candles_1h) < 10:
        return None

    # Build funding rate time series (hourly)
    funding_by_hour = {}
    for f in funding_history:
        hour_ts = (f["time"] // 3600000) * 3600000
        funding_by_hour[hour_ts] = float(f["fundingRate"])

    # Build candle returns (1h, 4h, 8h forward)
    candle_by_hour = {}
    for c in candles_1h:
        hour_ts = (c["t"] // 3600000) * 3600000
        candle_by_hour[hour_ts] = {
            "close": float(c["c"]),
            "open": float(c["o"]),
            "high": float(c["h"]),
            "low": float(c["l"]),
            "volume": float(c["v"]),
            "trades": c["n"],
        }

    # Align timestamps
    common_ts = sorted(set(funding_by_hour.keys()) & set(candle_by_hour.keys()))
    if len(common_ts) < 20:
        return None

    results = {}

    # Test multiple forward horizons
    for fwd_hours in [1, 2, 4, 8, 24]:
        funding_vals = []
        fwd_returns = []

        for i, ts in enumerate(common_ts):
            fwd_ts = ts + fwd_hours * 3600000
            if fwd_ts in candle_by_hour and ts in candle_by_hour:
                fr = funding_by_hour[ts]
                price_now = candle_by_hour[ts]["close"]
                price_fwd = candle_by_hour[fwd_ts]["close"]
                ret = (price_fwd - price_now) / price_now * 10000  # bps
                funding_vals.append(fr)
                fwd_returns.append(ret)

        if len(funding_vals) < 20:
            continue

        funding_arr = np.array(funding_vals)
        returns_arr = np.array(fwd_returns)

        corr, pval = spearmanr(funding_arr, returns_arr)

        # Win rate: when funding > 75th percentile, short. When < 25th, long.
        p75 = np.percentile(funding_arr, 75)
        p25 = np.percentile(funding_arr, 25)

        high_funding = returns_arr[funding_arr > p75]
        low_funding = returns_arr[funding_arr < p25]

        short_wr = (high_funding < 0).mean() if len(high_funding) > 0 else 0
        long_wr = (low_funding > 0).mean() if len(low_funding) > 0 else 0
        short_avg = -high_funding.mean() if len(high_funding) > 0 else 0
        long_avg = low_funding.mean() if len(low_funding) > 0 else 0

        results[f"fwd_{fwd_hours}h"] = {
            "corr": corr,
            "pval": pval,
            "n": len(funding_vals),
            "short_when_high_wr": short_wr,
            "short_when_high_avg_bps": short_avg,
            "long_when_low_wr": long_wr,
            "long_when_low_avg_bps": long_avg,
            "funding_p25": p25,
            "funding_p75": p75,
        }

    return results


def analyze_funding_change_signal(funding_history: list, candles_1h: list):
    """
    Test: does the CHANGE in funding rate predict returns?
    Hypothesis: funding accelerating positive -> overcrowded long -> reversal.
    """
    if len(funding_history) < 20:
        return None

    funding_by_hour = {}
    for f in funding_history:
        hour_ts = (f["time"] // 3600000) * 3600000
        funding_by_hour[hour_ts] = float(f["fundingRate"])

    candle_by_hour = {}
    for c in candles_1h:
        hour_ts = (c["t"] // 3600000) * 3600000
        candle_by_hour[hour_ts] = float(c["c"])

    sorted_ts = sorted(funding_by_hour.keys())

    results = {}
    for fwd_hours in [1, 4, 8, 24]:
        delta_funding = []
        fwd_returns = []

        for i in range(1, len(sorted_ts)):
            ts = sorted_ts[i]
            ts_prev = sorted_ts[i - 1]
            fwd_ts = ts + fwd_hours * 3600000

            if fwd_ts in candle_by_hour and ts in candle_by_hour:
                df = funding_by_hour[ts] - funding_by_hour[ts_prev]
                price_now = candle_by_hour[ts]
                price_fwd = candle_by_hour[fwd_ts]
                ret = (price_fwd - price_now) / price_now * 10000
                delta_funding.append(df)
                fwd_returns.append(ret)

        if len(delta_funding) < 20:
            continue

        corr, pval = spearmanr(delta_funding, fwd_returns)
        results[f"fwd_{fwd_hours}h"] = {
            "corr": corr,
            "pval": pval,
            "n": len(delta_funding),
        }

    return results


def analyze_basis_signal(candles_1h: list, funding_history: list):
    """
    Test: does mark-oracle premium predict returns?
    Premium is embedded in funding: funding = clamp(premium + interest_rate).
    We use funding premium field directly.
    """
    if len(funding_history) < 20:
        return None

    premium_by_hour = {}
    for f in funding_history:
        hour_ts = (f["time"] // 3600000) * 3600000
        premium_by_hour[hour_ts] = float(f.get("premium", 0))

    candle_by_hour = {}
    for c in candles_1h:
        hour_ts = (c["t"] // 3600000) * 3600000
        candle_by_hour[hour_ts] = float(c["c"])

    common_ts = sorted(set(premium_by_hour.keys()) & set(candle_by_hour.keys()))
    if len(common_ts) < 20:
        return None

    results = {}
    for fwd_hours in [1, 4, 8, 24]:
        premiums = []
        fwd_returns = []

        for ts in common_ts:
            fwd_ts = ts + fwd_hours * 3600000
            if fwd_ts in candle_by_hour:
                premiums.append(premium_by_hour[ts])
                ret = (candle_by_hour[fwd_ts] - candle_by_hour[ts]) / candle_by_hour[ts] * 10000
                fwd_returns.append(ret)

        if len(premiums) < 20:
            continue

        corr, pval = spearmanr(premiums, fwd_returns)
        results[f"fwd_{fwd_hours}h"] = {
            "corr": corr,
            "pval": pval,
            "n": len(premiums),
        }

    return results


def analyze_volume_momentum(candles_1h: list):
    """
    Test: does volume × price direction predict continuation?
    High volume + up candle = bullish continuation?
    """
    if len(candles_1h) < 30:
        return None

    results = {}
    for fwd_hours in [1, 4, 8]:
        signals = []
        fwd_returns = []

        for i in range(len(candles_1h) - fwd_hours):
            c = candles_1h[i]
            c_fwd = candles_1h[i + fwd_hours]

            close = float(c["c"])
            open_ = float(c["o"])
            vol = float(c["v"])
            ret_candle = (close - open_) / open_ * 10000 if open_ > 0 else 0

            # Signal: signed volume (positive = bullish volume, negative = bearish)
            signed_vol = ret_candle * vol  # direction × magnitude

            fwd_close = float(c_fwd["c"])
            fwd_ret = (fwd_close - close) / close * 10000

            signals.append(signed_vol)
            fwd_returns.append(fwd_ret)

        if len(signals) < 20:
            continue

        corr, pval = spearmanr(signals, fwd_returns)

        # Decile analysis
        sig_arr = np.array(signals)
        ret_arr = np.array(fwd_returns)
        p90 = np.percentile(sig_arr, 90)
        p10 = np.percentile(sig_arr, 10)

        bullish = ret_arr[sig_arr > p90]
        bearish = ret_arr[sig_arr < p10]

        results[f"fwd_{fwd_hours}h"] = {
            "corr": corr,
            "pval": pval,
            "n": len(signals),
            "bullish_top_decile_avg_bps": bullish.mean() if len(bullish) > 0 else 0,
            "bearish_bot_decile_avg_bps": bearish.mean() if len(bearish) > 0 else 0,
        }

    return results


def analyze_candle_momentum(candles: list, interval_label: str):
    """
    Test: does price momentum at longer timeframes predict continuation?
    This is what we couldn't test with L2 data (only had 1s resolution, 45s horizon).
    """
    if len(candles) < 30:
        return None

    results = {}
    for fwd_n in [1, 2, 4, 8]:
        past_returns = []
        fwd_returns = []

        for i in range(1, len(candles) - fwd_n):
            c_prev = candles[i - 1]
            c_curr = candles[i]
            c_fwd = candles[i + fwd_n]

            close_prev = float(c_prev["c"])
            close_curr = float(c_curr["c"])
            close_fwd = float(c_fwd["c"])

            past_ret = (close_curr - close_prev) / close_prev * 10000
            fwd_ret = (close_fwd - close_curr) / close_curr * 10000

            past_returns.append(past_ret)
            fwd_returns.append(fwd_ret)

        if len(past_returns) < 20:
            continue

        past_arr = np.array(past_returns)
        fwd_arr = np.array(fwd_returns)

        corr, pval = spearmanr(past_arr, fwd_arr)

        # Conditional WR after fees
        for threshold_bps in [0, 5, 10, 20]:
            long_mask = past_arr > threshold_bps
            short_mask = past_arr < -threshold_bps
            n_long = long_mask.sum()
            n_short = short_mask.sum()

            long_wr = (fwd_arr[long_mask] > 0).mean() if n_long > 5 else 0
            long_avg = fwd_arr[long_mask].mean() if n_long > 5 else 0
            short_wr = (fwd_arr[short_mask] < 0).mean() if n_short > 5 else 0
            short_avg = -fwd_arr[short_mask].mean() if n_short > 5 else 0

            results[f"fwd_{fwd_n}x{interval_label}_past>{threshold_bps}bps"] = {
                "corr": corr if threshold_bps == 0 else None,
                "n_long": int(n_long),
                "long_wr": long_wr,
                "long_avg_bps": long_avg,
                "long_net_bps": long_avg - 3.0,
                "n_short": int(n_short),
                "short_wr": short_wr,
                "short_avg_bps": short_avg,
                "short_net_bps": short_avg - 3.0,
            }

    return results


def analyze_cross_coin_funding(all_funding: dict, all_candles: dict):
    """
    Test: does relative funding between coins predict relative returns?
    If BTC funding >> ETH funding, does ETH outperform BTC?
    """
    coins = list(all_funding.keys())
    if len(coins) < 2:
        return None

    results = {}
    for i in range(len(coins)):
        for j in range(i + 1, len(coins)):
            c1, c2 = coins[i], coins[j]
            f1_by_hour = {(f["time"] // 3600000) * 3600000: float(f["fundingRate"]) for f in all_funding[c1]}
            f2_by_hour = {(f["time"] // 3600000) * 3600000: float(f["fundingRate"]) for f in all_funding[c2]}

            p1_by_hour = {}
            p2_by_hour = {}
            for c in all_candles.get(c1, []):
                hour_ts = (c["t"] // 3600000) * 3600000
                p1_by_hour[hour_ts] = float(c["c"])
            for c in all_candles.get(c2, []):
                hour_ts = (c["t"] // 3600000) * 3600000
                p2_by_hour[hour_ts] = float(c["c"])

            common = sorted(set(f1_by_hour) & set(f2_by_hour) & set(p1_by_hour) & set(p2_by_hour))

            for fwd_hours in [4, 8, 24]:
                fund_diff = []
                ret_diff = []
                for ts in common:
                    fwd_ts = ts + fwd_hours * 3600000
                    if fwd_ts in p1_by_hour and fwd_ts in p2_by_hour:
                        fd = f1_by_hour[ts] - f2_by_hour[ts]
                        r1 = (p1_by_hour[fwd_ts] - p1_by_hour[ts]) / p1_by_hour[ts] * 10000
                        r2 = (p2_by_hour[fwd_ts] - p2_by_hour[ts]) / p2_by_hour[ts] * 10000
                        ret_diff.append(r2 - r1)  # c2 - c1: if c1 funding > c2, expect c2 to outperform
                        fund_diff.append(fd)

                if len(fund_diff) < 20:
                    continue

                corr, pval = spearmanr(fund_diff, ret_diff)
                results[f"{c1}_vs_{c2}_fwd_{fwd_hours}h"] = {
                    "corr": corr,
                    "pval": pval,
                    "n": len(fund_diff),
                }

    return results


# ─── Main ─────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Macro signal analysis")
    parser.add_argument("--coins", default="BTC,ETH,SOL", help="Coins to analyze")
    parser.add_argument("--days", type=int, default=7, help="Days of history to fetch")
    args = parser.parse_args()

    coins = [c.strip() for c in args.coins.split(",")]
    now_ms = int(time.time() * 1000)
    start_ms = now_ms - args.days * 86400 * 1000

    print(f"=" * 70)
    print(f"MACRO SIGNAL ANALYSIS")
    print(f"Coins: {', '.join(coins)}")
    print(f"Period: {args.days} days ({datetime.fromtimestamp(start_ms/1000, tz=timezone.utc).strftime('%Y-%m-%d')} to now)")
    print(f"=" * 70)

    # ── Fetch data ────────────────────────────────────────────────────────
    print("\n[1/3] Fetching data from Hyperliquid API...")

    all_funding = {}
    all_candles_1h = {}
    all_candles_4h = {}

    for coin in coins:
        all_funding[coin] = fetch_funding_history(coin, start_ms, now_ms)
        all_candles_1h[coin] = fetch_candles(coin, "1h", start_ms, now_ms)
        all_candles_4h[coin] = fetch_candles(coin, "4h", start_ms, now_ms)
        time.sleep(0.5)  # Rate limit courtesy

    # Current snapshot
    meta, current_ctxs = fetch_meta_and_ctxs()
    predicted = fetch_predicted_fundings()

    print(f"\n  Data summary:")
    for coin in coins:
        n_fund = len(all_funding[coin])
        n_1h = len(all_candles_1h[coin])
        n_4h = len(all_candles_4h[coin])
        ctx = current_ctxs.get(coin, {})
        oi = ctx.get("openInterest", "?")
        funding = ctx.get("funding", "?")
        premium = ctx.get("premium", "?")
        vol = ctx.get("dayNtlVlm", "?")
        pred = predicted.get(coin, {}).get("predicted_rate", "?")
        print(f"  {coin:6s}: {n_fund:4d} funding pts, {n_1h:4d} 1h candles, {n_4h:4d} 4h candles")
        print(f"          OI={oi} funding={funding} premium={premium} 24hVol=${vol} predicted={pred}")

    # ── Analysis ──────────────────────────────────────────────────────────
    print(f"\n[2/3] Running signal analysis...\n")

    print("=" * 70)
    print("SIGNAL 1: FUNDING RATE LEVEL -> FUTURE RETURN")
    print("  Hypothesis: extreme funding predicts reversal")
    print("=" * 70)
    for coin in coins:
        res = analyze_funding_vs_returns(coin, all_funding[coin], all_candles_1h[coin])
        if not res:
            print(f"\n  {coin}: insufficient data")
            continue
        print(f"\n  {coin}:")
        print(f"  {'Horizon':<10s} {'Corr':>8s} {'p-val':>10s} {'N':>5s} | {'ShortWR':>8s} {'ShAvg':>8s} {'LongWR':>8s} {'LnAvg':>8s}")
        print(f"  {'-'*75}")
        for k, v in sorted(res.items()):
            star = " ***" if abs(v["corr"]) > 0.1 and v["pval"] < 0.05 else " *" if v["pval"] < 0.05 else ""
            print(f"  {k:<10s} {v['corr']:>+8.4f} {v['pval']:>10.2e} {v['n']:>5d} | "
                  f"{100*v['short_when_high_wr']:>7.1f}% {v['short_when_high_avg_bps']:>+7.1f} "
                  f"{100*v['long_when_low_wr']:>7.1f}% {v['long_when_low_avg_bps']:>+7.1f}{star}")

    print(f"\n{'=' * 70}")
    print("SIGNAL 2: FUNDING RATE CHANGE (DELTA) -> FUTURE RETURN")
    print("  Hypothesis: accelerating funding predicts overcrowding -> reversal")
    print("=" * 70)
    for coin in coins:
        res = analyze_funding_change_signal(all_funding[coin], all_candles_1h[coin])
        if not res:
            print(f"\n  {coin}: insufficient data")
            continue
        print(f"\n  {coin}:")
        for k, v in sorted(res.items()):
            star = " ***" if abs(v["corr"]) > 0.1 and v["pval"] < 0.05 else " *" if v["pval"] < 0.05 else ""
            print(f"  {k:<10s} corr={v['corr']:>+.4f} p={v['pval']:.2e} n={v['n']}{star}")

    print(f"\n{'=' * 70}")
    print("SIGNAL 3: MARK-ORACLE PREMIUM -> FUTURE RETURN")
    print("  Hypothesis: high premium = overbought -> reversal")
    print("=" * 70)
    for coin in coins:
        res = analyze_basis_signal(all_candles_1h[coin], all_funding[coin])
        if not res:
            print(f"\n  {coin}: insufficient data")
            continue
        print(f"\n  {coin}:")
        for k, v in sorted(res.items()):
            star = " ***" if abs(v["corr"]) > 0.1 and v["pval"] < 0.05 else " *" if v["pval"] < 0.05 else ""
            print(f"  {k:<10s} corr={v['corr']:>+.4f} p={v['pval']:.2e} n={v['n']}{star}")

    print(f"\n{'=' * 70}")
    print("SIGNAL 4: VOLUME × DIRECTION (signed volume) -> FUTURE RETURN")
    print("  Hypothesis: high bullish volume = continuation")
    print("=" * 70)
    for coin in coins:
        res = analyze_volume_momentum(all_candles_1h[coin])
        if not res:
            print(f"\n  {coin}: insufficient data")
            continue
        print(f"\n  {coin}:")
        for k, v in sorted(res.items()):
            star = " ***" if v.get("corr") and abs(v["corr"]) > 0.1 and v["pval"] < 0.05 else ""
            print(f"  {k:<10s} corr={v['corr']:>+.4f} p={v['pval']:.2e} "
                  f"bull_top10={v['bullish_top_decile_avg_bps']:>+.1f}bps "
                  f"bear_bot10={v['bearish_bot_decile_avg_bps']:>+.1f}bps{star}")

    print(f"\n{'=' * 70}")
    print("SIGNAL 5: CANDLE MOMENTUM (1h) -> FUTURE RETURN (after fees)")
    print("  Hypothesis: 1h momentum predicts next 1-8h")
    print("=" * 70)
    for coin in coins:
        res = analyze_candle_momentum(all_candles_1h[coin], "1h")
        if not res:
            print(f"\n  {coin}: insufficient data")
            continue
        print(f"\n  {coin}:")
        print(f"  {'Signal':<35s} {'Corr':>6s} | {'nL':>4s} {'WR_L':>5s} {'avgL':>7s} {'netL':>7s} | {'nS':>4s} {'WR_S':>5s} {'avgS':>7s} {'netS':>7s}")
        print(f"  {'-'*100}")
        for k, v in sorted(res.items()):
            corr_str = f"{v['corr']:>+.3f}" if v['corr'] is not None else "  —  "
            net_l = v['long_net_bps']
            net_s = v['short_net_bps']
            flag = " <<<" if net_l > 0 or net_s > 0 else ""
            print(f"  {k:<35s} {corr_str} | {v['n_long']:>4d} {100*v['long_wr']:>4.0f}% {v['long_avg_bps']:>+6.1f} {net_l:>+6.1f} | "
                  f"{v['n_short']:>4d} {100*v['short_wr']:>4.0f}% {v['short_avg_bps']:>+6.1f} {net_s:>+6.1f}{flag}")

    print(f"\n{'=' * 70}")
    print("SIGNAL 6: CANDLE MOMENTUM (4h) -> FUTURE RETURN (after fees)")
    print("=" * 70)
    for coin in coins:
        res = analyze_candle_momentum(all_candles_4h[coin], "4h")
        if not res:
            print(f"\n  {coin}: insufficient data")
            continue
        print(f"\n  {coin}:")
        print(f"  {'Signal':<35s} {'Corr':>6s} | {'nL':>4s} {'WR_L':>5s} {'avgL':>7s} {'netL':>7s} | {'nS':>4s} {'WR_S':>5s} {'avgS':>7s} {'netS':>7s}")
        print(f"  {'-'*100}")
        for k, v in sorted(res.items()):
            corr_str = f"{v['corr']:>+.3f}" if v['corr'] is not None else "  —  "
            net_l = v['long_net_bps']
            net_s = v['short_net_bps']
            flag = " <<<" if net_l > 0 or net_s > 0 else ""
            print(f"  {k:<35s} {corr_str} | {v['n_long']:>4d} {100*v['long_wr']:>4.0f}% {v['long_avg_bps']:>+6.1f} {net_l:>+6.1f} | "
                  f"{v['n_short']:>4d} {100*v['short_wr']:>4.0f}% {v['short_avg_bps']:>+6.1f} {net_s:>+6.1f}{flag}")

    print(f"\n{'=' * 70}")
    print("SIGNAL 7: CROSS-COIN FUNDING DIVERGENCE")
    print("  Hypothesis: if BTC funding >> ETH, ETH outperforms BTC")
    print("=" * 70)
    res = analyze_cross_coin_funding(all_funding, all_candles_1h)
    if res:
        for k, v in sorted(res.items(), key=lambda x: -abs(x[1]["corr"])):
            star = " ***" if abs(v["corr"]) > 0.1 and v["pval"] < 0.05 else " *" if v["pval"] < 0.05 else ""
            print(f"  {k:<30s} corr={v['corr']:>+.4f} p={v['pval']:.2e} n={v['n']}{star}")
    else:
        print("  Insufficient data")

    # ── Summary ───────────────────────────────────────────────────────────
    print(f"\n{'=' * 70}")
    print("CURRENT MARKET SNAPSHOT")
    print("=" * 70)
    print(f"  {'Coin':<6s} {'OI':>12s} {'Funding':>10s} {'Premium':>10s} {'24h Vol':>14s} {'Predicted':>10s}")
    print(f"  {'-'*68}")
    for coin in coins:
        ctx = current_ctxs.get(coin, {})
        pred = predicted.get(coin, {})
        oi = ctx.get("openInterest", "?")
        funding = ctx.get("funding", "?")
        premium = ctx.get("premium", "?")
        vol = ctx.get("dayNtlVlm", "?")
        pred_rate = pred.get("predicted_rate", "?")
        print(f"  {coin:<6s} {oi:>12s} {funding:>10s} {premium:>10s} ${vol:>13s} {pred_rate:>10s}")

    print(f"\n{'=' * 70}")
    print("CONCLUSIONS")
    print("=" * 70)


if __name__ == "__main__":
    main()
