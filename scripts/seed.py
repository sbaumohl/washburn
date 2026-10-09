#!/usr/bin/env python3
"""Add crawl seeds to Washburn's Postgres (seeds, hosts, queue).

Run from the operator machine on the tailnet, with psycopg available:

    uv run --with 'psycopg[binary]' scripts/seed.py --url https://example.com
    uv run --with 'psycopg[binary]' scripts/seed.py --file targets.txt --dry-run
    uv run --with 'psycopg[binary]' scripts/seed.py --csv out.csv --column primary_urls

Target parsing follows crates/crawler/README.md ("Inputs and crawling"). URL
normalisation mirrors the Rust `url` crate's serialisation so that a URL seeded
here and the same URL discovered by the sweeper share one queue row and one
url_hash: lowercase scheme and host, fragment dropped, default port dropped,
empty path becomes "/".

Exit codes: 0 ok (rejections are listed, not fatal), 1 no valid targets or a
database error, 2 missing config or bad arguments.
"""

from __future__ import annotations

import argparse
import csv
import getpass
import hashlib
import ipaddress
import json
import re
import sys
from collections.abc import Iterable
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

import tomllib

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CONFIG = "washburn.toml"
# Recognised CSV columns when --column is not given (crawler README).
DEFAULT_COLUMNS = ("target", "url", "domain", "ip", "target_domain", "primary_urls")
DEFAULT_PORTS = {"http": 80, "https": 443}
_LABEL = re.compile(r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$")
_MAX_SHOWN = 200


class SeedError(Exception):
    """Fatal input problem (bad CSV, missing column, bad config). Exit 1."""


class MissingConfigError(SeedError):
    """washburn.toml does not exist. Exit 2."""


@dataclass
class Rejection:
    raw: str
    reason: str

    def show(self) -> str:
        raw = self.raw if len(self.raw) <= _MAX_SHOWN else self.raw[:_MAX_SHOWN] + "..."
        return f"{raw!r}: {self.reason}"


@dataclass
class TargetPlan:
    urls: list[str] = field(default_factory=list)
    rejections: list[Rejection] = field(default_factory=list)
    duplicates: int = 0


@dataclass
class Counts:
    added: int = 0
    present: int = 0


# ---------------------------------------------------------------- parsing
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
        return False  # a name needs a dot: rejects "N/A"-style noise
    if not all(_LABEL.match(label) for label in labels):
        return False
    return any(ch.isalpha() for ch in labels[-1])  # "1.2.3.999" is not a name


def normalize_target(raw: str) -> str:
    """Return the canonical URL for one target, or raise ValueError(reason)."""
    text = raw.strip()
    if not text:
        raise ValueError("empty")
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
        raise ValueError(f"unparseable ({exc})") from None
    scheme = parts.scheme.lower()
    if scheme not in DEFAULT_PORTS:
        raise ValueError(f"unsupported scheme {scheme!r} (http or https only)")
    if parts.username is not None or parts.password is not None:
        raise ValueError("credentials in URL are not allowed")
    host = parts.hostname or ""
    if not host:
        raise ValueError("no host")
    if not _is_valid_host(host):
        raise ValueError(f"invalid host {host!r}")
    netloc = _bracket(host)
    if port is not None and port != DEFAULT_PORTS[scheme]:
        netloc += f":{port}"
    return urlunsplit((scheme, netloc, parts.path or "/", parts.query, ""))


def host_key(url: str) -> str:
    """hosts/seeds key: lowercase host, plus :port only when non-default."""
    parts = urlsplit(url)
    host = _bracket(parts.hostname or "")
    port = parts.port
    if port is not None and port != DEFAULT_PORTS.get(parts.scheme.lower()):
        host += f":{port}"
    return host


def split_cell(cell: str) -> list[str]:
    """One CSV cell: a JSON array of strings, or ';'/newline separated targets."""
    text = cell.strip()
    if not text:
        return []
    if text.startswith("["):
        try:
            value = json.loads(text)
        except json.JSONDecodeError:
            value = None  # e.g. "[::1]:8080": fall through to plain splitting
        if isinstance(value, list) and all(isinstance(v, str) for v in value):
            return [v.strip() for v in value if v.strip()]
    return [part.strip() for part in re.split(r"[;\r\n]+", text) if part.strip()]


def parse_text(text: str) -> list[str]:
    """Plain text: one target per line; blank lines and '#' comments ignored."""
    lines = (line.strip() for line in text.splitlines())
    return [line for line in lines if line and not line.startswith("#")]


def read_csv(path: Path, columns: list[str] | None) -> list[str]:
    with path.open(encoding="utf-8-sig", newline="") as fh:
        reader = csv.DictReader(fh)
        header = reader.fieldnames or []
        by_lower = {name.strip().lower(): name for name in header}
        if columns:
            wanted = [c.lower() for c in columns]
            missing = [c for c in wanted if c not in by_lower]
            if missing:
                raise SeedError(f"{path}: column(s) not in header: {', '.join(missing)}")
        else:
            wanted = list(DEFAULT_COLUMNS)
        use = [by_lower[c] for c in wanted if c in by_lower]
        if not use:
            raise SeedError(f"{path}: no recognised target column (expected one of {', '.join(DEFAULT_COLUMNS)})")
        targets: list[str] = []
        for row in reader:
            for col in use:
                targets.extend(split_cell(row.get(col) or ""))
    return targets


def prepare(raw_items: Iterable[str]) -> TargetPlan:
    plan = TargetPlan()
    seen: set[str] = set()
    for raw in raw_items:
        try:
            url = normalize_target(raw)
        except ValueError as exc:
            plan.rejections.append(Rejection(raw, str(exc)))
            continue
        if url in seen:
            plan.duplicates += 1
            continue
        seen.add(url)
        plan.urls.append(url)
    return plan


# ---------------------------------------------------------------- database
def load_connect_kwargs(config_path: Path) -> dict:
    if not config_path.is_file():
        raise MissingConfigError(
            f"{config_path} not found. Pass --config, or copy washburn.example.toml "
            "to washburn.toml and fill in [postgres]."
        )
    try:
        pg = tomllib.loads(config_path.read_text(encoding="utf-8"))["postgres"]
        return {
            "host": pg["host"],
            "port": int(pg["port"]),
            "dbname": pg["db"],
            "user": pg["user"],
            "password": pg["password"],
        }
    except (KeyError, TypeError, ValueError) as exc:
        raise SeedError(f"{config_path}: [postgres] needs host, port, db, user, password ({exc})") from None


def seed_database(
    connect_kwargs: dict, urls: list[str], note: str | None, added_by: str | None
) -> Counts:
    """One transaction per URL. psycopg is imported here so parsing works without it."""
    import psycopg

    counts = Counts()
    with psycopg.connect(**connect_kwargs, autocommit=True) as conn:
        for url in urls:
            host = host_key(url)
            with conn.transaction():
                row = conn.execute(
                    "INSERT INTO seeds (url, host, note, added_by) VALUES (%s, %s, %s, %s) "
                    "ON CONFLICT (url) DO NOTHING RETURNING id",
                    (url, host, note, added_by),
                ).fetchone()
                if row is None:
                    counts.present += 1
                    continue
                seed_id = row[0]
                # hosts before queue: queue.host has a foreign key to hosts.
                conn.execute(
                    "INSERT INTO hosts (host) VALUES (%s) ON CONFLICT (host) DO NOTHING",
                    (host,),
                )
                queued = conn.execute(
                    "INSERT INTO queue (url, url_hash, host, seed_id, depth) "
                    "VALUES (%s, %s, %s, %s, 0) ON CONFLICT (url) DO NOTHING RETURNING id",
                    (url, hashlib.sha256(url.encode("utf-8")).digest(), host, seed_id),
                ).fetchone()
                if queued is not None:
                    conn.execute(
                        "UPDATE hosts SET pending_count = pending_count + 1 WHERE host = %s",
                        (host,),
                    )
                counts.added += 1
    return counts


# ---------------------------------------------------------------- CLI
def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Seed Washburn crawl targets.")
    parser.add_argument("--config", default=DEFAULT_CONFIG, help="path to washburn.toml")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--url", nargs="+", action="extend", dest="urls", metavar="URL",
                        help="one or more targets (domain, IP or URL)")
    source.add_argument("--file", action="append", dest="files", metavar="FILE",
                        help="text file, one target per line (repeatable)")
    source.add_argument("--csv", action="append", dest="csvs", metavar="FILE",
                        help="CSV file (repeatable); see --column")
    parser.add_argument("--column", action="extend", nargs="+", dest="columns",
                        help="CSV column(s) to read; default: all recognised target columns")
    parser.add_argument("--note", help="note stored on each new seed")
    parser.add_argument("--added-by", dest="added_by", default=_default_user(),
                        help="operator name stored on each new seed (default: OS user)")
    parser.add_argument("--dry-run", action="store_true",
                        help="parse and print what would be seeded; no database access")
    args = parser.parse_args(argv)
    if args.columns and not args.csvs:
        parser.error("--column only applies with --csv")
    return args


def _default_user() -> str | None:
    try:
        return getpass.getuser()
    except Exception:  # noqa: BLE001  (no login name in some containers)
        return None


def collect(args: argparse.Namespace) -> TargetPlan:
    raw: list[str] = list(args.urls or [])
    for name in args.files or []:
        raw += parse_text(Path(name).read_text(encoding="utf-8-sig"))
    for name in args.csvs or []:
        raw += read_csv(Path(name), args.columns)
    return prepare(raw)


def print_rejections(plan: TargetPlan) -> None:
    if plan.rejections:
        print(f"Rejected {len(plan.rejections)} target(s):")
        for rej in plan.rejections:
            print(f"  - {rej.show()}")


def run(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        plan = collect(args)
    except MissingConfigError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except SeedError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except (OSError, csv.Error, UnicodeDecodeError) as exc:
        print(f"error: cannot read input: {exc}", file=sys.stderr)
        return 1

    print(f"Parsed {len(plan.urls)} unique valid target(s); {plan.duplicates} duplicate(s) dropped.")
    if not plan.urls:
        print_rejections(plan)
        print("error: no valid targets; nothing to seed.", file=sys.stderr)
        return 1

    if args.dry_run:
        for url in plan.urls:
            print(f"  would seed {url}  (host {host_key(url)})")
        print_rejections(plan)
        print("Dry run: no database access.")
        return 0

    try:
        connect_kwargs = load_connect_kwargs(Path(args.config))
    except MissingConfigError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except SeedError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    try:
        counts = seed_database(connect_kwargs, plan.urls, args.note, args.added_by)
    except Exception as exc:  # noqa: BLE001  (psycopg errors or ImportError)
        print(f"error: seeding failed: {exc}", file=sys.stderr)
        if isinstance(exc, ImportError):
            print("hint: run with: uv run --with 'psycopg[binary]' scripts/seed.py ...", file=sys.stderr)
        return 1

    print(f"added: {counts.added}")
    print(f"already present: {counts.present}")
    print(f"rejected: {len(plan.rejections)}")
    print_rejections(plan)
    return 0


if __name__ == "__main__":
    sys.exit(run())
