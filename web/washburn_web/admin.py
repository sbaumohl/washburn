"""Private admin app. Every route except /healthz requires HTTP Basic auth.

Served only on the Tailscale interface (see deploy/compose/web.yml).
"""

from __future__ import annotations

import html
import secrets
from collections.abc import Iterator
from typing import Annotated, Any
from urllib.parse import urlsplit

import zstandard
from fastapi import APIRouter, Depends, FastAPI, Form, HTTPException, Query, Request
from fastapi.responses import HTMLResponse, PlainTextResponse, RedirectResponse, StreamingResponse
from fastapi.security import HTTPBasic, HTTPBasicCredentials
from markupsafe import Markup

from washburn_web import db
from washburn_web.config import get_config
from washburn_web.markdown import render_markdown
from washburn_web.urls import InvalidSeedUrl, normalise_seed_url, parse_bulk_targets
from washburn_web.web import install_error_handlers, make_lifespan, render

PAGE_SIZE = 50
SEARCH_LIMIT = 50
FINDING_KINDS = ("level_one", "report")
BULK_MAX_LINES = 5000
BULK_SHOWN_REJECTIONS = 200
PUBLIC_SITE_KEY = "public_site_enabled"

_basic = HTTPBasic(realm="washburn-admin")


def require_admin(
    credentials: Annotated[HTTPBasicCredentials, Depends(_basic)],
) -> str:
    """Return the authenticated username. Both comparisons always run."""
    web = get_config().web
    user_ok = secrets.compare_digest(
        credentials.username.encode("utf-8"), web.admin_user.encode("utf-8")
    )
    pass_ok = secrets.compare_digest(
        credentials.password.encode("utf-8"), web.admin_password.encode("utf-8")
    )
    if not (user_ok and pass_ok):
        raise HTTPException(
            status_code=401,
            detail="Invalid credentials.",
            headers={"WWW-Authenticate": 'Basic realm="washburn-admin"'},
        )
    return credentials.username


def reject_cross_site(request: Request) -> None:
    """Basic auth is cached by browsers and sent cross-site, so refuse POSTs that a
    browser marks as cross-site or whose Origin does not match this host."""
    if request.headers.get("sec-fetch-site") == "cross-site":
        raise HTTPException(status_code=403, detail="Cross-site form submission refused.")
    origin = request.headers.get("origin")
    if origin is not None and urlsplit(origin).netloc != request.headers.get("host"):
        raise HTTPException(status_code=403, detail="Origin does not match this host.")


router = APIRouter(dependencies=[Depends(require_admin)])
app = FastAPI(
    title="Washburn admin",
    docs_url=None,
    redoc_url=None,
    openapi_url=None,
    lifespan=make_lifespan(),
)
install_error_handlers(app, "admin")


@app.get("/healthz")
def healthz() -> dict[str, bool]:
    return {"ok": True}


def _ratio(numerator: int, denominator: int) -> float | None:
    return None if denominator == 0 else numerator / denominator


# ---------------------------------------------------------------- dashboard
@router.get("/", response_class=HTMLResponse)
def dashboard(user: str = Depends(require_admin)):
    rates = db.flag_rates_24h()
    screenings = int(rates.get("screenings") or 0)
    judgments = int(rates.get("judgments") or 0)
    return render(
        "admin/dashboard.html",
        area="admin",
        user=user,
        workers=db.live_workers(),
        queue=db.queue_depth(),
        pages_per_hour=db.pages_per_hour(),
        rates=rates,
        flag_rate=_ratio(int(rates.get("level_one") or 0), screenings),
        confirm_rate=_ratio(int(rates.get("confirmed") or 0), judgments),
        downgrade_rate=_ratio(int(rates.get("downgraded") or 0), judgments),
        events=db.recent_events(50),
        public_site=db.get_setting_row(PUBLIC_SITE_KEY),
    )


# ---------------------------------------------------------------- findings
@router.get("/findings", response_class=HTMLResponse)
def findings(
    kind: str = Query("", max_length=32),
    published: str = Query("", max_length=8),
    page: int = Query(1, ge=1, le=100_000),
):
    if kind not in ("",) + FINDING_KINDS:
        raise HTTPException(status_code=400, detail="kind must be level_one or report.")
    if published not in ("", "true", "false"):
        raise HTTPException(status_code=400, detail="published must be true or false.")
    published_flag = None if published == "" else published == "true"
    rows = db.list_findings(
        kind=kind or None,
        published=published_flag,
        limit=PAGE_SIZE + 1,
        offset=(page - 1) * PAGE_SIZE,
    )
    return render(
        "admin/findings.html",
        area="admin",
        rows=rows[:PAGE_SIZE],
        kind=kind,
        published=published,
        kinds=FINDING_KINDS,
        page=page,
        has_prev=page > 1,
        has_next=len(rows) > PAGE_SIZE,
    )


@router.get("/findings/{finding_id}", response_class=HTMLResponse)
def finding(finding_id: int):
    row = db.get_finding(finding_id)
    if row is None:
        raise HTTPException(status_code=404, detail="No such finding.")
    return render(
        "admin/finding.html",
        area="admin",
        finding=row,
        body_html=render_markdown(row["markdown"] or ""),
        screenings=db.get_page_screenings(row["page_id"]),
        judgments=db.get_page_judgments(row["page_id"]),
        public_site_on=db.get_setting(PUBLIC_SITE_KEY, False) is True,
    )


def _set_publication(finding_id: int, published: bool, user: str, review_note: str) -> None:
    note = review_note.strip()[:4000] or None
    if not db.set_finding_publication(finding_id, published, user, note):
        raise HTTPException(status_code=404, detail="No such finding.")


@router.post(
    "/findings/{finding_id}/publish",
    dependencies=[Depends(reject_cross_site)],
)
def publish_finding(
    finding_id: int,
    user: str = Depends(require_admin),
    review_note: str = Form("", max_length=4000),
):
    _set_publication(finding_id, True, user, review_note)
    return RedirectResponse(f"/findings/{finding_id}", status_code=303)


@router.post(
    "/findings/{finding_id}/unpublish",
    dependencies=[Depends(reject_cross_site)],
)
def unpublish_finding(
    finding_id: int,
    user: str = Depends(require_admin),
    review_note: str = Form("", max_length=4000),
):
    _set_publication(finding_id, False, user, review_note)
    return RedirectResponse(f"/findings/{finding_id}", status_code=303)


# ---------------------------------------------------------------- pages
def _mark_snippet(row: dict) -> dict:
    """Escape the page text, then turn the headline markers into <mark> tags."""
    escaped = html.escape(row.get("snippet") or "", quote=False)
    marked = escaped.replace("\x02", "<mark>").replace("\x03", "</mark>")
    return {**row, "snippet_html": Markup(marked)}


@router.get("/pages", response_class=HTMLResponse)
def search(q: str = Query("", max_length=500)):
    query = q.strip()
    results = [_mark_snippet(r) for r in db.search_pages(query, SEARCH_LIMIT)] if query else []
    return render("admin/search.html", area="admin", q=query, results=results)


@router.get("/pages/{page_id}", response_class=HTMLResponse)
def page_evidence(page_id: int):
    row = db.get_page(page_id)
    if row is None:
        raise HTTPException(status_code=404, detail="No such page.")
    return render("admin/page.html", area="admin", page=row)


@router.get("/pages/{page_id}/body")
def page_body(page_id: int):
    compressed = db.get_page_body(page_id)
    if compressed is None:
        raise HTTPException(status_code=404, detail="No such page.")
    try:
        raw = zstandard.ZstdDecompressor().decompress(compressed, max_output_size=db.MAX_BODY_BYTES)
    except zstandard.ZstdError as exc:
        raise HTTPException(
            status_code=422, detail="Body could not be decompressed within the size limit."
        ) from exc

    def chunks() -> Iterator[bytes]:
        view = memoryview(raw)
        step = 256 * 1024
        for start in range(0, len(view), step):
            yield view[start : start + step]

    # Always a download, never rendered inline: raw pages are untrusted content.
    return StreamingResponse(
        chunks(),
        media_type="application/octet-stream",
        headers={
            "Content-Disposition": f'attachment; filename="page-{page_id}.bin"',
            "Content-Length": str(len(raw)),
            "X-Content-Type-Options": "nosniff",
            "Cache-Control": "no-store",
        },
    )


# ---------------------------------------------------------------- seeds
@router.get("/seeds", response_class=HTMLResponse)
def seeds():
    return render("admin/seeds.html", area="admin", seeds=db.list_seeds(), error="")


@router.post("/seeds", dependencies=[Depends(reject_cross_site)])
def add_seed(
    url: str = Form(..., max_length=2048),
    user: str = Depends(require_admin),
):
    try:
        normalise_seed_url(url)
    except InvalidSeedUrl as exc:
        return render(
            "admin/seeds.html",
            status=400,
            area="admin",
            seeds=db.list_seeds(),
            error=str(exc),
        )
    db.add_seed(url=url, added_by=user)
    return RedirectResponse("/seeds", status_code=303)


def _seeds_page(status: int = 200, error: str = "", bulk: dict | None = None):
    return render(
        "admin/seeds.html",
        status=status,
        area="admin",
        seeds=db.list_seeds(),
        error=error,
        bulk=bulk,
    )


@router.post("/seeds/bulk", dependencies=[Depends(reject_cross_site)])
def bulk_add_seeds(
    urls: str = Form(""),
    note: str = Form("", max_length=500),
    user: str = Depends(require_admin),
):
    line_count = len(urls.splitlines())
    if line_count > BULK_MAX_LINES:
        return _seeds_page(
            status=400,
            error=f"Too many lines ({line_count}); the limit is {BULK_MAX_LINES} per request.",
        )
    parsed = parse_bulk_targets(urls)
    rejected = [
        {"line": r.raw if len(r.raw) <= 200 else r.raw[:200] + "...", "reason": r.reason}
        for r in parsed.rejections[:BULK_SHOWN_REJECTIONS]
    ]
    summary: dict[str, Any] = {
        "added": 0,
        "already_present": 0,
        "duplicates": parsed.duplicates,
        "rejected_count": len(parsed.rejections),
        "rejected": rejected,
    }
    if not parsed.urls:
        return _seeds_page(
            status=400, error="No valid URLs found in the pasted text.", bulk=summary
        )
    counts = db.add_seeds(urls=parsed.urls, note=note.strip() or None, added_by=user)
    summary.update(added=counts["added"], already_present=counts["already_present"])
    return _seeds_page(bulk=summary)


@router.get("/seeds/export", response_class=PlainTextResponse)
def export_seeds():
    body = "".join(f"{url}\n" for url in db.list_enabled_seed_urls())
    return PlainTextResponse(body, headers={"Cache-Control": "no-store"})


@router.post("/seeds/{seed_id}/disable", dependencies=[Depends(reject_cross_site)])
def disable_seed(seed_id: int):
    if not db.disable_seed(seed_id):
        raise HTTPException(status_code=404, detail="No such seed.")
    return RedirectResponse("/seeds", status_code=303)


# ---------------------------------------------------------------- public site
@router.post("/settings/public-site", dependencies=[Depends(reject_cross_site)])
def set_public_site(
    enabled: str = Form(""),
    user: str = Depends(require_admin),
):
    if enabled not in ("true", "false"):
        raise HTTPException(status_code=400, detail="enabled must be true or false.")
    value = enabled == "true"
    db.set_setting(PUBLIC_SITE_KEY, value, user)
    db.record_event("admin", "public_site_toggled", {"enabled": value, "by": user})
    return RedirectResponse("/", status_code=303)


# ---------------------------------------------------------------- threshold
@router.get("/threshold", response_class=HTMLResponse)
def threshold():
    bins: list[dict[str, Any]] = []
    rows = {int(r["bin"]): r for r in db.threshold_histogram()}
    for index in range(20):
        row = rows.get(index, {})
        low = index * 0.05
        bins.append(
            {
                "low": round(low, 2),
                "label": f"{low:.2f}-{low + 0.05:.2f}",
                "screenings": int(row.get("screenings") or 0),
                "judged": int(row.get("judged") or 0),
                "confirmed": int(row.get("confirmed_serious") or 0),
            }
        )
    max_count = max((b["screenings"] for b in bins), default=0)
    return render(
        "admin/threshold.html",
        area="admin",
        bins=bins,
        max_count=max_count,
        routing_threshold=get_config().importance_threshold,
    )


app.include_router(router)
