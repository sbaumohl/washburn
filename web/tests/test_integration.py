"""Integration tests against a real Postgres.

Gated on WASHBURN_TEST_DATABASE_URL (an admin-capable URL on a disposable
server, e.g. ``postgresql://postgres:pw@localhost:55433/postgres``). Each test
run creates a fresh database, applies db/migrations/0001_init.sql, and drops it.
"""

from __future__ import annotations

import hashlib
import os
import uuid
from pathlib import Path

import pytest
import zstandard
from psycopg import sql
from psycopg.conninfo import conninfo_to_dict, make_conninfo
from psycopg_pool import ConnectionPool

from washburn_web import db

DATABASE_URL = os.environ.get("WASHBURN_TEST_DATABASE_URL")
MIGRATIONS = sorted((Path(__file__).resolve().parents[2] / "db" / "migrations").glob("*.sql"))

pytestmark = pytest.mark.skipif(not DATABASE_URL, reason="WASHBURN_TEST_DATABASE_URL is not set")


@pytest.fixture
def pool(monkeypatch: pytest.MonkeyPatch):
    import psycopg

    name = f"washburn_test_{uuid.uuid4().hex[:10]}"
    with psycopg.connect(DATABASE_URL, autocommit=True) as admin:
        admin.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
    target = make_conninfo(**{**conninfo_to_dict(DATABASE_URL), "dbname": name})
    p = ConnectionPool(target, min_size=1, max_size=2, open=True)
    monkeypatch.setattr(db, "_settings_cache", {})
    try:
        with p.connection() as conn:
            for migration in MIGRATIONS:  # every file, in filename order
                conn.execute(migration.read_text(encoding="utf-8"))
        monkeypatch.setattr(db, "_pool", p)
        yield p
    finally:
        monkeypatch.setattr(db, "_pool", None)
        p.close()
        with psycopg.connect(DATABASE_URL, autocommit=True) as admin:
            admin.execute(
                sql.SQL("DROP DATABASE IF EXISTS {} WITH (FORCE)").format(sql.Identifier(name))
            )


def _make_page(conn, url: str, body: bytes) -> int:
    """Insert blob, queue row and page; return page id. Seed must already exist."""
    seed_id, host = conn.execute("SELECT id, host FROM seeds ORDER BY id LIMIT 1").fetchone()
    sha = hashlib.sha256(body).digest()
    conn.execute(
        "INSERT INTO blobs (sha256, bytes, raw_len) VALUES (%s, %s, %s) ON CONFLICT DO NOTHING",
        (sha, zstandard.ZstdCompressor().compress(body), len(body)),
    )
    queue_id = conn.execute(
        """
        INSERT INTO queue (url, url_hash, host, seed_id, status)
        VALUES (%s, %s, %s, %s, 'done') RETURNING id
        """,
        (url, hashlib.sha256(url.encode()).digest(), host, seed_id),
    ).fetchone()[0]
    return conn.execute(
        """
        INSERT INTO pages (queue_id, url, final_url, seed_id, depth, status, fetched_at,
                           worker, body_sha256, text)
        VALUES (%s, %s, %s, %s, 0, 200, now(), 'test:1', %s, %s) RETURNING id
        """,
        (queue_id, url, url, seed_id, sha, body.decode("utf-8", "replace")),
    ).fetchone()[0]


def test_add_seed_inserts_seed_host_and_queue(pool):
    first = db.add_seed(url="HTTPS://Example.COM/start", added_by="tester")
    assert first["host"] == "example.com"
    assert first["queued"] is True

    second = db.add_seed(url="https://example.com/start", added_by="tester")
    assert second["queued"] is False
    assert second["seed_id"] == first["seed_id"]

    with pool.connection() as conn:
        assert conn.execute(
            "SELECT pending_count FROM hosts WHERE host = 'example.com'"
        ).fetchone() == (1,)
        url, url_hash, host = conn.execute("SELECT url, url_hash, host FROM queue").fetchone()
        assert url == "https://example.com/start"
        assert bytes(url_hash) == hashlib.sha256(url.encode()).digest()
        assert host == "example.com"
        assert conn.execute("SELECT host, enabled FROM seeds").fetchone() == ("example.com", True)


def test_add_seed_second_url_on_same_host_bumps_pending(pool):
    db.add_seed(url="https://example.com/a", added_by="tester")
    db.add_seed(url="https://example.com:443/b", added_by="tester")
    with pool.connection() as conn:
        assert conn.execute(
            "SELECT pending_count FROM hosts WHERE host = 'example.com'"
        ).fetchone() == (2,)
        assert conn.execute("SELECT count(*) FROM hosts").fetchone() == (1,)


def test_disable_seed(pool):
    seed = db.add_seed(url="https://example.org/", added_by="tester")
    assert db.disable_seed(seed["seed_id"]) is True
    assert db.disable_seed(999_999) is False
    with pool.connection() as conn:
        assert conn.execute("SELECT enabled FROM seeds").fetchone() == (False,)


def test_threshold_histogram_bins_and_judgments(pool):
    db.add_seed(url="https://example.com/", added_by="tester")
    with pool.connection() as conn:
        page_id = _make_page(conn, "https://example.com/p", b"some page text")
        routed = conn.execute(
            """
            INSERT INTO screenings (page_id, chunk, requested_model, model, importance,
              agent_activity, potentially_serious, level_one, threshold)
            VALUES (%s, 0, 'jev', 'jev', 0.72, 0.5, 0.5, true, 0.70) RETURNING id
            """,
            (page_id,),
        ).fetchone()[0]
        conn.execute(
            """
            INSERT INTO judgments (page_id, screening_id, chunk, requested_model, model,
              activity, seriousness, confidence, classification, confirmed_serious,
              summary, verdict)
            VALUES (%s, %s, 0, 'judge', 'judge', 0.9, 0.9, 0.9,
              'suspected_unauthorized', true, 'summary', '{}'::jsonb)
            """,
            (page_id, routed),
        )
        conn.execute(
            """
            INSERT INTO screenings (page_id, chunk, requested_model, model, importance,
              agent_activity, potentially_serious, level_one, threshold)
            VALUES (%s, 1, 'jev', 'jev', 0.12, 0.1, 0.1, false, 0.70)
            """,
            (page_id,),
        )

    rows = {r["bin"]: r for r in db.threshold_histogram()}
    assert rows[14] == {
        "bin": 14,
        "screenings": 1,
        "judged": 1,
        "confirmed_serious": 1,
    }
    assert rows[2]["screenings"] == 1
    assert rows[2]["judged"] == 0


def test_publish_and_unpublish_control_public_view(pool):
    db.add_seed(url="https://example.com/", added_by="tester")
    with pool.connection() as conn:
        page_id = _make_page(conn, "https://example.com/r", b"report source")
        finding_id = conn.execute(
            """
            INSERT INTO findings (page_id, kind, title, markdown)
            VALUES (%s, 'report', 'A report', '# A report') RETURNING id
            """,
            (page_id,),
        ).fetchone()[0]

    assert db.get_published_report(finding_id) is None
    assert db.set_finding_publication(finding_id, True, "reviewer", "ok") is True
    report = db.get_published_report(finding_id)
    assert report is not None and report["title"] == "A report"
    assert [r["id"] for r in db.list_published_reports(10, 0)] == [finding_id]
    stats = db.public_stats()
    assert stats["totals"]["published_findings"] == 1

    assert db.set_finding_publication(finding_id, False, "reviewer", None) is True
    assert db.get_published_report(finding_id) is None
    assert db.set_finding_publication(999_999, True, "reviewer", None) is False

    with pool.connection() as conn:
        reviewed_by, published_at = conn.execute(
            "SELECT reviewed_by, published_at FROM findings WHERE id = %s", (finding_id,)
        ).fetchone()
    assert reviewed_by == "reviewer"
    assert published_at is None


def test_body_round_trip(pool):
    db.add_seed(url="https://example.com/", added_by="tester")
    body = b"<html>round trip</html>"
    with pool.connection() as conn:
        page_id = _make_page(conn, "https://example.com/body", body)
    stored = db.get_page_body(page_id)
    assert zstandard.ZstdDecompressor().decompress(stored) == body
    assert db.get_page_body(999_999) is None


def test_search_pages_finds_text(pool):
    db.add_seed(url="https://example.com/", added_by="tester")
    with pool.connection() as conn:
        _make_page(conn, "https://example.com/s", b"the quick brown fox jumps")
    hits = db.search_pages("brown fox", 10)
    assert len(hits) == 1
    assert "brown" in hits[0]["snippet"]
    assert db.search_pages("elephant", 10) == []


def test_admin_threshold_page_against_real_db(pool, admin_client, admin_auth):
    db.add_seed(url="https://example.com/", added_by="tester")
    response = admin_client.get("/threshold", auth=admin_auth)
    assert response.status_code == 200
    assert "Importance threshold" in response.text


def test_bulk_add_seeds_then_repost_is_already_present(pool, admin_client, admin_auth):
    body = "https://a.test/one\nhttps://a.test/two\nb.test/start\n"
    response = admin_client.post("/seeds/bulk", data={"urls": body}, auth=admin_auth)
    assert response.status_code == 200
    assert "Added <strong>3</strong>" in response.text

    with pool.connection() as conn:
        assert conn.execute("SELECT count(*) FROM seeds").fetchone() == (3,)
        assert conn.execute("SELECT url FROM queue ORDER BY url").fetchall() == [
            ("https://a.test/one",),
            ("https://a.test/two",),
            ("https://b.test/start",),
        ]
        assert conn.execute(
            "SELECT depth, status::text FROM queue GROUP BY depth, status"
        ).fetchall() == [(0, "pending")]
        assert conn.execute("SELECT host, pending_count FROM hosts ORDER BY host").fetchall() == [
            ("a.test", 2),
            ("b.test", 1),
        ]

    response = admin_client.post("/seeds/bulk", data={"urls": body}, auth=admin_auth)
    assert response.status_code == 200
    assert "already present <strong>3</strong>" in response.text
    assert "Added <strong>0</strong>" in response.text
    with pool.connection() as conn:
        assert conn.execute("SELECT count(*) FROM seeds").fetchone() == (3,)
        assert conn.execute("SELECT count(*) FROM queue").fetchone() == (3,)
        assert conn.execute("SELECT host, pending_count FROM hosts ORDER BY host").fetchall() == [
            ("a.test", 2),
            ("b.test", 1),
        ]

    export = admin_client.get("/seeds/export", auth=admin_auth)
    assert export.text == "https://a.test/one\nhttps://a.test/two\nhttps://b.test/start\n"


def test_public_site_setting_round_trip(pool, admin_client, admin_auth):
    # The 0002 migration seeds the kill switch as on.
    assert db.get_setting("public_site_enabled", False) is True

    response = admin_client.post(
        "/settings/public-site", data={"enabled": "false"}, auth=admin_auth, follow_redirects=False
    )
    assert response.status_code == 303
    assert db.get_setting("public_site_enabled", True) is False  # cache cleared by set_setting

    row = db.get_setting_row("public_site_enabled")
    assert row["value"] is False
    assert row["updated_by"] == admin_auth[0]
    with pool.connection() as conn:
        assert conn.execute(
            "SELECT kind, detail FROM events WHERE kind = 'public_site_toggled'"
        ).fetchall() == [("public_site_toggled", {"enabled": False, "by": admin_auth[0]})]

    db.set_setting("public_site_enabled", True, "tester")
    assert db.get_setting("public_site_enabled", False) is True
    assert db.get_setting("no_such_key", "fallback") == "fallback"
