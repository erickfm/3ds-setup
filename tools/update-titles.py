#!/usr/bin/env python3
"""Rebuild titles.json (3DS title ID -> game name) from hax0kartik/3dsdb's eShop lists."""
import json
import re
import urllib.request
from pathlib import Path

REGIONS = ["US", "GB", "JP", "KR", "TW"]  # earlier regions win, so names are English when possible
URL = "https://raw.githubusercontent.com/hax0kartik/3dsdb/master/jsons/list_{}.json"

names = {}
for region in REGIONS:
    with urllib.request.urlopen(URL.format(region), timeout=60) as r:
        for entry in json.load(r):
            tid = entry.get("TitleID", "").upper()
            if tid.startswith("00040000") and tid not in names:  # games only, not updates/DLC
                names[tid] = re.sub(r"[™®]", "", entry["Name"]).strip()

out = Path(__file__).resolve().parent.parent / "titles.json"
out.write_text(json.dumps(names, ensure_ascii=False, sort_keys=True, indent=0) + "\n", encoding="utf-8")
print(f"{len(names)} games -> {out}")
