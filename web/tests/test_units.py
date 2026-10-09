import pytest

from washburn_web.config import ConfigError, parse_config, split_bind
from washburn_web.markdown import render_markdown
from washburn_web.urls import InvalidSeedUrl, normalise_host, normalise_seed_url


@pytest.mark.parametrize(
    "url, expected",
    [
        ("https://Example.COM/", "example.com"),
        ("HTTPS://Example.COM:443/start?x=1", "example.com"),
        ("http://example.com:80", "example.com"),
        ("http://Example.com:8080/a", "example.com:8080"),
        ("https://sub.example.com", "sub.example.com"),
        ("https://[2001:DB8::1]:8443/", "[2001:db8::1]:8443"),
        ("https://user:pw@example.com/", "example.com"),
    ],
)
def test_normalise_host(url, expected):
    assert normalise_host(url) == expected


@pytest.mark.parametrize(
    "url", ["ftp://x.test/", "example.com", "https://", "https://x.test:99999/"]
)
def test_normalise_host_rejects(url):
    with pytest.raises(InvalidSeedUrl):
        normalise_host(url)


def test_normalise_seed_url_canonical():
    assert normalise_seed_url("HTTPS://Example.COM") == "https://example.com/"
    assert normalise_seed_url("https://example.com:443/a#frag") == "https://example.com/a"
    assert normalise_seed_url("http://Example.com:8080/a?b=1") == "http://example.com:8080/a?b=1"


def test_markdown_escapes_raw_html():
    html = render_markdown('<script>alert("x")</script>\n\n<img src=x onerror=alert(1)>')
    assert "<script" not in html
    assert "<img" not in html
    assert "&lt;script&gt;" in html


def test_markdown_drops_javascript_links():
    html = render_markdown("[click](javascript:alert(1))")
    assert "href" not in html
    assert "<a " not in html


def test_markdown_keeps_normal_markdown():
    html = render_markdown("## Title\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |")
    assert "<h2>Title</h2>" in html
    assert "<li>one</li>" in html
    assert "<table>" in html


def test_config_repr_hides_passwords(config_file):
    from washburn_web.config import load_config

    cfg = load_config(config_file)
    text = repr(cfg)
    assert "db-secret-password" not in text
    assert "correct-horse-battery" not in text
    assert "db-secret-password" in cfg.postgres.conninfo()


def test_config_missing_section_is_clear_error():
    with pytest.raises(ConfigError, match=r"\[web\]"):
        parse_config({"postgres": {"host": "h", "db": "d", "user": "u", "password": "p"}})


def test_config_missing_field_is_clear_error():
    data = {
        "postgres": {"host": "h", "db": "d", "user": "u", "password": ""},
        "web": {"admin_user": "a", "admin_password": "b"},
    }
    with pytest.raises(ConfigError, match=r"\[postgres\]\.password"):
        parse_config(data)


def test_split_bind():
    assert split_bind("0.0.0.0:8000") == ("0.0.0.0", 8000)
    assert split_bind("[::]:8001") == ("::", 8001)
    with pytest.raises(ConfigError):
        split_bind("8000")


def test_search_snippet_escapes_page_text_and_marks_hits():
    from washburn_web.admin import _mark_snippet

    row = _mark_snippet({"snippet": "a <script>x</script> \x02brown\x03 fox"})
    html_out = str(row["snippet_html"])
    assert "<script>" not in html_out
    assert "&lt;script&gt;" in html_out
    assert "<mark>brown</mark>" in html_out
