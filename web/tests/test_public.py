from datetime import UTC, datetime

from tests.conftest import non_gate_calls


def test_healthz(public_client):
    response = public_client.get("/healthz")
    assert response.status_code == 200
    assert response.json() == {"ok": True}


def test_unpublished_report_is_404(public_client, fake_db):
    fake_db.returns["get_published_report"] = None
    response = public_client.get("/reports/42")
    assert response.status_code == 404
    assert fake_db.called("get_published_report")[0][0] == (42,)


def test_published_report_renders_markdown_escaped(public_client, fake_db):
    fake_db.returns["get_published_report"] = {
        "id": 7,
        "title": "Example report",
        "markdown": "# Heading\n\n<script>alert('x')</script>\n\nBody text.",
        "published_at": datetime(2026, 10, 1, tzinfo=UTC),
        "final_url": "https://example.com/page",
        "fetched_at": datetime(2026, 9, 30, tzinfo=UTC),
    }
    response = public_client.get("/reports/7")
    assert response.status_code == 200
    assert "<h1>Heading</h1>" in response.text
    assert "<script>alert" not in response.text
    assert "&lt;script&gt;" in response.text
    assert "https://example.com/page" in response.text


def test_reports_list_pagination(public_client, fake_db):
    rows = [
        {
            "id": i,
            "title": f"r{i}",
            "published_at": datetime(2026, 10, 1, tzinfo=UTC),
            "final_url": "https://example.com/",
            "fetched_at": datetime(2026, 10, 1, tzinfo=UTC),
        }
        for i in range(26)
    ]
    fake_db.returns["list_published_reports"] = lambda limit, offset: rows[offset : offset + limit]
    response = public_client.get("/reports?page=1")
    assert response.status_code == 200
    assert "/reports?page=2" in response.text
    args, kwargs = fake_db.called("list_published_reports")[0]
    assert kwargs == {"limit": 26, "offset": 0}


def test_stats_page_renders(public_client, fake_db):
    fake_db.returns["public_stats"] = {
        "totals": {
            "pages": 10,
            "hosts": 2,
            "published_findings": 1,
            "last_published_at": datetime(2026, 10, 1, tzinfo=UTC),
        },
        "by_kind": [{"kind": "report", "n": 1}],
    }
    response = public_client.get("/")
    assert response.status_code == 200
    assert "pages crawled" in response.text
    assert "2026-10-01" in response.text


def test_public_app_has_no_admin_routes(public_client, fake_db):
    for path in ["/findings", "/pages", "/seeds", "/threshold", "/pages/1/body", "/docs"]:
        assert public_client.get(path).status_code == 404, path
    assert non_gate_calls(fake_db) == []


def test_public_app_has_no_write_routes(public_client, fake_db):
    assert public_client.post("/seeds", data={"url": "https://x.test/"}).status_code in (404, 405)
    assert non_gate_calls(fake_db) == []


def test_about_page(public_client, fake_db):
    response = public_client.get("/about")
    assert response.status_code == 200
    assert "reviewed" in response.text.lower()
