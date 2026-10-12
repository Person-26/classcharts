#!/usr/bin/env python3
"""Scrape prerequisite rules from UCSB GOLD's Find Courses, headless.

Usage: python3 scripts/gold_scrape.py <out.json> <quarter>[,<quarter>...] SUBJ [SUBJ ...]
  e.g. python3 scripts/gold_scrape.py data/gold-prereqs.json "Fall 2026,Summer 2026" MATH PHYS
  SUBJ may be ALL for every subject in GOLD's list. Progress is saved after
  each subject and courses already in out.json are skipped, so an
  interrupted run can simply be restarted.

Needs a session saved by gold_login.py (~/.cache/claude-code/gold-auth.json).
Only the read-only "Course Info" panel is opened; no cart/add buttons are
touched. GOLD only lists courses offered in the quarters it exposes, so
courses not offered then are absent. Raw rule rows are kept as-is; parsing
happens later.
"""
import json
import re
import sys
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

STATE = Path.home() / ".cache/claude-code/gold-auth.json"
FIND = "https://my.sa.ucsb.edu/gold/BasicFindCourses.aspx"
HEADING_JS = """() => {
  const el = [...document.querySelectorAll('#pageContent_CourseInfoUpdatePanel span, #pageContent_CourseInfoUpdatePanel div')]
    .find(e => /Course Information for/.test(e.textContent) && e.children.length === 0);
  return el ? el.textContent.trim() : '';
}"""
ROWS_JS = """() => [...document.querySelectorAll('#prePageView tr')]
  .map(tr => [...tr.children].map(td => td.textContent.replace(/\\s+/g, ' ').trim()))
  .filter(cells => cells.length >= 4)"""
GENERAL_JS = """() => {
  const out = {};
  const rows = document.querySelectorAll('#descPageView tr');
  rows.forEach(tr => {
    const tds = [...tr.children];
    if (tds.length >= 2) out[tds[0].textContent.trim().replace(/:$/, '')] =
      tds.slice(1).map(td => td.textContent).join(' ').replace(/\\s+/g, ' ').trim();
  });
  return out;
}"""


class SessionExpired(Exception):
    pass


def goto_find(pg):
    pg.goto(FIND)
    if "sso." in pg.url or "login" in pg.url.lower():
        raise SessionExpired


def subject_codes(pg):
    goto_find(pg)
    opts = pg.eval_on_selector_all("#pageContent_subjectAreaDropDown option", "es => es.map(e => e.text.trim())")
    return [o.rsplit("- ", 1)[-1].strip() for o in opts[1:]]


def search(pg, quarter, subj):
    goto_find(pg)
    if pg.eval_on_selector("#pageContent_quarterDropDown",
                           "e => e.selectedOptions[0].text").strip() != quarter:
        with pg.expect_navigation():
            pg.select_option("#pageContent_quarterDropDown", label=quarter)
    opts = pg.eval_on_selector_all("#pageContent_subjectAreaDropDown option",
                                   "es => es.map(e => [e.value, e.text.trim()])")
    match = [v for v, t in opts if re.search(rf"- {re.escape(subj)}$", t)]
    if not match:
        print(f"  {quarter}: no subject {subj}", file=sys.stderr)
        return False
    pg.select_option("#pageContent_subjectAreaDropDown", value=match[0])
    pg.click("#pageContent_searchButton")
    pg.wait_for_load_state("networkidle")
    return True


SNAPSHOT_JS = f"""() => {{
  const txt = sel => {{ const e = document.querySelector(sel);
                        return e ? e.textContent.replace(/\\s+/g, ' ').trim() : ''; }};
  const panel = document.querySelector('#pageContent_CourseInfoUpdatePanel');
  return {{
    html: panel ? panel.innerHTML : '',
    heading: ({HEADING_JS})(),
    general: ({GENERAL_JS})(),
    rows: ({ROWS_JS})(),
    no_prereq_msg: txt('#pageContent_CourseInfo1_preReqMsglbl'),
    recommended_prep: txt('#pageContent_CourseInfo1_RecPreplbl'),
    restrictions: txt('#restPageView'),
  }};
}}"""


def read_panel(pg, btn, listed):
    """Open a course's info panel and return its fields once they are consistent.

    The panel updates asynchronously and can be read mid-update (heading from
    one course, tabs from the previous one), so a read only counts when the
    panel HTML has stopped changing, the heading matches the course listed
    above the button, and the General tab's title matches the heading.
    """
    for attempt in range(4):
        with pg.expect_response(lambda r: "ResultsFindCourses" in r.url
                                and r.request.method == "POST", timeout=30000):
            btn.evaluate("e => e.click()")
        pg.wait_for_load_state("networkidle")
        snap, stable, deadline = None, 0, time.time() + 10
        while time.time() < deadline and stable < 2:
            time.sleep(0.3)
            cur = pg.evaluate(SNAPSHOT_JS)
            stable = stable + 1 if snap and cur["html"] == snap["html"] else 0
            snap = cur
        heading = snap["heading"] if snap else ""
        parts = heading.split(" - ")
        cid = re.sub(r"\s+", " ", parts[0]).strip()
        title = " - ".join(parts[1:-1]).strip()
        if (stable >= 2 and heading and (not listed or cid == listed)
                and re.sub(r"\s+", " ", snap["general"].get("Title", "")).strip() == re.sub(r"\s+", " ", title)):
            return {
                "id": cid,
                "heading": heading,
                "rows": snap["rows"],
                "no_prereq_msg": snap["no_prereq_msg"],
                "recommended_prep": snap["recommended_prep"],
                "prereq_comments": snap["general"].get("PreRequisite Comments", ""),
                "restrictions": snap["restrictions"],
            }
        time.sleep(1.5 * (attempt + 1))
    return None


def scrape(pg, quarter, subj, out):
    if not search(pg, quarter, subj):
        return
    buttons = pg.query_selector_all("input.course-info[id$='_CourseDetailLink_0']")
    new = 0
    for btn in buttons:
        # Course id from the nearest course header, to skip ones we already have.
        listed = btn.evaluate("""e => { let last = '';
            for (const t of document.querySelectorAll('span.courseTitle')) {
                if (t.compareDocumentPosition(e) & Node.DOCUMENT_POSITION_FOLLOWING) last = t.textContent;
                else break;
            } return last; }""")
        listed = re.sub(r"\s+", " ", listed.split("\u00a0-")[0].split(" - ")[0]).strip()
        if listed and listed in out:
            continue
        rec = read_panel(pg, btn, listed)
        if rec is None:
            print(f"  could not confirm panel for {listed or btn.get_attribute('id')}",
                  file=sys.stderr, flush=True)
            continue
        cid = rec.pop("id")
        if cid in out:  # already have it from an earlier quarter
            continue
        out[cid] = {"quarter": quarter, **rec}
        new += 1
        time.sleep(0.5)
    print(f"  {quarter} {subj}: {len(buttons)} listed, {new} new", file=sys.stderr, flush=True)


def main(dst, quarters, *subjects):
    dst = Path(dst)
    out = json.loads(dst.read_text())["courses"] if dst.exists() else {}
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        pg = browser.new_context(storage_state=str(STATE)).new_page()
        try:
            if "ALL" in subjects:
                subjects = subject_codes(pg)
            for q in quarters.split(","):
                for s in subjects:
                    scrape(pg, q.strip(), s, out)
                    dst.write_text(json.dumps({"courses": out}, indent=1, ensure_ascii=False))
        except SessionExpired:
            sys.exit(f"GOLD session expired ({len(out)} courses saved) - rerun gold_login.py, "
                     "then this command to resume")
        finally:
            browser.close()
    print(f"{len(out)} courses saved to {dst}")


if __name__ == "__main__":
    main(*sys.argv[1:])
