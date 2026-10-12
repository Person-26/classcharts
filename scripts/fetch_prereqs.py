#!/usr/bin/env python3
"""Fetch prerequisite text for every course in the current UCSB catalog.

Usage: python3 -I scripts/fetch_prereqs.py <output.json> [YYYY-MM-DD]

Uses the (undocumented, unauthenticated) Coursedog search endpoint that
backs catalog.ucsb.edu. Output maps course_id ("MATH CS 120FO") to the
freeform prerequisite text, with HTML tags stripped.
"""
import datetime
import html
import json
import re
import sys
import time
import urllib.request

URL = ("https://app.coursedog.com/api/v1/cm/ucsb/courses/search/%24filters"
       "?skip={skip}&limit={limit}&columns=code,courseGroupId,subjectCode,"
       "courseNumber,requisites&effectiveDatesRange={d},{d}")
HEADERS = {
    "x-requested-with": "catalog",
    "Origin": "https://catalog.ucsb.edu",
    "Referer": "https://catalog.ucsb.edu/",
    "content-type": "application/json",
    "User-Agent": "classcharts prereq fetch (personal use)",
}
LIMIT = 500


def strip_html(v):
    v = re.sub(r"<br\s*/?>|</p>\s*<p>", " ", v or "")
    v = html.unescape(re.sub(r"<[^>]+>", "", v))
    return re.sub(r"\s+", " ", v).strip()


def fetch(skip, day):
    body = json.dumps({"condition": "and", "filters": []}).encode()
    req = urllib.request.Request(URL.format(skip=skip, limit=LIMIT, d=day),
                                 data=body, headers=HEADERS, method="POST")
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.load(resp)


def main(dst, day=None):
    day = day or datetime.date.today().isoformat()
    out, other_keys, skip, total, dupes = {}, set(), 0, None, []
    while total is None or skip < total:
        page = fetch(skip, day)
        total = page["listLength"]
        for c in page["data"]:
            req = c.get("requisites") or {}
            other_keys |= set(req) - {"requisitesFreeform"}
            cid = re.sub(r"\s+", " ", f"{c['subjectCode']} {c['courseNumber']}").strip()
            text = strip_html((req.get("requisitesFreeform") or {}).get("value"))
            if cid in out:
                dupes.append(cid)
                text = text or out[cid]  # never let an empty version win
            out[cid] = text
        skip += LIMIT
        print(f"  {min(skip, total)}/{total}", file=sys.stderr)
        time.sleep(1)
    with open(dst, "w", encoding="utf-8") as fh:
        json.dump({"fetched": day, "courses": out}, fh, indent=1, ensure_ascii=False)
    print(f"{len(out)} courses written; other requisite keys seen: {sorted(other_keys)}")
    print(f"duplicate records merged: {sorted(set(dupes))}")


if __name__ == "__main__":
    main(*sys.argv[1:3])
