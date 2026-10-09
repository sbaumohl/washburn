"""Pure URL helpers for seed input. No I/O, no database."""

from __future__ import annotations

import ipaddress
import re
from dataclasses import dataclass, field
from urllib.parse import urlsplit, urlunsplit

_DEFAULT_PORTS = {"http": 80, "https": 443}
_LABEL = re.compile(r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$")


class InvalidSeedUrl(ValueError):
    """The operator typed something that is not an absolute http(s) URL."""


def _split_netloc(parts) -> tuple[str, int | None]:
    host = parts.hostname
    if not host:
        raise InvalidSeedUrl("seed URL has no host")
    try:
        port = parts.port
    except ValueError as exc:
        raise InvalidSeedUrl("seed URL has an invalid port") from exc
    if ":" in host:  # IPv6 literal needs brackets in host[:port]
        host = f"[{host}]"
    return host, port


def normalise_host(url: str) -> str:
    """Return the scope key ``host[:port]`` for a URL, lowercase.

    Default ports (80 for http, 443 for https) are dropped so that
    ``https://x.test:443/`` and ``https://x.test/`` share one scope.
    """
    parts = urlsplit(url.strip())
    if parts.scheme.lower() not in _DEFAULT_PORTS:
        raise InvalidSeedUrl("seed URL must start with http:// or https://")
    host, port = _split_netloc(parts)
    if port is None or port == _DEFAULT_PORTS[parts.scheme.lower()]:
        return host.lower()
    return f"{host.lower()}:{port}"


def normalise_seed_url(url: str) -> str:
    """Canonical seed URL: lowercase scheme and host, path defaults to '/', no fragment."""
    parts = urlsplit(url.strip())
    scheme = parts.scheme.lower()
    if scheme not in _DEFAULT_PORTS:
        raise InvalidSeedUrl("seed URL must start with http:// or https://")
    host, port = _split_netloc(parts)
    netloc = host.lower()
    if port is not None and port != _DEFAULT_PORTS[scheme]:
        netloc = f"{netloc}:{port}"
    return urlunsplit((scheme, netloc, parts.path or "/", parts.query, ""))


# ---------------------------------------------------------------- bulk paste
# Mirrors scripts/seed.py (normalize_target, _is_valid_host, parse_text, prepare).
# Kept in sync by hand: the web app must not import scripts/.


@dataclass
class Rejection:
    raw: str
    reason: str


@dataclass
class BulkParse:
    urls: list[str] = field(default_factory=list)
    rejections: list[Rejection] = field(default_factory=list)
    duplicates: int = 0


def _bracket(host: str) -> str:
    return f"[{host}]" if ":" in host else host


def _is_valid_host(host: str) -> bool:
    try:
        ipaddress.ip_address(host)
        return True
    except ValueError:
        pass
    labels = host.split(".")
    if len(labels) < 2 or len(host) > 253:
        return False  # a name needs a dot
    if not all(_LABEL.match(label) for label in labels):
        return False
    return any(ch.isalpha() for ch in labels[-1])  # "1.2.3.999" is not a name


def normalise_target(raw: str) -> str:
    """Canonical URL for one pasted target. Bare domains and IPs get https://.

    Raises InvalidSeedUrl with a human-readable reason.
    """
    text = raw.strip()
    if not text:
        raise InvalidSeedUrl("empty")
    if "://" not in text:
        try:
            ipaddress.IPv6Address(text)
            text = f"[{text}]"  # bare IPv6 literal
        except ValueError:
            pass
        text = "https://" + text
    try:
        parts = urlsplit(text)
        port = parts.port
    except ValueError as exc:
        raise InvalidSeedUrl(f"unparseable ({exc})") from None
    scheme = parts.scheme.lower()
    if scheme not in _DEFAULT_PORTS:
        raise InvalidSeedUrl(f"unsupported scheme {scheme!r} (http or https only)")
    if parts.username is not None or parts.password is not None:
        raise InvalidSeedUrl("credentials in URL are not allowed")
    host = parts.hostname or ""
    if not host:
        raise InvalidSeedUrl("no host")
    if not _is_valid_host(host):
        raise InvalidSeedUrl(f"invalid host {host!r}")
    netloc = _bracket(host)
    if port is not None and port != _DEFAULT_PORTS[scheme]:
        netloc += f":{port}"
    return urlunsplit((scheme, netloc, parts.path or "/", parts.query, ""))


def parse_bulk_targets(text: str) -> BulkParse:
    """One target per line; blank lines and lines starting with '#' are ignored.

    Accepted URLs are canonical and deduplicated, in first-seen order.
    """
    plan = BulkParse()
    seen: set[str] = set()
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        try:
            url = normalise_target(stripped)
        except InvalidSeedUrl as exc:
            plan.rejections.append(Rejection(stripped, str(exc)))
            continue
        if url in seen:
            plan.duplicates += 1
            continue
        seen.add(url)
        plan.urls.append(url)
    return plan
