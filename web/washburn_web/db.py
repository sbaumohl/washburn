"""Thin query layer over the Washburn schema (db/migrations/0001_init.sql).

Every value reaches Postgres as a bound parameter. The only SQL built from
Python strings is fixed text; user input never gets formatted into it.
Writes go through ``pool.connection()`` which commits on success.
"""

from __future__ import annotations

import hashlib
import logging
import time
from collections.abc import Sequence
from typing import Any

from psycopg.rows import dict_row
from psycopg.types.json import Jsonb
from psycopg_pool import ConnectionPool

from washburn_web.config import PostgresConfig
from washburn_web.urls import normalise_host, normalise_seed_url

log = logging.getLogger("washburn_web.db")

_pool: ConnectionPool | None = None
MAX_BODY_BYTES = 50 * 1024 * 1024  # decompression ceiling for /pages/{id}/body
EVIDENCE_TEXT_CHARS = 20_000
SEARCH_TEXT_CHARS = 200_000  # must match the expression behind pages.text_tsv
SETTINGS_CACHE_TTL = 5.0  # seconds; module global so tests can override it
_settings_cache: dict[str, tuple[float, Any]] = {}


# ---------------------------------------------------------------- pool lifecycle
def open_pool(pg: PostgresConfig) -> ConnectionPool:
    """Create and open the pool. ``wait=False`` so an unreachable DB never blocks startup."""
    global _pool
    if _pool is None:
        _pool = ConnectionPool(
            conninfo=pg.conninfo(),
            min_size=1,
            max_size=4,
            timeout=10,
            open=False,
            name="washburn-web",
        )
        _pool.open(wait=False)
    return _pool


def close_pool() -> None:
    global _pool
    if _pool is not None:
        _pool.close()
        _pool = None


def get_pool() -> ConnectionPool:
    if _pool is None:
        raise RuntimeError("database pool is not open; the app lifespan has not run")
    return _pool


def _fetch_all(sql: str, params: dict[str, Any] | Sequence[Any] | None = None) -> list[dict]:
    with get_pool().connection() as conn, conn.cursor(row_factory=dict_row) as cur:
        cur.execute(sql, params)
        return list(cur.fetchall())


def _fetch_one(sql: str, params: dict[str, Any] | Sequence[Any] | None = None) -> dict | None:
    with get_pool().connection() as conn, conn.cursor(row_factory=dict_row) as cur:
        cur.execute(sql, params)
        return cur.fetchone()


# ---------------------------------------------------------------- public site
def public_stats() -> dict[str, Any]:
    """Aggregates only. Findings are counted only when published, so unpublished
    findings are not visible even as counts on the public site."""
    totals = _fetch_one(
        """
        SELECT
          (SELECT count(*) FROM pages)     AS pages,
          (SELECT count(*) FROM hosts)     AS hosts,
          (SELECT count(*) FROM findings WHERE published) AS published_findings,
          (SELECT max(published_at) FROM findings WHERE published) AS last_published_at
        """
    )
    by_kind = _fetch_all(
        """
        SELECT kind::text AS kind, count(*) AS n
        FROM findings WHERE published
        GROUP BY kind ORDER BY kind
        """
    )
    return {"totals": totals or {}, "by_kind": by_kind}


def list_published_reports(limit: int, offset: int) -> list[dict]:
    return _fetch_all(
        """
        SELECT id, title, published_at, final_url, fetched_at
        FROM published_reports
        ORDER BY published_at DESC, id DESC
        LIMIT %(limit)s OFFSET %(offset)s
        """,
        {"limit": limit, "offset": offset},
    )


def get_published_report(report_id: int) -> dict | None:
    return _fetch_one(
        """
        SELECT id, title, markdown, published_at, final_url, fetched_at
        FROM published_reports
        WHERE id = %(id)s
        """,
        {"id": report_id},
    )


# ---------------------------------------------------------------- admin: dashboard
def live_workers() -> list[dict]:
    return _fetch_all(
        """
        SELECT worker, hostname, pid, version, started_at, last_seen, active_jobs, leased_host
        FROM live_workers ORDER BY worker
        """
    )


def queue_depth() -> list[dict]:
    return _fetch_all("SELECT host, pending FROM queue_depth ORDER BY pending DESC, host LIMIT 200")


def pages_per_hour() -> list[dict]:
    return _fetch_all(
        """
        SELECT date_trunc('hour', fetched_at) AS hour, count(*) AS pages
        FROM pages
        WHERE fetched_at > now() - interval '24 hours'
        GROUP BY 1 ORDER BY 1
        """
    )


def flag_rates_24h() -> dict[str, Any]:
    """Jev flag rate and judge confirm/downgrade rates over the last 24 hours."""
    return (
        _fetch_one(
            """
        SELECT
          (SELECT count(*) FROM screenings
             WHERE created_at > now() - interval '24 hours')                       AS screenings,
          (SELECT count(*) FROM screenings
             WHERE created_at > now() - interval '24 hours' AND level_one)         AS level_one,
          (SELECT count(*) FROM judgments
             WHERE created_at > now() - interval '24 hours')                       AS judgments,
          (SELECT count(*) FROM judgments
             WHERE created_at > now() - interval '24 hours' AND confirmed_serious) AS confirmed,
          (SELECT count(*) FROM judgments
             WHERE created_at > now() - interval '24 hours'
               AND classification IN ('no_signal', 'authorized'))                  AS downgraded
        """
        )
        or {}
    )


def recent_events(limit: int = 50) -> list[dict]:
    return _fetch_all(
        "SELECT id, at, worker, kind, detail FROM events ORDER BY at DESC, id DESC LIMIT %(limit)s",
        {"limit": limit},
    )


# ---------------------------------------------------------------- admin: findings
def list_findings(kind: str | None, published: bool | None, limit: int, offset: int) -> list[dict]:
    """``kind`` must be 'level_one' or 'report' (or None); validated by the caller."""
    return _fetch_all(
        """
        SELECT f.id, f.kind::text AS kind, f.title, f.published, f.published_at,
               f.reviewed_by, f.created_at, p.url, p.final_url
        FROM findings f JOIN pages p ON p.id = f.page_id
        WHERE (%(kind)s::finding_kind IS NULL OR f.kind = %(kind)s::finding_kind)
          AND (%(published)s::boolean IS NULL OR f.published = %(published)s::boolean)
        ORDER BY f.created_at DESC, f.id DESC
        LIMIT %(limit)s OFFSET %(offset)s
        """,
        {"kind": kind, "published": published, "limit": limit, "offset": offset},
    )


def get_finding(finding_id: int) -> dict | None:
    return _fetch_one(
        """
        SELECT f.id, f.page_id, f.kind::text AS kind, f.title, f.markdown,
               f.created_at, f.updated_at, f.reviewed_by, f.reviewed_at, f.review_note,
               f.published, f.published_at,
               p.url, p.final_url, p.status, p.fetched_at
        FROM findings f JOIN pages p ON p.id = f.page_id
        WHERE f.id = %(id)s
        """,
        {"id": finding_id},
    )


def get_page_screenings(page_id: int) -> list[dict]:
    return _fetch_all(
        """
        SELECT id, chunk, model, importance, agent_activity, potentially_serious,
               level_one, threshold, created_at
        FROM screenings WHERE page_id = %(page_id)s ORDER BY chunk, id
        """,
        {"page_id": page_id},
    )


def get_page_judgments(page_id: int) -> list[dict]:
    return _fetch_all(
        """
        SELECT id, screening_id, chunk, model, activity, seriousness, confidence,
               classification::text AS classification, confirmed_serious, summary,
               evidence, uncertainties, created_at
        FROM judgments WHERE page_id = %(page_id)s ORDER BY chunk, id
        """,
        {"page_id": page_id},
    )


def set_finding_publication(
    finding_id: int, published: bool, reviewed_by: str, review_note: str | None
) -> bool:
    """Set the review fields and publish state. Returns False when the id does not exist."""
    with get_pool().connection() as conn, conn.cursor() as cur:
        cur.execute(
            """
            UPDATE findings SET
              reviewed_by  = %(reviewed_by)s,
              reviewed_at  = now(),
              review_note  = %(review_note)s,
              published    = %(published)s,
              published_at = CASE
                               WHEN %(published)s THEN COALESCE(published_at, now())
                               ELSE NULL
                             END,
              updated_at   = now()
            WHERE id = %(id)s
            RETURNING id
            """,
            {
                "id": finding_id,
                "published": published,
                "reviewed_by": reviewed_by,
                "review_note": review_note or None,
            },
        )
        return cur.fetchone() is not None


# ---------------------------------------------------------------- admin: pages
def search_pages(query: str, limit: int) -> list[dict]:
    """Ranked full-text search. Snippet hit markers are \x02 and \x03 so the caller can
    HTML-escape the text first and then substitute real <mark> tags."""
    return _fetch_all(
        """
        SELECT p.id, p.url, p.final_url, p.status, p.fetched_at,
               ts_rank(p.text_tsv, q) AS rank,
               ts_headline('simple', left(p.text, %(chars)s), q, %(headline_opts)s) AS snippet
        FROM pages p, plainto_tsquery('simple', %(query)s) AS q
        WHERE p.text_tsv @@ q
        ORDER BY rank DESC, p.id DESC
        LIMIT %(limit)s
        """,
        {
            "query": query,
            "limit": limit,
            "chars": SEARCH_TEXT_CHARS,
            "headline_opts": "StartSel=\x02, StopSel=\x03, MaxFragments=2, MinWords=5, MaxWords=25",
        },
    )


def get_page(page_id: int) -> dict | None:
    return _fetch_one(
        """
        SELECT id, url, final_url, status, content_type, fetched_at, worker,
               redirects, jsonb_array_length(links) AS links_count,
               left(text, %(chars)s) AS text_head,
               length(text) AS text_len,
               body_sha256
        FROM pages WHERE id = %(id)s
        """,
        {"id": page_id, "chars": EVIDENCE_TEXT_CHARS},
    )


def get_page_body(page_id: int) -> bytes | None:
    """Compressed (zstd) body for the page, or None when the page does not exist."""
    row = _fetch_one(
        """
        SELECT b.bytes
        FROM pages p JOIN blobs b ON b.sha256 = p.body_sha256
        WHERE p.id = %(id)s
        """,
        {"id": page_id},
    )
    return bytes(row["bytes"]) if row else None


# ---------------------------------------------------------------- admin: seeds
def list_seeds() -> list[dict]:
    return _fetch_all(
        """
        SELECT s.id, s.url, s.host, s.note, s.added_by, s.added_at, s.enabled,
               (SELECT count(*) FROM queue q WHERE q.seed_id = s.id) AS queued
        FROM seeds s ORDER BY s.added_at DESC, s.id DESC
        """
    )


def _queue_root(cur, url: str, host: str, seed_id: int) -> bool:
    """Ensure hosts has the host, queue the root URL at depth 0, and bump pending_count.

    Returns False when the URL was already in the queue (visited set).
    """
    url_hash = hashlib.sha256(url.encode("utf-8")).digest()
    cur.execute(
        "INSERT INTO hosts (host) VALUES (%(host)s) ON CONFLICT (host) DO NOTHING",
        {"host": host},
    )
    cur.execute(
        """
        INSERT INTO queue (url, url_hash, host, seed_id, parent_url, depth, status)
        VALUES (%(url)s, %(url_hash)s, %(host)s, %(seed_id)s, NULL, 0, 'pending')
        ON CONFLICT DO NOTHING
        RETURNING id
        """,
        {"url": url, "url_hash": url_hash, "host": host, "seed_id": seed_id},
    )
    queued = cur.fetchone() is not None
    if queued:
        cur.execute(
            "UPDATE hosts SET pending_count = pending_count + 1 WHERE host = %(host)s",
            {"host": host},
        )
    return queued


def add_seed(url: str, added_by: str, note: str | None = None) -> dict[str, Any]:
    """Insert seed, host and root queue row in one transaction.

    Returns ``{"seed_id", "host", "queued"}``; ``queued`` is False when the URL
    was already in the queue (visited set), in which case nothing else changes.
    """
    canonical = normalise_seed_url(url)
    host = normalise_host(canonical)
    with (
        get_pool().connection() as conn,
        conn.transaction(),
        conn.cursor(row_factory=dict_row) as cur,
    ):
        cur.execute(
            """
            INSERT INTO seeds (url, host, note, added_by)
            VALUES (%(url)s, %(host)s, %(note)s, %(added_by)s)
            ON CONFLICT (url) DO NOTHING
            RETURNING id
            """,
            {"url": canonical, "host": host, "note": note, "added_by": added_by},
        )
        row = cur.fetchone()
        if row is None:
            cur.execute("SELECT id FROM seeds WHERE url = %(url)s", {"url": canonical})
            row = cur.fetchone()
        seed_id = row["id"]
        queued = _queue_root(cur, canonical, host, seed_id)
    log.info("seed added id=%s host=%s queued=%s", seed_id, host, queued)
    return {"seed_id": seed_id, "host": host, "queued": queued}


def add_seeds(urls: Sequence[str], note: str | None, added_by: str) -> dict[str, int]:
    """Insert many seeds in ONE transaction (all or nothing).

    URLs are canonicalised and deduplicated first. A URL already in ``seeds`` counts
    as already_present and changes nothing. A new seed gets the same hosts/queue/
    pending_count handling as ``add_seed``. Returns ``{"added", "already_present"}``.
    """
    canonical = list(dict.fromkeys(normalise_seed_url(u) for u in urls))
    added = already_present = 0
    with (
        get_pool().connection() as conn,
        conn.transaction(),
        conn.cursor(row_factory=dict_row) as cur,
    ):
        for url in canonical:
            host = normalise_host(url)
            cur.execute(
                """
                INSERT INTO seeds (url, host, note, added_by)
                VALUES (%(url)s, %(host)s, %(note)s, %(added_by)s)
                ON CONFLICT (url) DO NOTHING
                RETURNING id
                """,
                {"url": url, "host": host, "note": note, "added_by": added_by},
            )
            row = cur.fetchone()
            if row is None:
                already_present += 1
                continue
            _queue_root(cur, url, host, row["id"])
            added += 1
    log.info("bulk seeds added=%s already_present=%s", added, already_present)
    return {"added": added, "already_present": already_present}


def list_enabled_seed_urls() -> list[str]:
    rows = _fetch_all("SELECT url FROM seeds WHERE enabled ORDER BY id")
    return [row["url"] for row in rows]


def disable_seed(seed_id: int) -> bool:
    with get_pool().connection() as conn, conn.cursor() as cur:
        cur.execute(
            "UPDATE seeds SET enabled = false WHERE id = %(id)s RETURNING id",
            {"id": seed_id},
        )
        return cur.fetchone() is not None


# ---------------------------------------------------------------- settings
def get_setting(key: str, default: Any = None) -> Any:
    """Stored JSON value for ``key``, or ``default`` when no row exists.

    Stored values are cached per process for SETTINGS_CACHE_TTL seconds. Defaults
    are never cached. Database errors propagate; callers decide how to fail.
    """
    now = time.monotonic()
    hit = _settings_cache.get(key)
    if hit is not None and hit[0] > now:
        return hit[1]
    row = _fetch_one("SELECT value FROM settings WHERE key = %(key)s", {"key": key})
    if row is None:
        return default
    _settings_cache[key] = (now + SETTINGS_CACHE_TTL, row["value"])
    return row["value"]


def get_setting_row(key: str) -> dict | None:
    """Uncached read of the value plus who changed it and when (for the admin UI)."""
    return _fetch_one(
        "SELECT key, value, updated_at, updated_by FROM settings WHERE key = %(key)s",
        {"key": key},
    )


def set_setting(key: str, value: Any, updated_by: str) -> None:
    """Upsert a JSON value. Clears this process's cache entry for ``key``."""
    with get_pool().connection() as conn, conn.cursor() as cur:
        cur.execute(
            """
            INSERT INTO settings (key, value, updated_at, updated_by)
            VALUES (%(key)s, %(value)s, now(), %(updated_by)s)
            ON CONFLICT (key) DO UPDATE SET
              value      = EXCLUDED.value,
              updated_at = EXCLUDED.updated_at,
              updated_by = EXCLUDED.updated_by
            """,
            {"key": key, "value": Jsonb(value), "updated_by": updated_by},
        )
    _settings_cache.pop(key, None)


def record_event(worker: str, kind: str, detail: dict[str, Any]) -> None:
    with get_pool().connection() as conn, conn.cursor() as cur:
        cur.execute(
            "INSERT INTO events (worker, kind, detail) VALUES (%(worker)s, %(kind)s, %(detail)s)",
            {"worker": worker, "kind": kind, "detail": Jsonb(detail)},
        )


# ---------------------------------------------------------------- admin: threshold
def threshold_histogram() -> list[dict]:
    """Screenings bucketed into 0.05-wide importance bins (bin 0 = [0.00, 0.05), ...).

    ``judged`` counts screenings with a judgment. Judgments only exist for
    level_one screenings, so bins below the threshold in force show 0 judged.
    """
    return _fetch_all(
        """
        WITH binned AS (
          SELECT least(greatest(floor(s.importance * 20)::int, 0), 19) AS bin,
                 s.id AS screening_id,
                 j.id AS judgment_id,
                 j.confirmed_serious
          FROM screenings s
          LEFT JOIN judgments j ON j.screening_id = s.id
        )
        SELECT bin,
               count(*)                                        AS screenings,
               count(judgment_id)                              AS judged,
               count(*) FILTER (WHERE confirmed_serious)       AS confirmed_serious
        FROM binned
        GROUP BY bin ORDER BY bin
        """
    )
