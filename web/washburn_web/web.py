"""Shared plumbing for both ASGI apps: templates, lifespan, error pages."""

from __future__ import annotations

import logging
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

import psycopg
from fastapi import FastAPI, Request
from fastapi.responses import HTMLResponse
from jinja2 import Environment, FileSystemLoader, select_autoescape
from psycopg_pool import PoolTimeout
from starlette.exceptions import HTTPException as StarletteHTTPException

from washburn_web import db
from washburn_web.config import get_config

logger = logging.getLogger("washburn_web")
TEMPLATE_DIR = Path(__file__).parent / "templates"

_env = Environment(
    loader=FileSystemLoader(TEMPLATE_DIR),
    autoescape=select_autoescape(["html"]),
    trim_blocks=True,
    lstrip_blocks=True,
)


def render(name: str, status: int = 200, **context: Any) -> HTMLResponse:
    html = _env.get_template(name).render(**context)
    return HTMLResponse(html, status_code=status, headers={"Cache-Control": "no-store"})


def message_page(area: str, status: int, title: str, detail: str) -> HTMLResponse:
    return render("error.html", status=status, area=area, title=title, detail=detail)


def make_lifespan():
    @asynccontextmanager
    async def lifespan(app: FastAPI) -> AsyncIterator[None]:
        db.open_pool(get_config().postgres)
        try:
            yield
        finally:
            db.close_pool()

    return lifespan


def install_error_handlers(app: FastAPI, area: str) -> None:
    @app.exception_handler(StarletteHTTPException)
    async def http_error(request: Request, exc: StarletteHTTPException) -> HTMLResponse:
        response = message_page(area, exc.status_code, f"{exc.status_code}", str(exc.detail or ""))
        if exc.headers:
            response.headers.update(exc.headers)
        return response

    @app.exception_handler(psycopg.OperationalError)
    @app.exception_handler(PoolTimeout)
    async def db_unavailable(request: Request, exc: Exception) -> HTMLResponse:
        # Log the exception type only: driver messages can echo connection details.
        logger.error("database unavailable: %s", type(exc).__name__)
        return message_page(
            area, 503, "503", "The database is unavailable right now. Try again shortly."
        )
