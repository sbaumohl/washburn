"""Shared fixtures. Unit tests never touch Postgres: every query function in
``washburn_web.db`` is replaced by a recording stub."""

from __future__ import annotations

from typing import Any

import pytest
from fastapi.testclient import TestClient

from washburn_web import db
from washburn_web.config import get_config

ADMIN_USER = "operator"
ADMIN_PASSWORD = "correct-horse-battery"

DB_FUNCTIONS = [
    "public_stats",
    "list_published_reports",
    "get_published_report",
    "live_workers",
    "queue_depth",
    "pages_per_hour",
    "flag_rates_24h",
    "recent_events",
    "list_findings",
    "get_finding",
    "get_page_screenings",
    "get_page_judgments",
    "set_finding_publication",
    "search_pages",
    "get_page",
    "get_page_body",
    "list_seeds",
    "add_seed",
    "add_seeds",
    "list_enabled_seed_urls",
    "disable_seed",
    "get_setting",
    "get_setting_row",
    "set_setting",
    "record_event",
    "threshold_histogram",
]

DEFAULT_RETURNS: dict[str, Any] = {
    "public_stats": {"totals": {"pages": 0, "hosts": 0}, "by_kind": []},
    "flag_rates_24h": {},
    "set_finding_publication": True,
    "add_seed": {"seed_id": 1, "host": "example.com", "queued": True},
    "disable_seed": True,
    "add_seeds": {"added": 0, "already_present": 0},
    "get_setting": True,  # public site is on unless a test says otherwise
}


class FakeDB:
    def __init__(self) -> None:
        self.returns: dict[str, Any] = {}
        self.calls: list[tuple[str, tuple, dict]] = []

    def _stub(self, name: str):
        def stub(*args: Any, **kwargs: Any) -> Any:
            self.calls.append((name, args, kwargs))
            if name in self.returns:
                value = self.returns[name]
            elif name in DEFAULT_RETURNS:
                value = DEFAULT_RETURNS[name]
            elif name.startswith(
                ("list_", "search_", "live_", "queue_", "pages_", "recent_")
            ) or name in (
                "get_page_screenings",
                "get_page_judgments",
                "threshold_histogram",
            ):
                value = []
            else:
                value = None
            return value(*args, **kwargs) if callable(value) else value

        return stub

    def called(self, name: str) -> list[tuple[tuple, dict]]:
        return [(args, kwargs) for n, args, kwargs in self.calls if n == name]


def non_gate_calls(fake: FakeDB) -> list:
    """Calls other than the public-site gate's own settings read."""
    return [c for c in fake.calls if c[0] != "get_setting"]


@pytest.fixture
def fake_db(monkeypatch: pytest.MonkeyPatch) -> FakeDB:
    fake = FakeDB()
    for name in DB_FUNCTIONS:
        monkeypatch.setattr(db, name, fake._stub(name))
    return fake


@pytest.fixture
def config_file(tmp_path, monkeypatch: pytest.MonkeyPatch):
    path = tmp_path / "washburn.toml"
    path.write_text(
        f"""
[postgres]
host = "127.0.0.1"
port = 1
db = "washburn"
user = "washburn"
password = "db-secret-password"

[web]
admin_user = "{ADMIN_USER}"
admin_password = "{ADMIN_PASSWORD}"
public_bind = "127.0.0.1:8000"
admin_bind = "127.0.0.1:8001"
""",
        encoding="utf-8",
    )
    monkeypatch.setenv("WASHBURN_CONFIG", str(path))
    get_config.cache_clear()
    yield path
    get_config.cache_clear()


@pytest.fixture
def public_client(config_file):
    from washburn_web.public import app

    # No `with` block: the lifespan (which opens the pool) is not run.
    return TestClient(app)


@pytest.fixture
def admin_client(config_file):
    from washburn_web.admin import app

    return TestClient(app)


@pytest.fixture
def admin_auth() -> tuple[str, str]:
    return (ADMIN_USER, ADMIN_PASSWORD)
