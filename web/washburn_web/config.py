"""Load the shared washburn.toml. Both apps read the same file.

The password fields are excluded from ``repr`` so they cannot end up in logs
or tracebacks by accident.
"""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Any

from psycopg.conninfo import make_conninfo

DEFAULT_CONFIG_PATH = "/etc/washburn/washburn.toml"


class ConfigError(RuntimeError):
    """Raised when washburn.toml is missing or incomplete."""


@dataclass(frozen=True)
class PostgresConfig:
    host: str
    port: int
    db: str
    user: str
    password: str = field(repr=False)

    def conninfo(self) -> str:
        return make_conninfo(
            host=self.host,
            port=self.port,
            dbname=self.db,
            user=self.user,
            password=self.password,
        )


@dataclass(frozen=True)
class WebConfig:
    admin_user: str
    admin_password: str = field(repr=False)
    public_bind: str
    admin_bind: str


@dataclass(frozen=True)
class Config:
    postgres: PostgresConfig
    web: WebConfig
    importance_threshold: float = 0.50  # [crawl].importance_threshold, shown on /threshold


def _section(data: dict[str, Any], name: str) -> dict[str, Any]:
    value = data.get(name)
    if not isinstance(value, dict):
        raise ConfigError(f"missing [{name}] table in config file")
    return value


def _require(section: dict[str, Any], name: str, table: str) -> Any:
    if section.get(name) in (None, ""):
        raise ConfigError(f"missing [{table}].{name} in config file")
    return section[name]


def parse_config(data: dict[str, Any]) -> Config:
    pg = _section(data, "postgres")
    web = _section(data, "web")
    postgres = PostgresConfig(
        host=str(_require(pg, "host", "postgres")),
        port=int(pg.get("port", 5432)),
        db=str(_require(pg, "db", "postgres")),
        user=str(_require(pg, "user", "postgres")),
        password=str(_require(pg, "password", "postgres")),
    )
    web_cfg = WebConfig(
        admin_user=str(_require(web, "admin_user", "web")),
        admin_password=str(_require(web, "admin_password", "web")),
        public_bind=str(web.get("public_bind", "0.0.0.0:8000")),
        admin_bind=str(web.get("admin_bind", "0.0.0.0:8001")),
    )
    crawl = data.get("crawl") if isinstance(data.get("crawl"), dict) else {}
    return Config(
        postgres=postgres,
        web=web_cfg,
        importance_threshold=float(crawl.get("importance_threshold", 0.50)),
    )


def load_config(path: str | os.PathLike[str] | None = None) -> Config:
    """Read the TOML file at ``path``, or at $WASHBURN_CONFIG, or the default."""
    config_path = Path(path or os.environ.get("WASHBURN_CONFIG", DEFAULT_CONFIG_PATH))
    try:
        with config_path.open("rb") as fh:
            data = tomllib.load(fh)
    except FileNotFoundError as exc:
        raise ConfigError(f"config file not found: {config_path}") from exc
    return parse_config(data)


@lru_cache(maxsize=1)
def get_config() -> Config:
    """Process-wide config, loaded once. Tests call ``get_config.cache_clear()``."""
    return load_config()


def split_bind(bind: str) -> tuple[str, int]:
    """Split ``host:port`` (IPv6 in brackets allowed) into its parts."""
    if bind.startswith("["):
        host, _, rest = bind[1:].partition("]")
        port = rest.removeprefix(":")
    else:
        host, _, port = bind.rpartition(":")
    if not host or not port:
        raise ConfigError(f"bind address must be host:port, got {bind!r}")
    return host, int(port)
