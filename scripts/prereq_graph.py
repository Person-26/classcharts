#!/usr/bin/env python3
"""Parse prerequisite rules into a graph JSON for the Rust viewer (viewer/).

Usage: python3 -I scripts/prereq_graph.py <courses-clean.csv> <catalog-prereqs.json> <outdir>
           [--gold gold-prereqs.json] SUBJ [SUBJ ...]

With --gold, GOLD's enforced registration rules replace the catalog text for
every course GOLD lists; other courses fall back to the catalog wording.

SUBJ values are base departments (e.g. MATH PHYS); their CCS and online
variants are included. Writes <outdir>/prereq-graph.json: every charted
course plus every course/exam it references, with rules as groups (all
required) of alternatives (choose one) of items (all of).

Parsing model: a prerequisite is an AND of groups; each group is an OR of
alternatives; each alternative is one or more courses (a range like
"118A-B-C" means all of them) or a non-course item such as an AP exam.
Anything that isn't a course (consent, standing, majors only) becomes a note.
"""
import csv
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gold_rules import describe, parse_gold  # noqa: E402

SUBJECT_NAMES = [  # longest first
    ("physics cs", "PHYS CS"), ("math cs", "MATH CS"),
    ("computer science", "CMPSC"), ("mathematics", "MATH"), ("engineering", "ENGR"),
    ("physics phys", "PHYS"), ("physics", "PHYS"), ("phys", "PHYS"), ("math", "MATH"),
    ("pstat", "PSTAT"), ("ece", "ECE"), ("me", "ME"),
]
SUBJ_RE = "|".join(re.escape(n) for n, _ in SUBJECT_NAMES)
SUBJ_MAP = dict(SUBJECT_NAMES)
NUM = r"\d+[A-Z]{0,3}(?:-(?:\d+|[A-Z]{1,2}))*"
# Grade letters are case-sensitive so "at another" isn't read as "at A".
GRADE_RE = re.compile(
    r",?\s*(?i:each |all )?(?i:completed )?(?i:with |at )(?i:an? |at least an? )?"
    r"(?i:minimum |letter )?(?i:grade of )?\"?[ABCD][+-]?\"?"
    r"(?i: or (?:better|above))*(?i: in each)?|,?\s*(?i:each)(?= |$)")
ITEM_CONCURRENT_RE = re.compile(r"\(?may be taken concurrently\)?", re.I)
CLAUSE_CONCURRENT_RE = re.compile(r"concurrent enrollment in|co-requisite:", re.I)
EXAM_RE = re.compile(r"AP (?:Math|Calculus) (AB|BC)(?: exam)?(?: with a)? score of (\d)\+?"
                     r"|qualifying score on the mathematics placement exam", re.I)


def expand(subj, tok):
    """'118A-B-C' -> [118A,118B,118C]; '103-104' -> [103,104]; '5AA-ZZ' kept."""
    if re.fullmatch(r"\d+AA-ZZ", tok):
        return [f"{subj} {tok}"]
    parts = tok.split("-")
    first = re.fullmatch(r"(\d+)([A-Z]*)", parts[0])
    out = [parts[0]]
    for p in parts[1:]:
        out.append(p if p[0].isdigit() else first.group(1) + p)
    return [f"{subj} {c}" for c in out]


def parse_alternative(text, ctx):
    """One OR-alternative -> (list of course ids, exam label or None, leftover)."""
    exam = EXAM_RE.search(text)
    if exam:
        label = (f"AP Calc {exam.group(1).upper()} ≥ {exam.group(2)}" if exam.group(1)
                 else "Math placement exam")
        return [], label, ""
    courses = []
    pos = 0
    pat = re.compile(rf"(?:\b({SUBJ_RE})\s+)?\b({NUM})\b", re.I)
    for m in pat.finditer(text):
        if m.group(1):
            ctx["subj"] = SUBJ_MAP[m.group(1).lower()]
        if not ctx.get("subj"):
            continue
        courses += expand(ctx["subj"], m.group(2).upper())
        pos = m.end()
    leftover = text[pos:] if courses else text
    return courses, None, leftover.strip(" ,.;")


def parse(text):
    """Return {'groups': [{'alts': [...], 'concurrent': bool}], 'notes': [...]}."""
    text = text.replace("Ph.D.", "PhD").replace("M.A.", "MA")
    text = re.sub(r"\(or (\d+[A-Z]*)\)", r"or \1", text)
    text = re.sub(r"\(AP Math BC Exam score of (\d) or better\)", r"or AP Math BC exam score of \1", text)
    text = re.sub(r"score of (\d) or better", r"score of \1+", text)
    text = re.sub(r"^None\.?$", "", text.strip(), flags=re.I)
    result = {"groups": [], "notes": []}
    ctx = {}
    for clause in re.split(r";|\.\s+|\.$|:\s*(?=[A-Z])", text):
        clause = clause.strip(" ,")
        if not clause:
            continue
        if re.fullmatch(r"co-requisite", clause, re.I):
            ctx["coreq"] = True
            continue
        or_merge = bool(re.match(r"or\b", clause, re.I))
        clause = re.sub(r"^(?:or|and|either)\b,?\s*", "", clause, flags=re.I)
        conc_clause = bool(CLAUSE_CONCURRENT_RE.search(clause)) or ctx.pop("coreq", False)
        clause = GRADE_RE.sub("", CLAUSE_CONCURRENT_RE.sub("", clause)).strip(" ,")
        items = [i for i in re.split(r",\s*(?:and,?\s+)?|\s+and,?\s+", clause) if i.strip()]
        groups, notes = [], []
        for item in items:
            conc = conc_clause or bool(ITEM_CONCURRENT_RE.search(item))
            item = ITEM_CONCURRENT_RE.sub("", item)
            alts = []
            for alt in re.split(r"\s+or\s+|/", item):
                courses, exam, left = parse_alternative(alt, ctx)
                if courses:
                    alts.append({"courses": courses})
                elif exam:
                    alts.append({"exam": exam})
                if left and not re.fullmatch(r"(?:the )?equivalent|in each|each|either", left, re.I):
                    # A non-course alternative beside courses is an "or" option.
                    notes.append(("or " if courses or alts else "") + left)
            if alts:
                groups.append({"alts": alts, "concurrent": conc})
        if not groups:
            # Nothing course-like: keep the clause whole ("consent of
            # department and instructor") rather than its and-split pieces.
            whole = ITEM_CONCURRENT_RE.sub("", clause).strip(" ,")
            notes = [("or " if or_merge else "") + whole] if whole else []
        if or_merge and groups and len(result["groups"]) == 1:
            result["groups"][0]["alts"] += [a for g in groups for a in g["alts"]]
        elif or_merge and groups:
            notes = ["or " + clause]
        else:
            result["groups"] += groups
        result["notes"] += notes
    return result


def sort_key(cid):
    subj, num = cid.rsplit(" ", 1)
    m = re.match(r"(\d+)(.*)", num)
    return (subj, int(m.group(1)) if m else 0, m.group(2) if m else num)


def course_entry(c, cid, catalog, in_chart):
    subj = cid.rsplit(" ", 1)[0]
    family = subj.split()[0] if subj.split()[0] in ("MATH", "PHYS") else "other"
    return {
        "title": c["title"] if c else "", "description": c["description"] if c else "",
        "family": family, "in_chart": in_chart,
        "in_csv": bool(c), "in_catalog": cid in catalog,
        "is_ccs": bool(c) and c["is_ccs"] == "True",
        "units": (c["units_min"] if c and c["units_min"] == c["units_max"]
                  else f'{c["units_min"]}-{c["units_max"]}') if c else "",
        "level": c["level"] if c else "",
        "source": "", "gold_quarter": "", "catalog_text": "", "gold_rule": "",
        "notes": [], "groups": [],
    }


def main(src, catalog_json, outdir, *args):
    args = list(args)
    gold = {}
    if "--gold" in args:
        i = args.index("--gold")
        with open(args[i + 1], encoding="utf-8") as fh:
            gold = json.load(fh)["courses"]
        del args[i:i + 2]
    subjects = args
    outdir = Path(outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    with open(src, encoding="utf-8", newline="") as fh:
        courses = {r["course_id"]: r for r in csv.DictReader(fh)}
    # Online duplicates of in-person courses add nothing to a prereq chart.
    chart = {cid for cid, r in courses.items()
             if r["subject_base"] in subjects and not r["online_version_of"]}
    with open(catalog_json, encoding="utf-8") as fh:
        catalog = json.load(fh)["courses"]
    parsed = {}
    for cid in chart:
        text, source = courses[cid]["prereq_text"], "catalog"
        if not text:
            # Courses missing from the catalog sometimes state prerequisites
            # in their description ("Prerequisites include ...").
            m = re.search(r"Prerequisites?(?: include| are)?:?\s*([^.]+\.)", courses[cid]["description"])
            text, source = (m.group(1), "description") if m else ("", "")
        parsed[cid] = parse(text)
        parsed[cid].update(text=text, source=source)
        if cid in gold:
            groups = []
            for g in parse_gold(gold[cid]["rows"]):
                # GOLD sometimes lists a course as satisfying itself (repeats).
                g["alts"] = [a for a in g["alts"] if cid not in a["courses"]]
                if g["alts"]:
                    groups.append(g)
            notes = [n for n in parsed[cid]["notes"] if not n.startswith("or ")]
            if not groups and parsed[cid]["groups"]:
                notes.append(f"Catalog lists: {text} GOLD enforces none.")
            parsed[cid].update(
                groups=groups, source="gold", gold_quarter=gold[cid]["quarter"],
                # Keep catalog conditions GOLD's rule table doesn't encode
                # (consent, standing), but not catalog "or ..." alternatives.
                notes=notes)
    rule_source = ("GOLD registration rules (courses offered Summer/Fall 2026); "
                   "other courses use catalog text" if gold else "UCSB catalog text")
    graph = {"subjects": subjects, "rule_source": rule_source, "courses": {}}
    for cid in sorted(parsed, key=sort_key):
        p, c = parsed[cid], courses[cid]
        groups = []
        for g in p["groups"]:
            alts = [{"items": [f'exam:{a["exam"]}'] if "exam" in a else a["courses"],
                     "concurrent": bool(a.get("concurrent"))} for a in g["alts"]]
            groups.append({"alts": alts, "concurrent": bool(g["concurrent"])})
        graph["courses"][cid] = course_entry(c, cid, catalog, True) | {
            "source": p["source"], "gold_quarter": p.get("gold_quarter", ""),
            "catalog_text": p["text"], "gold_rule": describe(p["groups"]) if p["source"] == "gold" else "",
            "notes": p["notes"], "groups": groups}
    for cid in sorted({i for e in list(graph["courses"].values()) for g in e["groups"]
                       for a in g["alts"] for i in a["items"]}
                      - set(graph["courses"]), key=sort_key):
        if not cid.startswith("exam:") and "*" not in cid:
            graph["courses"][cid] = course_entry(courses.get(cid), cid, catalog, False)
    (outdir / "prereq-graph.json").write_text(json.dumps(graph, indent=1, ensure_ascii=False))
    print(f"{len(parsed)} charted courses, {len(graph['courses'])} total nodes -> "
          f"{outdir / 'prereq-graph.json'}")

    missing = sorted({c for p in parsed.values() for g in p["groups"] for a in g["alts"]
                      for c in a.get("courses", [])
                      if c not in courses and not c.startswith("exam:") and "*" not in c},
                     key=sort_key)
    unparsed = sorted(c for c, p in parsed.items()
                      if courses[c]["has_prereq"] == "True" and not p["groups"] and not p["notes"])
    print("referenced but not in CSV:", missing)
    print("has_prereq=True but nothing parsed:", unparsed)
    if gold:
        print(f"rules from GOLD: {sum(p['source'] == 'gold' for p in parsed.values())} "
              f"of {len(parsed)} courses")


if __name__ == "__main__":
    main(*sys.argv[1:])
