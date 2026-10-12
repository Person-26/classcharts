#!/usr/bin/env python3
"""Open a visible browser on UCSB GOLD and wait for a manual login.

Usage: python3 scripts/gold_login.py [state.json]

The user types their own credentials and completes Duo in the window; this
script never sees them. Once GOLD's logged-in pages load, the session
cookies are saved to state.json (mode 0600) so later scraping can run
headless.
"""
import os
import sys
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

STATE = Path(sys.argv[1] if len(sys.argv) > 1 else
             Path.home() / ".cache/claude-code/gold-auth.json")
LOGIN_URL = "https://my.sa.ucsb.edu/gold/Login.aspx"
TIMEOUT_S = 15 * 60


def logged_in(url):
    return "my.sa.ucsb.edu/gold/" in url and "login" not in url.lower()


def main():
    STATE.parent.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=False)
        ctx = browser.new_context()
        page = ctx.new_page()
        page.goto(LOGIN_URL)
        print("Browser open - log in to GOLD in that window.", flush=True)
        deadline = time.time() + TIMEOUT_S
        while not logged_in(page.url):
            if time.time() > deadline:
                sys.exit("Timed out waiting for login.")
            page.wait_for_timeout(1000)
        page.wait_for_load_state("networkidle")
        ctx.storage_state(path=str(STATE))
        os.chmod(STATE, 0o600)
        print(f"Logged in; session saved. Landed on: {page.url}", flush=True)
        browser.close()


if __name__ == "__main__":
    main()
