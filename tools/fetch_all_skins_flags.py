#!/usr/bin/env python3
"""
Fetch all Guild Wars 2 skins from the public API and export fields useful for
manual in-game offset matching (flags, rarity, details, etc).

Usage:
  python3 tools/fetch_all_skins_flags.py
  python3 tools/fetch_all_skins_flags.py --out-json skins_flags.json --out-csv skins_flags.csv
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any


API_BASE = "https://api.guildwars2.com/v2"


def fetch_json(url: str, retries: int = 4, timeout: float = 20.0) -> Any:
    last_err: Exception | None = None
    for attempt in range(1, retries + 1):
        try:
            with urllib.request.urlopen(url, timeout=timeout) as resp:
                payload = resp.read().decode("utf-8")
                return json.loads(payload)
        except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json.JSONDecodeError) as err:
            last_err = err
            if attempt < retries:
                time.sleep(min(2.0 * attempt, 5.0))
    raise RuntimeError(f"Failed to fetch {url}: {last_err}")


def chunked(values: list[int], size: int) -> list[list[int]]:
    return [values[i : i + size] for i in range(0, len(values), size)]


def flags_to_bits(flags: list[str]) -> str:
    # API flags are symbolic; we keep explicit names for manual matching.
    return "|".join(flags) if flags else ""


def make_row(skin: dict[str, Any]) -> dict[str, Any]:
    details = skin.get("details") or {}
    flags = skin.get("flags") or []
    restrictions = skin.get("restrictions") or []
    return {
        "id": skin.get("id", 0),
        "name": skin.get("name", ""),
        "type": skin.get("type", ""),
        "rarity": skin.get("rarity", ""),
        "flags": flags,
        "flags_joined": flags_to_bits(flags),
        "has_show_in_wardrobe": "ShowInWardrobe" in flags,
        "has_override_rarity": "OverrideRarity" in flags,
        "details_type": details.get("type", ""),
        "damage_type": details.get("damage_type", ""),
        "weight_class": details.get("weight_class", ""),
        "restrictions_count": len(restrictions),
        "icon": skin.get("icon", ""),
    }


def build_feature_set(row: dict[str, Any], stratified: bool) -> set[str]:
    flags = row.get("flags") or []
    rarity = (row.get("rarity") or "").strip()
    skin_type = (row.get("type") or "").strip()
    details_type = (row.get("details_type") or "").strip()
    base = ""
    if stratified:
        base = f"type={skin_type}|details={details_type}|rarity={rarity}|"
    features: set[str] = set()
    for flag in flags:
        features.add(f"{base}flag:{flag}")
    if not flags:
        features.add(f"{base}flag:<none>")
    return features


def suggest_probe_ids(
    rows: list[dict[str, Any]], max_ids: int, stratified: bool
) -> list[dict[str, Any]]:
    if max_ids <= 0:
        return []

    # Greedy set-cover:
    # pick the id that adds the most uncovered features each step.
    uncovered: set[str] = set()
    row_features: dict[int, set[str]] = {}
    for row in rows:
        skin_id = int(row.get("id", 0))
        feats = build_feature_set(row, stratified)
        row_features[skin_id] = feats
        uncovered |= feats

    chosen: list[int] = []
    chosen_features: set[str] = set()
    remaining_ids = set(row_features.keys())
    while uncovered and remaining_ids and len(chosen) < max_ids:
        best_id = None
        best_gain = -1
        for skin_id in remaining_ids:
            gain = len(row_features[skin_id] & uncovered)
            if gain > best_gain:
                best_gain = gain
                best_id = skin_id
        if best_id is None or best_gain <= 0:
            break
        chosen.append(best_id)
        chosen_features |= row_features[best_id]
        uncovered -= row_features[best_id]
        remaining_ids.remove(best_id)

    row_by_id = {int(r["id"]): r for r in rows}
    out: list[dict[str, Any]] = []
    for skin_id in chosen:
        row = row_by_id[skin_id]
        feats = sorted(row_features[skin_id])
        out.append(
            {
                "id": skin_id,
                "name": row.get("name", ""),
                "type": row.get("type", ""),
                "rarity": row.get("rarity", ""),
                "flags": row.get("flags", []),
                "details_type": row.get("details_type", ""),
                "covers": feats,
            }
        )
    return out


def summarize_coverage(
    rows: list[dict[str, Any]], chosen: list[dict[str, Any]], stratified: bool
) -> dict[str, Any]:
    all_features: set[str] = set()
    for row in rows:
        all_features |= build_feature_set(row, stratified)

    covered: set[str] = set()
    for row in chosen:
        flags = row.get("flags") or []
        rarity = (row.get("rarity") or "").strip()
        skin_type = (row.get("type") or "").strip()
        details_type = (row.get("details_type") or "").strip()
        base = f"type={skin_type}|details={details_type}|rarity={rarity}|" if stratified else ""
        if flags:
            for flag in flags:
                covered.add(f"{base}flag:{flag}")
        else:
            covered.add(f"{base}flag:<none>")

    total = len(all_features)
    cov = len(covered & all_features)
    pct = (100.0 * cov / total) if total else 100.0
    return {
        "feature_count_total": total,
        "feature_count_covered": cov,
        "coverage_percent": round(pct, 2),
        "stratified": stratified,
    }


def detect_sparse_input(rows: list[dict[str, Any]]) -> dict[str, Any]:
    if not rows:
        return {"rows": 0, "flags_missing_pct": 100.0, "rarity_missing_pct": 100.0}
    total = len(rows)
    flags_missing = sum(1 for r in rows if not (r.get("flags") or []))
    rarity_missing = sum(1 for r in rows if not (r.get("rarity") or ""))
    return {
        "rows": total,
        "flags_missing_pct": round(100.0 * flags_missing / total, 2),
        "rarity_missing_pct": round(100.0 * rarity_missing / total, 2),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-json", default="skins_flags_full.json")
    parser.add_argument("--out-csv", default="skins_flags_full.csv")
    parser.add_argument("--batch-size", type=int, default=200)
    parser.add_argument("--sleep-ms", type=int, default=50)
    parser.add_argument(
        "--from-cache",
        default="",
        help="Read rows from existing db_api_skins.json instead of fetching from API",
    )
    parser.add_argument(
        "--suggest-max-ids",
        type=int,
        default=20,
        help="Max number of suggested probe IDs for flag verification",
    )
    parser.add_argument(
        "--suggest-stratified",
        action="store_true",
        help="Cover flags stratified by type/details_type/rarity instead of global flags only",
    )
    parser.add_argument(
        "--out-suggestions",
        default="skins_probe_suggestions.json",
        help="Where to write suggested minimal probe IDs",
    )
    args = parser.parse_args()

    if args.batch_size <= 0 or args.batch_size > 200:
        print("--batch-size must be between 1 and 200", file=sys.stderr)
        return 2

    all_rows: list[dict[str, Any]] = []
    if args.from_cache:
        with open(args.from_cache, "r", encoding="utf-8") as f:
            cached = json.load(f)
        if not isinstance(cached, list):
            raise RuntimeError("from-cache file must be an array")
        all_rows = [make_row(r) for r in cached if isinstance(r, dict)]
        print(f"Loaded {len(all_rows)} skins from cache {args.from_cache}", file=sys.stderr)
    else:
        ids_url = f"{API_BASE}/skins"
        ids = fetch_json(ids_url)
        if not isinstance(ids, list):
            raise RuntimeError("Unexpected /skins response")
        ids = [int(v) for v in ids]
        ids.sort()

        batches = chunked(ids, args.batch_size)
        total = len(batches)
        for idx, batch in enumerate(batches, start=1):
            q = urllib.parse.urlencode({"ids": ",".join(str(v) for v in batch)})
            url = f"{API_BASE}/skins?{q}"
            records = fetch_json(url)
            if not isinstance(records, list):
                raise RuntimeError(f"Unexpected batch response on batch {idx}")
            all_rows.extend(make_row(r) for r in records if isinstance(r, dict))
            print(f"[{idx}/{total}] fetched {len(records)} skins", file=sys.stderr)
            if args.sleep_ms > 0:
                time.sleep(args.sleep_ms / 1000.0)

    all_rows.sort(key=lambda r: int(r["id"]))

    with open(args.out_json, "w", encoding="utf-8") as f:
        json.dump(all_rows, f, ensure_ascii=False, indent=2)

    csv_fields = [
        "id",
        "name",
        "type",
        "rarity",
        "flags_joined",
        "has_show_in_wardrobe",
        "has_override_rarity",
        "details_type",
        "damage_type",
        "weight_class",
        "restrictions_count",
        "icon",
    ]
    with open(args.out_csv, "w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=csv_fields)
        writer.writeheader()
        for row in all_rows:
            writer.writerow({k: row.get(k, "") for k in csv_fields})

    suggestions = suggest_probe_ids(
        all_rows, args.suggest_max_ids, args.suggest_stratified
    )
    summary = summarize_coverage(all_rows, suggestions, args.suggest_stratified)
    sparse = detect_sparse_input(all_rows)
    warning = None
    if sparse["flags_missing_pct"] >= 95.0:
        warning = (
            "Input appears sparse (mostly missing `flags`). "
            "Use live API fetch mode (omit --from-cache) to get meaningful flag suggestions."
        )
    suggestions_payload = {
        "summary": summary,
        "input_quality": sparse,
        "warning": warning,
        "suggested_ids": suggestions,
    }
    with open(args.out_suggestions, "w", encoding="utf-8") as f:
        json.dump(suggestions_payload, f, ensure_ascii=False, indent=2)

    print(
        f"Wrote {len(all_rows)} rows to {args.out_json} and {args.out_csv}; "
        f"suggestions -> {args.out_suggestions} "
        f"(coverage {summary['coverage_percent']}%)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
