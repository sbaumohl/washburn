from datetime import UTC, datetime

import zstandard

from tests.conftest import ADMIN_PASSWORD, ADMIN_USER


def test_healthz_needs_no_auth(admin_client, fake_db):
    response = admin_client.get("/healthz")
    assert response.status_code == 200
    assert response.json() == {"ok": True}
    assert fake_db.calls == []


PROTECTED_GETS = [
    "/",
    "/findings",
    "/findings/1",
    "/pages",
    "/pages/1",
    "/pages/1/body",
    "/seeds",
    "/threshold",
]


def test_routes_401_without_auth(admin_client, fake_db):
    for path in PROTECTED_GETS:
        response = admin_client.get(path)
        assert response.status_code == 401, path
        assert response.headers["www-authenticate"].startswith("Basic"), path
    assert admin_client.post("/seeds", data={"url": "https://x.test/"}).status_code == 401
    assert admin_client.post("/findings/1/publish").status_code == 401
    assert admin_client.post("/findings/1/unpublish").status_code == 401
    assert admin_client.post("/seeds/1/disable").status_code == 401
    assert fake_db.calls == []


def test_wrong_password_is_401(admin_client, fake_db):
    response = admin_client.get("/", auth=(ADMIN_USER, "nope"))
    assert response.status_code == 401
    response = admin_client.get("/", auth=("someone", ADMIN_PASSWORD))
    assert response.status_code == 401
    assert fake_db.calls == []


def test_routes_200_with_auth(admin_client, fake_db, admin_auth):
    fake_db.returns["get_finding"] = {
        "id": 1,
        "page_id": 2,
        "kind": "report",
        "title": "T",
        "markdown": "# x",
        "created_at": datetime(2026, 10, 1, tzinfo=UTC),
        "updated_at": datetime(2026, 10, 1, tzinfo=UTC),
        "reviewed_by": None,
        "reviewed_at": None,
        "review_note": None,
        "published": False,
        "published_at": None,
        "url": "https://x.test/",
        "final_url": "https://x.test/",
        "status": 200,
        "fetched_at": datetime(2026, 10, 1, tzinfo=UTC),
    }
    fake_db.returns["get_page"] = {
        "id": 1,
        "url": "https://x.test/",
        "final_url": "https://x.test/",
        "status": 200,
        "content_type": "text/html",
        "fetched_at": datetime(2026, 10, 1, tzinfo=UTC),
        "worker": "w:1",
        "redirects": [],
        "links_count": 0,
        "text_head": "hello",
        "text_len": 5,
        "body_sha256": b"\x01\x02",
    }
    for path in PROTECTED_GETS:
        if path == "/pages/1/body":
            continue  # 404 by default stub; covered separately
        response = admin_client.get(path, auth=admin_auth)
        assert response.status_code == 200, path


def test_dashboard_with_data(admin_client, fake_db, admin_auth):
    fake_db.returns["flag_rates_24h"] = {
        "screenings": 100,
        "level_one": 10,
        "judgments": 10,
        "confirmed": 4,
        "downgraded": 5,
    }
    fake_db.returns["queue_depth"] = [{"host": "x.test", "pending": 7}]
    fake_db.returns["pages_per_hour"] = [
        {"hour": datetime(2026, 10, 1, 12, tzinfo=UTC), "pages": 3}
    ]
    response = admin_client.get("/", auth=admin_auth)
    assert response.status_code == 200
    assert "10.0%" in response.text  # flag rate
    assert "40.0%" in response.text  # confirm rate
    assert "50.0%" in response.text  # downgrade rate
    assert "x.test" in response.text


def test_publish_calls_db_with_auth_username(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/findings/9/publish",
        data={"review_note": "checked the quotes"},
        auth=admin_auth,
        follow_redirects=False,
    )
    assert response.status_code == 303
    assert response.headers["location"] == "/findings/9"
    args, kwargs = fake_db.called("set_finding_publication")[0]
    assert args == (9, True, ADMIN_USER, "checked the quotes")


def test_unpublish_calls_db_with_false(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/findings/9/unpublish",
        data={"review_note": ""},
        auth=admin_auth,
        follow_redirects=False,
    )
    assert response.status_code == 303
    args, _ = fake_db.called("set_finding_publication")[0]
    assert args == (9, False, ADMIN_USER, None)  # empty note is stored as NULL


def test_publish_unknown_finding_is_404(admin_client, fake_db, admin_auth):
    fake_db.returns["set_finding_publication"] = False
    response = admin_client.post("/findings/999/publish", data={}, auth=admin_auth)
    assert response.status_code == 404


def test_cross_site_post_refused(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/findings/9/publish",
        data={},
        auth=admin_auth,
        headers={"Sec-Fetch-Site": "cross-site"},
        follow_redirects=False,
    )
    assert response.status_code == 403
    assert fake_db.called("set_finding_publication") == []


def test_findings_filter_validation(admin_client, fake_db, admin_auth):
    assert admin_client.get("/findings?kind=bogus", auth=admin_auth).status_code == 400
    assert admin_client.get("/findings?published=maybe", auth=admin_auth).status_code == 400
    response = admin_client.get("/findings?kind=report&published=true", auth=admin_auth)
    assert response.status_code == 200
    _, kwargs = fake_db.called("list_findings")[0]
    assert kwargs["kind"] == "report"
    assert kwargs["published"] is True


def test_body_download_is_attachment_octet_stream(admin_client, fake_db, admin_auth):
    raw = b"<html>hello</html>" * 100
    fake_db.returns["get_page_body"] = zstandard.ZstdCompressor().compress(raw)
    response = admin_client.get("/pages/5/body", auth=admin_auth)
    assert response.status_code == 200
    assert response.content == raw
    assert response.headers["content-type"] == "application/octet-stream"
    assert response.headers["content-disposition"].startswith("attachment")
    assert response.headers["x-content-type-options"] == "nosniff"


def test_body_missing_page_is_404(admin_client, fake_db, admin_auth):
    assert admin_client.get("/pages/5/body", auth=admin_auth).status_code == 404


def test_seed_post_calls_add_seed(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/seeds",
        data={"url": "HTTPS://Example.COM:443/start"},
        auth=admin_auth,
        follow_redirects=False,
    )
    assert response.status_code == 303
    args, kwargs = fake_db.called("add_seed")[0]
    assert kwargs == {"url": "HTTPS://Example.COM:443/start", "added_by": ADMIN_USER}


def test_seed_post_rejects_non_http(admin_client, fake_db, admin_auth):
    response = admin_client.post("/seeds", data={"url": "ftp://x.test/"}, auth=admin_auth)
    assert response.status_code == 400
    assert fake_db.called("add_seed") == []


def test_disable_seed(admin_client, fake_db, admin_auth):
    response = admin_client.post("/seeds/3/disable", auth=admin_auth, follow_redirects=False)
    assert response.status_code == 303
    assert fake_db.called("disable_seed")[0][0] == (3,)
    fake_db.returns["disable_seed"] = False
    assert admin_client.post("/seeds/4/disable", auth=admin_auth).status_code == 404


def test_threshold_page_has_20_bins(admin_client, fake_db, admin_auth):
    fake_db.returns["threshold_histogram"] = [
        {"bin": 14, "screenings": 5, "judged": 2, "confirmed_serious": 1}
    ]
    response = admin_client.get("/threshold", auth=admin_auth)
    assert response.status_code == 200
    assert response.text.count("routed") == 10  # bins 0.50 through 0.95
    assert "0.50-0.55" in response.text


# ---------------------------------------------------------------- bulk seeds
MIXED_BULK = (
    "example.com\n"
    "http://Plain.test/x\n"
    "https://dup.test/a#frag\n"
    "# a comment\n"
    "\n"
    "junk!!\n"
    "https://dup.test/a\n"
)


def test_bulk_post_parses_mixed_input_and_calls_helper_once(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/seeds/bulk", data={"urls": MIXED_BULK, "note": "  spring list "}, auth=admin_auth
    )
    assert response.status_code == 200
    calls = fake_db.called("add_seeds")
    assert len(calls) == 1
    _, kwargs = calls[0]
    assert kwargs == {
        "urls": ["https://example.com/", "http://plain.test/x", "https://dup.test/a"],
        "note": "spring list",
        "added_by": ADMIN_USER,
    }


def test_bulk_summary_counts_and_escapes_rejections(admin_client, fake_db, admin_auth):
    fake_db.returns["add_seeds"] = {"added": 2, "already_present": 1}
    body = "example.com\nexample.org\nexample.net\n<script>alert(1)</script>\nhttps://ok.test\n"
    response = admin_client.post("/seeds/bulk", data={"urls": body}, auth=admin_auth)
    assert response.status_code == 200
    assert "Added <strong>2</strong>" in response.text
    assert "already present <strong>1</strong>" in response.text
    assert "rejected <strong>1</strong>" in response.text
    assert "invalid host" in response.text or "unparseable" in response.text
    assert "<script>alert(1)" not in response.text
    assert "&lt;script&gt;" in response.text


def test_bulk_rejection_display_is_capped_at_200(admin_client, fake_db, admin_auth):
    body = "\n".join(f"junk{i}!!" for i in range(250)) + "\nexample.com\n"
    response = admin_client.post("/seeds/bulk", data={"urls": body}, auth=admin_auth)
    assert response.status_code == 200
    assert "rejected <strong>250</strong>" in response.text
    assert "junk199!!" in response.text
    assert "junk200!!" not in response.text


def test_bulk_over_5000_lines_is_400(admin_client, fake_db, admin_auth):
    body = "\n".join(["example.com"] * 5001)
    response = admin_client.post("/seeds/bulk", data={"urls": body}, auth=admin_auth)
    assert response.status_code == 400
    assert "5001" in response.text
    assert fake_db.called("add_seeds") == []


def test_bulk_exactly_5000_lines_is_accepted(admin_client, fake_db, admin_auth):
    body = "\n".join(["example.com"] * 5000)
    response = admin_client.post("/seeds/bulk", data={"urls": body}, auth=admin_auth)
    assert response.status_code == 200
    assert fake_db.called("add_seeds")[0][1]["urls"] == ["https://example.com/"]


def test_bulk_empty_or_nothing_parses_is_400(admin_client, fake_db, admin_auth):
    assert admin_client.post("/seeds/bulk", data={}, auth=admin_auth).status_code == 400
    response = admin_client.post(
        "/seeds/bulk", data={"urls": "  \n# only a comment\n\n"}, auth=admin_auth
    )
    assert response.status_code == 400
    response = admin_client.post("/seeds/bulk", data={"urls": "ftp://x.test/"}, auth=admin_auth)
    assert response.status_code == 400
    assert "rejected <strong>1</strong>" in response.text
    assert fake_db.called("add_seeds") == []


def test_bulk_routes_401_without_auth(admin_client, fake_db):
    assert admin_client.post("/seeds/bulk", data={"urls": "example.com"}).status_code == 401
    assert admin_client.get("/seeds/export").status_code == 401
    assert fake_db.calls == []


def test_bulk_post_refused_cross_site(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/seeds/bulk",
        data={"urls": "example.com"},
        auth=admin_auth,
        headers={"Sec-Fetch-Site": "cross-site"},
    )
    assert response.status_code == 403
    response = admin_client.post(
        "/seeds/bulk",
        data={"urls": "example.com"},
        auth=admin_auth,
        headers={"Origin": "https://evil.test"},
    )
    assert response.status_code == 403
    assert fake_db.called("add_seeds") == []


def test_seeds_export_is_plain_text_one_per_line(admin_client, fake_db, admin_auth):
    fake_db.returns["list_enabled_seed_urls"] = ["https://a.test/", "http://b.test/x"]
    response = admin_client.get("/seeds/export", auth=admin_auth)
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/plain")
    assert response.text == "https://a.test/\nhttp://b.test/x\n"


# ---------------------------------------------------------------- public site toggle
def test_public_site_toggle_requires_auth(admin_client, fake_db):
    assert admin_client.post("/settings/public-site", data={"enabled": "false"}).status_code == 401
    assert fake_db.calls == []


def test_public_site_toggle_refused_cross_site(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/settings/public-site",
        data={"enabled": "false"},
        auth=admin_auth,
        headers={"Sec-Fetch-Site": "cross-site"},
        follow_redirects=False,
    )
    assert response.status_code == 403
    assert fake_db.called("set_setting") == []


def test_public_site_toggle_sets_value_and_records_event(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/settings/public-site", data={"enabled": "false"}, auth=admin_auth, follow_redirects=False
    )
    assert response.status_code == 303
    assert response.headers["location"] == "/"
    args, kwargs = fake_db.called("set_setting")[0]
    assert args == ("public_site_enabled", False, ADMIN_USER)
    args, _ = fake_db.called("record_event")[0]
    assert args == ("admin", "public_site_toggled", {"enabled": False, "by": ADMIN_USER})


def test_public_site_toggle_on(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/settings/public-site", data={"enabled": "true"}, auth=admin_auth, follow_redirects=False
    )
    assert response.status_code == 303
    args, _ = fake_db.called("set_setting")[0]
    assert args == ("public_site_enabled", True, ADMIN_USER)


def test_public_site_toggle_rejects_bad_value(admin_client, fake_db, admin_auth):
    response = admin_client.post(
        "/settings/public-site", data={"enabled": "maybe"}, auth=admin_auth
    )
    assert response.status_code == 400
    assert fake_db.called("set_setting") == []


def test_dashboard_shows_public_site_state(admin_client, fake_db, admin_auth):
    stamp = datetime(2026, 10, 2, 9, 30, tzinfo=UTC)
    fake_db.returns["get_setting_row"] = {
        "key": "public_site_enabled",
        "value": False,
        "updated_at": stamp,
        "updated_by": "carol",
    }
    response = admin_client.get("/", auth=admin_auth)
    assert response.status_code == 200
    assert "Public site:" in response.text
    assert "<strong>OFF</strong>" in response.text
    assert "2026-10-02 09:30:00" in response.text
    assert "carol" in response.text
    assert "Turn public site ON" in response.text


def test_dashboard_public_site_on_offers_off_button(admin_client, fake_db, admin_auth):
    fake_db.returns["get_setting_row"] = {
        "key": "public_site_enabled",
        "value": True,
        "updated_at": datetime(2026, 10, 2, tzinfo=UTC),
        "updated_by": None,
    }
    response = admin_client.get("/", auth=admin_auth)
    assert "<strong>ON</strong>" in response.text
    assert "Turn public site OFF" in response.text


def _finding_row() -> dict:
    stamp = datetime(2026, 10, 1, tzinfo=UTC)
    return {
        "id": 3,
        "page_id": 2,
        "kind": "report",
        "title": "T",
        "markdown": "# x",
        "created_at": stamp,
        "updated_at": stamp,
        "reviewed_by": None,
        "reviewed_at": None,
        "review_note": None,
        "published": False,
        "published_at": None,
        "url": "https://x.test/",
        "final_url": "https://x.test/",
        "status": 200,
        "fetched_at": stamp,
    }


def test_publish_warns_when_public_site_off(admin_client, fake_db, admin_auth):
    fake_db.returns["get_finding"] = _finding_row()
    fake_db.returns["get_setting"] = False
    response = admin_client.get("/findings/3", auth=admin_auth)
    assert response.status_code == 200
    assert "this report will not be visible until it is turned on" in response.text
    assert "/findings/3/publish" in response.text  # publishing is not blocked


def test_publish_has_no_warning_when_public_site_on(admin_client, fake_db, admin_auth):
    fake_db.returns["get_finding"] = _finding_row()
    response = admin_client.get("/findings/3", auth=admin_auth)
    assert response.status_code == 200
    assert "will not be visible" not in response.text
