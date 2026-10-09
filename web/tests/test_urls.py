"""Bulk paste parsing: mirrors the scripts/seed.py rules."""

import pytest

from washburn_web.urls import InvalidSeedUrl, normalise_target, parse_bulk_targets


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("example.com", "https://example.com/"),
        ("  Example.COM:443/Start?q=1#top  ", "https://example.com/Start?q=1"),
        ("http://a.test:80", "http://a.test/"),
        ("http://a.test:8080/x", "http://a.test:8080/x"),
        ("10.0.0.1", "https://10.0.0.1/"),
        ("::1", "https://[::1]/"),
        ("[::1]:8443", "https://[::1]:8443/"),
    ],
)
def test_normalise_target_accepts(raw, expected):
    assert normalise_target(raw) == expected


@pytest.mark.parametrize(
    ("raw", "reason_part"),
    [
        ("", "empty"),
        ("ftp://x.test/", "unsupported scheme"),
        ("https://user:pw@x.test/", "credentials"),
        ("localhost", "invalid host"),
        ("1.2.3.999", "invalid host"),
        ("junk!!", "invalid host"),
        ("https://x.test:99999/", "unparseable"),
    ],
)
def test_normalise_target_rejects_with_reason(raw, reason_part):
    with pytest.raises(InvalidSeedUrl) as info:
        normalise_target(raw)
    assert reason_part in str(info.value)


def test_parse_bulk_targets_mixed_input():
    text = "a.test\n# note\n\nhttp://B.test/x\nhttps://a.test/#frag\njunk!!\nhttp://b.test/x\n"
    plan = parse_bulk_targets(text)
    assert plan.urls == ["https://a.test/", "http://b.test/x"]
    assert plan.duplicates == 2
    assert [r.raw for r in plan.rejections] == ["junk!!"]


def test_parse_bulk_targets_handles_crlf_and_whitespace():
    plan = parse_bulk_targets("a.test\r\n   \r\nb.test\r\n")
    assert plan.urls == ["https://a.test/", "https://b.test/"]
