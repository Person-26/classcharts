"""Parse GOLD's structured prerequisite rows (see gold_scrape.py).

GOLD lays a rule out as rows of [_, block, connector, item]:
  - block == "As well as:" starts a new requirement that must also be met,
  - connector == "OR" starts a new alternative within the requirement,
  - an item starting "AND " adds to the current alternative.
The result uses the same shape as prereq_graph.parse(): a list of groups,
each {"alts": [...], "concurrent": False}, where an alternative is
{"courses": [...], "concurrent": bool} and exam items are "exam:<label>".
"""
import re

# College Board AP exam codes seen in GOLD rules.
AP_EXAMS = {
    "66": "AP Calc AB",
    "68": "AP Calc BC",
    "69": "AP Calc BC (AB subscore)",
    "80": "AP Physics C: Mech",
    "82": "AP Physics C: E&M",
}
EXAM_RE = re.compile(r"^(?:Taken Exam: )?(AP|IB|UCSB) (\w+) with a (?:minimum )?score of (\d+)$")
CONCURRENT = "(can be taken concurrently)"


def item_id(text):
    text = re.sub(r"^AND ", "", text).strip()
    concurrent = CONCURRENT in text
    text = text.replace(CONCURRENT, "")
    text = re.sub(r" with a minimum grade of [A-D][+-]?", "", text).strip()
    m = EXAM_RE.match(text)
    if m:
        kind, code, score = m.groups()
        name = AP_EXAMS.get(code, f"{kind} {code}") if kind == "AP" else f"{kind} {code}"
        if (kind, code) == ("UCSB", "MP"):
            name = "Math placement exam"
        return f"exam:{name} ≥ {score}", False
    return re.sub(r"\s+", " ", text), concurrent


def parse_gold(rows):
    groups = []
    for _, block, connector, item in rows:
        if block == "As well as:" or not groups:
            groups.append({"alts": [], "concurrent": False})
        if connector == "OR" or not groups[-1]["alts"]:
            groups[-1]["alts"].append({"items": []})
        groups[-1]["alts"][-1]["items"].append(item_id(item))
    for g in groups:
        alts = []
        for a in g["alts"]:
            ids = [i for i, _ in a["items"]]
            conc = all(c for _, c in a["items"])
            alt = {"courses": ids, "concurrent": conc}
            if alt not in alts:
                alts.append(alt)
        g["alts"] = alts
    return groups


def describe(groups):
    """Human-readable rule: requirements joined by ';', options by ' or '."""
    parts = []
    for g in groups:
        opts = []
        for a in g["alts"]:
            names = [c.removeprefix("exam:") for c in a["courses"]]
            s = " and ".join(names)
            if len(names) > 1 and len(g["alts"]) > 1:
                s = f"({s})"
            if a["concurrent"]:
                s += " [may be concurrent]"
            opts.append(s)
        parts.append(" or ".join(opts))
    return "; ".join(parts)
