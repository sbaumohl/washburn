"""Console entry points: ``washburn-public`` and ``washburn-admin``."""

from __future__ import annotations

import uvicorn

from washburn_web.config import get_config, split_bind


def _serve(target: str, bind: str) -> None:
    host, port = split_bind(bind)
    uvicorn.run(target, host=host, port=port, proxy_headers=False, server_header=False)


def run_public() -> None:
    _serve("washburn_web.public:app", get_config().web.public_bind)


def run_admin() -> None:
    _serve("washburn_web.admin:app", get_config().web.admin_bind)
