"""Public, read-only site: stats, published reports, about. No write path."""

from __future__ import annotations

import logging

from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.responses import HTMLResponse
from starlette.concurrency import run_in_threadpool

from washburn_web import db
from washburn_web.markdown import render_markdown
from washburn_web.web import install_error_handlers, make_lifespan, render

logger = logging.getLogger("washburn_web.public")
PAGE_SIZE = 25
PUBLIC_SITE_KEY = "public_site_enabled"
OFFLINE_HTML = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Temporarily offline</title>
<style>body {
  font: 16px/1.5 system-ui, sans-serif; margin: 0 16px; padding: 4rem 0; text-align: center;
}</style>
</head><body><h1>This site is temporarily offline</h1><p>Please check back later.</p></body></html>
"""

app = FastAPI(
    title="Washburn",
    docs_url=None,
    redoc_url=None,
    openapi_url=None,
    lifespan=make_lifespan(),
)
install_error_handlers(app, "public")


def _public_site_enabled() -> bool:
    """Fail closed: anything other than the stored JSON true (or any error) means offline."""
    try:
        return db.get_setting(PUBLIC_SITE_KEY, False) is True
    except Exception as exc:  # noqa: BLE001  (any DB or driver failure)
        # Log the exception type only: driver messages can echo connection details.
        logger.error("public site state unreadable, serving offline: %s", type(exc).__name__)
        return False


@app.middleware("http")
async def public_site_gate(request: Request, call_next):
    if request.url.path == "/healthz":
        return await call_next(request)
    if await run_in_threadpool(_public_site_enabled):
        return await call_next(request)
    return HTMLResponse(
        OFFLINE_HTML,
        status_code=503,
        headers={"Retry-After": "300", "Cache-Control": "no-store"},
    )


@app.get("/healthz")
def healthz() -> dict[str, bool]:
    return {"ok": True}


@app.get("/", response_class=HTMLResponse)
def stats():
    data = db.public_stats()
    return render("public/stats.html", area="public", stats=data)


@app.get("/reports", response_class=HTMLResponse)
def reports(page: int = Query(1, ge=1, le=100_000)):
    rows = db.list_published_reports(limit=PAGE_SIZE + 1, offset=(page - 1) * PAGE_SIZE)
    has_next = len(rows) > PAGE_SIZE
    return render(
        "public/reports.html",
        area="public",
        reports=rows[:PAGE_SIZE],
        page=page,
        has_prev=page > 1,
        has_next=has_next,
    )


@app.get("/reports/{report_id}", response_class=HTMLResponse)
def report(report_id: int):
    row = db.get_published_report(report_id)
    if row is None:
        raise HTTPException(status_code=404, detail="No such published report.")
    return render(
        "public/report.html",
        area="public",
        report=row,
        body_html=render_markdown(row["markdown"] or ""),
    )


@app.get("/about", response_class=HTMLResponse)
def about():
    return render("public/about.html", area="public")
