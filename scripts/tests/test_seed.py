"""Tests for scripts/seed.py: parsing and normalisation (no database), plus an
integration test gated on WASHBURN_TEST_DATABASE_URL."""

from __future__ import annotations

import hashlib
import os
import secrets
from pathlib import Path

import pytest
import seed
from conftest import REPO_ROOT

MIGRATION = REPO_ROOT / "db" / "migrations" / "0001_init.sql"


# ---------------------------------------------------------------- text input
def test_text_skips_blank_and_comment_lines() -> None:
    text = "# header\n\nexample.com\n   \n  # indented comment\n10.0.0.1\n"
    assert seed.parse_text(text) == ["example.com", "10.0.0.1"]


def test_bare_domain_gets_https() -> None:
    assert seed.normalize_target("example.com") == "https://example.com/"


def test_ip_gets_https() -> None:
    assert seed.normalize_target("192.0.2.7") == "https://192.0.2.7/"


def test_bare_ipv6_is_bracketed() -> None:
    assert seed.normalize_target("2001:db8::1") == "https://[2001:db8::1]/"


def test_explicit_http_is_kept() -> None:
    assert seed.normalize_target("http://example.com/a/b?x=1") == "http://example.com/a/b?x=1"


def test_fragment_is_stripped() -> None:
    assert seed.normalize_target("https://example.com/page#section") == "https://example.com/page"


def test_scheme_host_lowercased_and_empty_path_is_slash() -> None:
    assert seed.normalize_target("HTTPS://Example.COM") == "https://example.com/"


def test_default_port_dropped_and_non_default_kept() -> None:
    assert seed.normalize_target("https://example.com:443/") == "https://example.com/"
    assert seed.normalize_target("http://example.com:80") == "http://example.com/"
    assert seed.normalize_target("https://example.com:8443/x") == "https://example.com:8443/x"


@pytest.mark.parametrize(
    ("raw", "reason_fragment"),
    [
        ("ftp://example.com", "unsupported scheme"),
        ("N/A", "invalid host"),
        ("https://user:pw@example.com", "credentials"),
        ("https://example.com:notaport", "unparseable"),
        ("https://1.2.3.999", "invalid host"),
        ("   ", "empty"),
    ],
)
def test_invalid_targets_are_rejected_with_reason(raw: str, reason_fragment: str) -> None:
    with pytest.raises(ValueError, match=reason_fragment):
        seed.normalize_target(raw)


# ---------------------------------------------------------------- host key
@pytest.mark.parametrize(
    ("url", "host"),
    [
        ("https://Example.com/", "example.com"),
        ("https://example.com:443/", "example.com"),
        ("http://example.com:80/", "example.com"),
        ("http://example.com:8080/", "example.com:8080"),
        ("https://[::1]/", "[::1]"),
        ("https://[::1]:8443/", "[::1]:8443"),
        ("https://192.0.2.7/", "192.0.2.7"),
    ],
)
def test_host_key_normalisation(url: str, host: str) -> None:
    assert seed.host_key(seed.normalize_target(url)) == host


# ---------------------------------------------------------------- cells
def test_cell_single_target() -> None:
    assert seed.split_cell(" example.com ") == ["example.com"]


def test_cell_json_array() -> None:
    assert seed.split_cell('["https://a.example.com/", "b.example.org"]') == [
        "https://a.example.com/",
        "b.example.org",
    ]


def test_cell_semicolon_and_newline_separated() -> None:
    assert seed.split_cell("a.example.com; b.example.org\nc.example.net") == [
        "a.example.com",
        "b.example.org",
        "c.example.net",
    ]


def test_cell_bracketed_ipv6_is_not_json() -> None:
    assert seed.split_cell("[::1]:8080") == ["[::1]:8080"]


def test_cell_empty_is_no_targets() -> None:
    assert seed.split_cell("   ") == []


# ---------------------------------------------------------------- plan
def test_prepare_dedupes_after_normalisation_and_records_rejections() -> None:
    plan = seed.prepare(
        ["example.com", "https://EXAMPLE.com/#frag", "https://example.com", "ftp://x.example", "N/A"]
    )
    assert plan.urls == ["https://example.com/"]
    assert plan.duplicates == 2
    assert [r.raw for r in plan.rejections] == ["ftp://x.example", "N/A"]


def test_rejection_display_truncates_long_input() -> None:
    rej = seed.Rejection("x" * 500, "why")
    assert len(rej.show()) < 260


# ---------------------------------------------------------------- csv
def test_csv_reads_named_column_with_bom_and_mixed_cells(tmp_path: Path) -> None:
    path = tmp_path / "targets.csv"
    path.write_text(
        '﻿Name,Primary_URLs,Other\r\n'
        'a,"[""https://a.example.com/"", ""b.example.org""]",x\r\n'
        'b,"c.example.net;d.example.org",y\r\n'
        'c,,z\r\n',
        encoding="utf-8",
    )
    assert seed.read_csv(path, ["primary_urls"]) == [
        "https://a.example.com/",
        "b.example.org",
        "c.example.net",
        "d.example.org",
    ]


def test_csv_default_columns_pick_recognised_headers(tmp_path: Path) -> None:
    path = tmp_path / "t.csv"
    path.write_text("domain,ignored\nexample.com,1\n", encoding="utf-8")
    assert seed.read_csv(path, None) == ["example.com"]


def test_csv_missing_column_is_fatal(tmp_path: Path) -> None:
    path = tmp_path / "t.csv"
    path.write_text("domain\nexample.com\n", encoding="utf-8")
    with pytest.raises(seed.SeedError, match="primary_urls"):
        seed.read_csv(path, ["primary_urls"])


def test_csv_without_any_target_column_is_fatal(tmp_path: Path) -> None:
    path = tmp_path / "t.csv"
    path.write_text("name,notes\nx,y\n", encoding="utf-8")
    with pytest.raises(seed.SeedError, match="no recognised target column"):
        seed.read_csv(path, None)


# ---------------------------------------------------------------- integration
DB_URL = os.environ.get("WASHBURN_TEST_DATABASE_URL", "")


@pytest.mark.skipif(not DB_URL, reason="set WASHBURN_TEST_DATABASE_URL to run")
def test_seed_inserts_and_maintains_pending_count() -> None:
    psycopg = pytest.importorskip("psycopg")
    schema = f"washburn_seedtest_{secrets.token_hex(4)}"
    with psycopg.connect(DB_URL, autocommit=True) as admin:
        admin.execute(f'CREATE SCHEMA "{schema}"')
    kwargs = {"conninfo": DB_URL, "options": f"-c search_path={schema}"}
    try:
        with psycopg.connect(**kwargs, autocommit=True) as conn:
            conn.execute(MIGRATION.read_text(encoding="utf-8"))

        urls = ["https://a.example.com/one", "https://a.example.com/two", "https://b.example.org/"]
        counts = seed.seed_database(kwargs, urls, "integration", "pytest")
        assert (counts.added, counts.present) == (3, 0)

        with psycopg.connect(**kwargs, autocommit=True) as conn:
            pending = dict(conn.execute("SELECT host, pending_count FROM hosts").fetchall())
            assert pending == {"a.example.com": 2, "b.example.org": 1}
            digest = conn.execute(
                "SELECT url_hash FROM queue WHERE url = %s", ("https://a.example.com/one",)
            ).fetchone()[0]
            assert bytes(digest) == hashlib.sha256(b"https://a.example.com/one").digest()
            seeds = conn.execute("SELECT count(*) FROM seeds WHERE added_by = 'pytest'").fetchone()[0]
            assert seeds == 3

        # Re-seeding the same URLs is a no-op: already present, no double counting.
        again = seed.seed_database(kwargs, urls, None, None)
        assert (again.added, again.present) == (0, 3)
        with psycopg.connect(**kwargs, autocommit=True) as conn:
            pending = dict(conn.execute("SELECT host, pending_count FROM hosts").fetchall())
            assert pending == {"a.example.com": 2, "b.example.org": 1}
    finally:
        with psycopg.connect(DB_URL, autocommit=True) as admin:
            admin.execute(f'DROP SCHEMA IF EXISTS "{schema}" CASCADE')
