#!/usr/bin/env python3
"""Normalize the UCSB courses report export into a tidy CSV.

Usage: python3 -I scripts/clean_courses.py <input.csv> <output.csv> [prereqs.json [gold.json]]

prereqs.json is the output of fetch_prereqs.py; if given, prerequisite
text from the current catalog is joined on course_id. gold.json (from
gold_scrape.py) adds the registration rules GOLD enforces, where GOLD lists
the course.

The input file is never modified. A summary of fixes, dropped rows and
anything ambiguous is printed to stdout.
"""
import csv
import json
import re
import sys
import unicodedata
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gold_rules import describe, parse_gold  # noqa: E402

# Hand fixes for U+FFFD replacement characters found in the export.
FFFD_FIXES = {
    "preparatory�course": "preparatory course",
    "�Mediterranean Crossroads.�": "“Mediterranean Crossroads.”",
    "157A-B-C. ��": "157A-B-C.",
}

# "W L CS" is the official catalog code; one row used "W&L CS".
SUBJECT_FIXES = {"PSY  W": "PSY W", "W&L CS": "W L CS"}

# CCS-only programs whose base code does not otherwise exist in the data.
CCS_ONLY = {"BIOL CS": "BIOL", "CMPTGCS": "CMPTG", "MARINCS": "MARIN", "W L CS": "W L"}

GE_AREAS = {
    "Area A (Course 1): English Reading and Composition": "A1",
    "Area A (Course 2): English Reading and Composition": "A2",
    "Area B: Language": "B",
    "Area C: Science, Mathematics, and Technology": "C",
    "Area D: Social Sciences": "D",
    "Area E: Culture and Thought": "E",
    "Area F: Arts": "F",
    "Area G: Literature": "G",
}
SPECIAL_AREAS = {
    "Writing": "writing",
    "World Cultures": "world_cultures",
    "Ethnicity": "ethnicity",
    "European Traditions": "european_traditions",
    "Quantitative Relationships": "quantitative",
}
UNIV_REQS = {
    "American History & Institutions": "american_history_institutions",
    "Entry Level Writing": "entry_level_writing",
}

WORD_NUMS = {w: i for i, w in enumerate(
    "zero one two three four five six seven eight nine ten eleven twelve".split())}

OUT_COLS = (
    ["course_id", "subject", "subject_base", "is_online", "is_ccs",
     "course_number", "course_num", "course_suffix", "level", "title",
     "units_type", "units_min", "units_max", "description",
     "advisor_comments", "repeat_comments", "is_repeatable",
     "repeat_max_units", "online_version_of", "online_version", "has_prereq", "prereq_text", "prereq_in_catalog", "prereq_gold", "prereq_gold_quarter",
     "ge_areas", "special_areas", "university_requirements"]
    + [f"ge_{c}" for c in GE_AREAS.values()]
    + [f"sa_{c}" for c in SPECIAL_AREAS.values()]
    + [f"ur_{c}" for c in UNIV_REQS.values()]
)


def clean_text(v):
    v = unicodedata.normalize("NFC", v)
    for bad, good in FFFD_FIXES.items():
        v = v.replace(bad, good)
    v = v.replace("‐", "-")
    v = re.sub(r"\s+", " ", v).strip()  # \xa0, \t, \n and runs of spaces
    return "" if v == "-" else v


def num(v):
    return float(v) if v else None


def fmt(x):
    return "" if x is None else (str(int(x)) if x == int(x) else str(x))


def units(fixed, vmin, vmax):
    f, mn, mx = num(fixed), num(vmin), num(vmax)
    if f and (mx in (None, 0) or (mn in (None, 0) and mx == f)):
        return "fixed", f, f
    if mx:
        return "variable", mn or 0.0, mx
    raise ValueError(f"unclassified units {(fixed, vmin, vmax)}")


def split_list(v, names, ge=False):
    if not v:
        return []
    parts = re.split(r",\s*(?=Area )", v) if ge else [p.strip() for p in v.split(",")]
    for p in parts:
        if p not in names:
            raise ValueError(f"unknown area {p!r}")
    order = list(names)
    return sorted(parts, key=order.index)


def gold_key(cid):
    return re.sub(r"\s+", "", cid.replace("&", " "))


def gold_rule(rec, cid):
    """GOLD's enforced rule as text; "none" when GOLD lists the course with no rule."""
    if not rec:
        return ""
    groups = []
    for g in parse_gold(rec["rows"]):
        g["alts"] = [a for a in g["alts"]  # drop self-repeats
                     if gold_key(cid) not in map(gold_key, a["courses"])]
        if g["alts"]:
            groups.append(g)
    return describe(groups) or "none"


def level(n):
    if n < 100:
        return "lower_division"
    if n < 200:
        return "upper_division"
    if n < 300:
        return "graduate"
    if n < 500:
        return "professional"
    return "graduate_special_study"


def repeat_info(text):
    t = text.lower()
    if re.search(r"(may|can)\s*not be repeated|not repeatable", t):
        rep = False
    elif re.search(r"(may|can)( only)? be repeated|repeatable", t):
        rep = True
    else:
        rep = None
    m = re.search(r"(?:maximum of|up to|max(?:imum)?) (\d+(?:\.\d+)?|[a-z]+) units", t)
    mx = None
    if m:
        g = m.group(1)
        mx = float(g) if g[0].isdigit() else WORD_NUMS.get(g)
    return rep, mx


def main(src, dst, prereq_json=None, gold_json=None):
    prereqs, gold = {}, {}
    if gold_json:
        with open(gold_json, encoding="utf-8") as fh:
            # GOLD spells some codes differently ("W&L CS", "ARTHI W" vs
            # "W L CS", "ARTHIW"); join on a space-free key.
            gold = {gold_key(k): v for k, v in json.load(fh)["courses"].items()}
    if prereq_json:
        with open(prereq_json, encoding="utf-8") as fh:
            prereqs = json.load(fh)["courses"]
    with open(src, encoding="utf-8-sig", newline="") as fh:
        rows = list(csv.DictReader(fh))
    log = []
    subjects = {SUBJECT_FIXES.get(clean_text(r["Subject & Prefix"]), clean_text(r["Subject & Prefix"]))
                for r in rows}
    fffd_left = 0
    out, seen = [], {}
    for r in rows:
        c = {k: clean_text(v) for k, v in r.items()}
        fffd_left += sum("�" in v for v in c.values())
        subj = SUBJECT_FIXES.get(c["Subject & Prefix"], c["Subject & Prefix"])
        if c["Subject & Prefix"] != subj:
            log.append(f"subject {c['Subject & Prefix']!r} -> {subj!r}")

        base, online, ccs = subj, False, False
        if subj in CCS_ONLY:
            base, ccs = CCS_ONLY[subj], True
        elif m := re.fullmatch(r"(.+?) ?CS", subj):
            if m.group(1) in subjects:
                base, ccs = m.group(1), True
        elif m := re.fullmatch(r"(.+?) ?W", subj):
            if m.group(1) in subjects:
                base, online = m.group(1), True

        cn = c["Course Number"]
        m = re.fullmatch(r"(\d+)([A-Z]*)", cn)
        if not m:
            raise ValueError(f"bad course number {cn!r}")
        n, suffix = int(m.group(1)), m.group(2)

        utype, umin, umax = units(c["Units - Units Fixed - Unit Value"],
                                  c["Units - Units Variable - Min"],
                                  c["Units - Units Variable - Max"])

        adv = re.sub(r"^Comments?:\s*", "", c["Advisor Enrollment Comments"])
        rpt = re.sub(r"^Comments?:\s*", "", c["Repeat Comments"])
        repeatable, rmax = repeat_info(f"{adv} {rpt}")

        ov = ""
        if online:
            ov = f"{base} {cn}"

        ge = split_list(c["General Subject Areas"], GE_AREAS, ge=True)
        sa = split_list(c["Special Subject Areas"], SPECIAL_AREAS)
        ur = split_list(c["University Requirements"], UNIV_REQS)

        rec = {
            "course_id": f"{subj} {cn}",
            "subject": subj, "subject_base": base,
            "is_online": online, "is_ccs": ccs,
            "course_number": cn, "course_num": n, "course_suffix": suffix,
            "level": level(n), "title": c["Full Course Title"],
            "units_type": utype, "units_min": fmt(umin), "units_max": fmt(umax),
            "description": c["Course Description"],
            "advisor_comments": adv, "repeat_comments": rpt,
            "is_repeatable": "" if repeatable is None else repeatable,
            "repeat_max_units": fmt(rmax),
            "online_version_of": ov,
            "has_prereq": {"Yes": True, "No": False}[c["Prerequisites"]],
            "prereq_text": re.sub(r"^None\.?$", "", prereqs.get(f"{subj} {cn}", ""), flags=re.I),
            "prereq_in_catalog": f"{subj} {cn}" in prereqs if prereqs else "",
            "prereq_gold": gold_rule(gold.get(gold_key(f"{subj} {cn}")), f"{subj} {cn}"),
            "prereq_gold_quarter": gold.get(gold_key(f"{subj} {cn}"), {}).get("quarter", ""),
            "ge_areas": "; ".join(GE_AREAS[a] for a in ge),
            "special_areas": "; ".join(sa),
            "university_requirements": "; ".join(ur),
        }
        rec.update({f"ge_{v}": k in ge for k, v in GE_AREAS.items()})
        rec.update({f"sa_{v}": k in sa for k, v in SPECIAL_AREAS.items()})
        rec.update({f"ur_{v}": k in ur for k, v in UNIV_REQS.items()})

        key = rec["course_id"]
        if key in seen:
            prev = seen[key]
            # Keep the row that came through cleaner (fewer blank unit fields
            # and no "Comments:" prefix in the raw export).
            score = lambda raw: (raw["Units - Units Variable - Min"].strip() != "-",
                                 not raw["Advisor Enrollment Comments"].startswith("Comments"))
            if score(r) > score(prev[1]):
                out[out.index(prev[0])] = rec
                seen[key] = (rec, r)
            log.append(f"dropped duplicate {key}")
            continue
        seen[key] = (rec, r)
        out.append(rec)

    # Link online sections to their in-person course only when it exists.
    by_id = {o["course_id"]: o for o in out}
    for o in out:
        o.setdefault("online_version", "")
        base_course = by_id.get(o["online_version_of"])
        if base_course:
            base_course["online_version"] = o["course_id"]
        else:
            o["online_version_of"] = ""

    assert fffd_left == 0, f"{fffd_left} fields still contain U+FFFD"
    ambiguous = sorted({s for s in subjects if re.fullmatch(r".+ ?W", s)
                        and not any(o["subject"] == s and o["is_online"] for o in out)})

    with open(dst, "w", encoding="utf-8", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=OUT_COLS)
        w.writeheader()
        w.writerows(out)

    print(f"rows in: {len(rows)}  rows out: {len(out)}  columns: {len(OUT_COLS)}")
    for line in sorted(set(log)):
        print(" ", line)
    print("  W-suffixed subjects NOT treated as online:", ambiguous)


if __name__ == "__main__":
    main(*sys.argv[1:5])
