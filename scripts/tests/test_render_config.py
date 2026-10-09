"""Tests for scripts/render-config.py. Run as subprocesses, like an operator."""

from __future__ import annotations

import importlib.util
import json
import re
import secrets
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any

import tomllib
from conftest import REPO_ROOT

SCRIPT = REPO_ROOT / "scripts" / "render-config.py"
EXAMPLE = REPO_ROOT / "washburn.example.toml"
SECRET_KEYS = {"api_key", "password", "borg_passphrase", "auth_key", "admin_password"}
SECRET_FILES = [
    "infra/terraform.tfvars.json",
    "config/crawler.toml",
    "config/web.toml",
    "deploy/compose/crawler.env",
    "deploy/compose/data.env",
    "deploy/compose/web.env",
    "deploy/backup/borg.env",
]
ALL_FILES = [*SECRET_FILES, "deploy/caddy/Caddyfile"]
CRAWLER_TABLES = {"typesafe", "openrouter", "postgres", "crawl", "judge"}
WEB_TABLES = {"postgres", "web", "domain", "crawl"}
TFVARS_KEYS = {
    "vultr_api_key", "region", "os_id", "ssh_key_ids", "plans", "counts",
    "tailscale_auth_key", "tailnet", "public_host", "enable_vpc", "project_tag",
    "enable_reserved_ip",
}


def run_render(config: Path, out: Path, *, allow: bool = False) -> subprocess.CompletedProcess:
    cmd = [sys.executable, "-I", str(SCRIPT), "--config", str(config), "--out", str(out)]
    if allow:
        cmd.append("--allow-placeholders")
    return subprocess.run(cmd, capture_output=True, text=True, cwd="/", check=False)


def make_config(
    tmp: Path,
    values: dict[str, str] | None = None,
    extra: str = "",
    *,
    substitute: bool = True,
) -> Path:
    """Copy the example. With substitute=True, replace every REPLACE line:
    secret fields get random tokens, `values` overrides by field name, others get
    harmless text. With substitute=False the placeholders are left as they are.
    """
    values = values or {}
    lines = []
    for line in EXAMPLE.read_text(encoding="utf-8").splitlines():
        if not substitute:
            lines.append(line)
            continue
        m = re.match(r'^(\s*)([A-Za-z_]+)(\s*=\s*)(.*)$', line)
        if m and m.group(4).startswith(('"REPLACE', '["REPLACE')):
            name = m.group(2)
            if name in values:
                new = values[name]
            elif name in SECRET_KEYS:
                new = secrets.token_hex(16)
            elif name == "ssh_key_ids":
                new = None
            else:
                new = "replaced-value"
            if new is None:
                line = f'{m.group(1)}{name}{m.group(3)}["12345"]'
            else:
                escaped = new.replace("\\", "\\\\").replace('"', '\\"')
                line = f'{m.group(1)}{name}{m.group(3)}"{escaped}"'
        lines.append(line)
    path = tmp / "washburn.toml"
    path.write_text("\n".join(lines) + "\n" + extra, encoding="utf-8")
    return path


def secret_tokens(config_text: str) -> list[str]:
    return re.findall(r'^\s*(?:api_key|password|borg_passphrase|auth_key|admin_password)\s*=\s*"([^"]+)"',
                      config_text, flags=re.MULTILINE)


def test_renders_all_files_with_placeholders_allowed(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    out = tmp_path / "out"
    result = run_render(config, out, allow=True)
    assert result.returncode == 0, result.stderr
    for rel in ALL_FILES:
        assert (out / rel).is_file(), rel
    tfvars = json.loads((out / "infra/terraform.tfvars.json").read_text())
    assert set(tfvars) == TFVARS_KEYS
    assert tfvars["enable_vpc"] is False
    assert tfvars["project_tag"] == "washburn"
    assert tfvars["enable_reserved_ip"] is False
    assert tfvars["plans"] == {"crawler": "vc2-2c-4gb", "data": "vhp-4c-8gb-amd", "web": "vc2-2c-4gb"}
    assert tfvars["counts"] == {"crawler": 1}
    # The full config is never rendered; the data VM gets no TOML at all.
    assert not (out / "washburn.toml").exists()
    assert not (out / "config" / "data.toml").exists()


def test_env_contract_per_role(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    out = tmp_path / "out"
    assert run_render(config, out, allow=True).returncode == 0

    def keys(rel: str) -> set[str]:
        return {line.partition("=")[0] for line in (out / rel).read_text().splitlines()
                if line and not line.startswith("#")}

    crawler = keys("deploy/compose/crawler.env")
    assert {"WASHBURN_WORKERS", "WASHBURN_IMAGE_SWEEPER", "WASHBURN_IMAGE_JUDGE",
            "WASHBURN_TAILNET"} <= crawler
    data = keys("deploy/compose/data.env")
    assert {"POSTGRES_DB", "POSTGRES_USER", "POSTGRES_PASSWORD", "WASHBURN_BACKUP_SCHEDULE"} <= data
    web = keys("deploy/compose/web.env")
    assert {"WASHBURN_PUBLIC_HOST", "WASHBURN_ACME_EMAIL", "WASHBURN_IMAGE_WEB"} <= web
    for rel in ("deploy/compose/crawler.env", "deploy/compose/data.env", "deploy/compose/web.env"):
        assert "WASHBURN_TAILSCALE_IP" not in keys(rel), rel
    borg = (out / "deploy/backup/borg.env").read_text()
    assert "WASHBURN_PG_CONTAINER=washburn-postgres\n" in borg
    assert 'BORG_RSH="ssh -i /opt/washburn/backup/id_ed25519 -p 23 ' in borg
    assert "KEEP_DAILY=7\n" in borg  # no spaces: unquoted
    assert "WASHBURN_BACKUP_SCHEDULE" not in borg  # deploy.sh reads it from data.env
    assert "WASHBURN_BACKUP_SCHEDULE=*-*-* 03:17:00 UTC" in (out / "deploy/compose/data.env").read_text()
    assert "/./backups/washburn" in borg


def test_placeholders_rejected_without_flag(tmp_path: Path) -> None:
    config = make_config(tmp_path, substitute=False)
    out = tmp_path / "out"
    result = run_render(config, out)
    assert result.returncode == 1
    assert "postgres.password" in result.stderr
    assert "vultr.api_key" in result.stderr
    assert "vultr.ssh_key_ids[0]" in result.stderr
    assert not out.exists()


def test_missing_config_exits_2(tmp_path: Path) -> None:
    result = run_render(tmp_path / "nope.toml", tmp_path / "out")
    assert result.returncode == 2
    assert "washburn.example.toml" in result.stderr


def test_missing_field_is_named(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    text = config.read_text().replace('repo_path = "./backups/washburn"\n', "")
    config.write_text(text)
    result = run_render(config, tmp_path / "out", allow=True)
    assert result.returncode == 1
    assert "hetzner.repo_path" in result.stderr


def test_substituted_secrets_never_printed_and_files_are_0600(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    tokens = secret_tokens(config.read_text())
    assert len(tokens) == 7  # every secret field in the example (no [images] table there)
    out = tmp_path / "out"
    result = run_render(config, out)
    assert result.returncode == 0, result.stderr
    combined = result.stdout + result.stderr
    for token in tokens:
        assert token not in combined
    # Four secrets are rendered into files (tfvars, postgres, borg). The others
    # (typesafe, openrouter, admin) are read by the services from the mounted TOML.
    rendered = "".join((out / rel).read_text() for rel in SECRET_FILES)
    config_text = config.read_text()
    for field in ("vultr.api_key", "tailscale.auth_key", "postgres.password", "hetzner.borg_passphrase"):
        table, key = field.split(".")
        value = re.search(rf"^\[{table}\][\s\S]*?^{key} = \"([^\"]+)\"", config_text, flags=re.MULTILINE).group(1)
        assert value in rendered, field
    assert len(tokens) == 7
    for rel in SECRET_FILES:
        mode = stat.S_IMODE((out / rel).stat().st_mode)
        assert mode == 0o600, (rel, oct(mode))
    assert stat.S_IMODE((out / "deploy/caddy/Caddyfile").stat().st_mode) == 0o644


def test_images_table_is_used_when_present(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    config.write_text(
        config.read_text()
        .replace('registry = "ghcr.io/OWNER"', 'registry = "ghcr.io/acme"')
        .replace('tag = "latest"', 'tag = "v1"')
    )
    out = tmp_path / "out"
    result = run_render(config, out)
    assert result.returncode == 0, result.stderr
    assert "OWNER" not in result.stderr
    assert "WASHBURN_IMAGE_WEB=ghcr.io/acme/washburn-web:v1" in (out / "deploy/compose/web.env").read_text()


def test_owner_placeholder_registry_warns(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    result = run_render(config, tmp_path / "out")
    assert result.returncode == 0, result.stderr
    assert "warning: [images] registry still contains OWNER" in result.stderr
    assert "ghcr.io/OWNER/washburn-sweeper:latest" in (tmp_path / "out/deploy/compose/crawler.env").read_text()


def test_absent_images_table_falls_back_with_warning(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    text = config.read_text()
    start = text.index("[images]")
    end = text.index("\n[", start + 1)
    config.write_text(text[:start] + text[end + 1:])
    out = tmp_path / "out"
    result = run_render(config, out)
    assert result.returncode == 0, result.stderr
    assert "warning: [images] not set" in result.stderr
    assert "ghcr.io/OWNER/washburn-web:latest" in (out / "deploy/compose/web.env").read_text()
    # Empty pull credentials mean no docker login.
    assert "WASHBURN_GHCR_USER=\n" in (out / "deploy/compose/web.env").read_text()


def test_empty_pull_credentials_are_accepted(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    result = run_render(config, tmp_path / "out")
    assert result.returncode == 0, result.stderr
    env = (tmp_path / "out/deploy/compose/data.env").read_text()
    assert "WASHBURN_GHCR_USER=\n" in env and "WASHBURN_GHCR_TOKEN=\n" in env


def test_data_ip_line_in_crawler_and_web_only(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    out = tmp_path / "out"
    assert run_render(config, out).returncode == 0
    for role in ("crawler", "web"):
        lines = (out / f"deploy/compose/{role}.env").read_text().splitlines()
        idx = lines.index("WASHBURN_DATA_IP=")
        assert lines[idx - 1] == "# deploy.sh writes this from `tailscale ip -4 data` on the VM"
    assert "WASHBURN_DATA_IP" not in (out / "deploy/compose/data.env").read_text()


def test_per_role_toml_has_exact_tables_and_round_trips(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    out = tmp_path / "out"
    assert run_render(config, out).returncode == 0
    source = tomllib.loads(config.read_text(encoding="utf-8"))
    for role, tables in (("crawler", CRAWLER_TABLES), ("web", WEB_TABLES)):
        path = out / "config" / f"{role}.toml"
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        rendered = tomllib.loads(path.read_text(encoding="utf-8"))
        assert set(rendered) == tables, role
        assert rendered == {t: source[t] for t in tables}, role


def test_per_role_toml_escapes_strings_and_round_trips(tmp_path: Path) -> None:
    tricky = 'wash "quoted" \\ back\\slash'
    config = make_config(tmp_path)
    text = config.read_text(encoding="utf-8")
    text = re.sub(r'^user_agent = .*$', lambda m: 'user_agent = "' + tricky.replace("\\", "\\\\").replace('"', '\\"') + '"', text, flags=re.MULTILINE)
    config.write_text(text, encoding="utf-8")
    out = tmp_path / "out"
    assert run_render(config, out).returncode == 0
    rendered = tomllib.loads((out / "config/crawler.toml").read_text(encoding="utf-8"))
    assert rendered["crawl"]["user_agent"] == tricky


def _load_render_module() -> Any:
    spec = importlib.util.spec_from_file_location("render_config", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules["render_config"] = module
    spec.loader.exec_module(module)
    return module


def test_dump_toml_round_trips_scalars_arrays_and_nested_tables() -> None:
    module = _load_render_module()
    data = {
        "top": {
            "text": 'a"b\\c\td\x01\ne',
            "neg": -3,
            "frac": 0.7,
            "big": 1e16,
            "flag": False,
            "items": ["x\"y", "z"],
            "none": [],
            "odd key": "v",
            "nested": {"k": "v"},
        }
    }
    assert tomllib.loads(module.dump_toml(data)) == data


def test_unsafe_env_secret_is_refused_without_echoing_it(tmp_path: Path) -> None:
    bad = 'p#ss word"x'
    config = make_config(tmp_path, values={"password": bad})
    out = tmp_path / "out"
    result = run_render(config, out)
    assert result.returncode == 1
    assert "postgres.password" in result.stderr
    assert bad not in result.stdout + result.stderr
    assert not out.exists()


def test_rerender_reports_changed_keys_without_values(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    out = tmp_path / "out"
    assert run_render(config, out).returncode == 0
    first_pw = re.search(r'^password = "([^"]+)"', config.read_text(), flags=re.MULTILINE).group(1)
    new_pw = secrets.token_hex(16)
    config.write_text(config.read_text().replace(first_pw, new_pw))
    result = run_render(config, out)
    assert result.returncode == 0, result.stderr
    assert "[updated, mode 0600]" in result.stdout
    assert "~ POSTGRES_PASSWORD (value changed; not shown)" in result.stdout
    assert new_pw not in result.stdout and first_pw not in result.stdout


def test_wrong_type_is_rejected(tmp_path: Path) -> None:
    config = make_config(tmp_path)
    config.write_text(config.read_text().replace("os_id = 2284", 'os_id = "2284"'))
    result = run_render(config, tmp_path / "out", allow=True)
    assert result.returncode == 1
    assert "vultr.os_id must be a integer" in result.stderr
