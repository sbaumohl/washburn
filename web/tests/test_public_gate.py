"""Public-site kill switch: every route except /healthz is 503 while the setting is off."""

import pytest

from tests.conftest import non_gate_calls

PUBLIC_PATHS = ["/", "/reports", "/reports/1", "/about", "/does-not-exist"]


def _assert_offline(response):
    assert response.status_code == 503
    assert response.headers["retry-after"] == "300"
    assert response.headers["cache-control"] == "no-store"
    assert "temporarily offline" in response.text


def test_disabled_returns_503_on_every_route_except_healthz(public_client, fake_db):
    fake_db.returns["get_setting"] = False
    for path in PUBLIC_PATHS:
        _assert_offline(public_client.get(path))
    assert public_client.get("/healthz").status_code == 200
    assert non_gate_calls(fake_db) == []  # no data reads while offline


def test_disabled_offline_page_carries_no_data(public_client, fake_db):
    fake_db.returns["get_setting"] = False
    fake_db.returns["public_stats"] = {"totals": {"pages": 999}, "by_kind": []}
    response = public_client.get("/")
    assert "999" not in response.text
    assert fake_db.called("public_stats") == []


def test_enabled_serves_normally(public_client, fake_db):
    fake_db.returns["get_setting"] = True
    assert public_client.get("/").status_code == 200
    assert public_client.get("/about").status_code == 200
    assert public_client.get("/healthz").status_code == 200


def test_setting_read_error_fails_closed(public_client, fake_db):
    def boom(*args, **kwargs):
        raise RuntimeError("database password=hunter2 is wrong")

    fake_db.returns["get_setting"] = boom
    for path in ["/", "/about", "/reports"]:
        _assert_offline(public_client.get(path))
    assert public_client.get("/healthz").status_code == 200


def test_non_boolean_stored_value_fails_closed(public_client, fake_db):
    fake_db.returns["get_setting"] = "true"
    _assert_offline(public_client.get("/about"))


@pytest.fixture
def cache_reset(monkeypatch):
    from washburn_web import db

    monkeypatch.setattr(db, "_settings_cache", {})
    return db


def test_get_setting_caches_for_ttl(cache_reset, monkeypatch):
    db = cache_reset
    calls = []

    def fake_fetch(sql, params=None):
        calls.append(params)
        return {"value": True}

    monkeypatch.setattr(db, "_fetch_one", fake_fetch)
    monkeypatch.setattr(db, "SETTINGS_CACHE_TTL", 60.0)
    assert db.get_setting("public_site_enabled", False) is True
    assert db.get_setting("public_site_enabled", False) is True
    assert len(calls) == 1


def test_get_setting_ttl_is_overridable(cache_reset, monkeypatch):
    db = cache_reset
    calls = []
    monkeypatch.setattr(
        db, "_fetch_one", lambda sql, params=None: calls.append(1) or {"value": True}
    )
    monkeypatch.setattr(db, "SETTINGS_CACHE_TTL", 0.0)
    db.get_setting("public_site_enabled", False)
    db.get_setting("public_site_enabled", False)
    assert len(calls) == 2


def test_get_setting_missing_row_returns_default_uncached(cache_reset, monkeypatch):
    db = cache_reset
    monkeypatch.setattr(db, "_fetch_one", lambda sql, params=None: None)
    assert db.get_setting("public_site_enabled", False) is False
    assert db._settings_cache == {}
