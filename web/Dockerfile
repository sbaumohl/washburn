# Washburn web: one image, two apps. Select with WASHBURN_APP=public|admin.
# Build context is this directory (web/). Config is mounted at /etc/washburn/washburn.toml.
FROM python:3.13-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON_DOWNLOADS=never \
    UV_PROJECT_ENVIRONMENT=/app/.venv \
    WASHBURN_CONFIG=/etc/washburn/washburn.toml \
    WASHBURN_APP=public

COPY --from=ghcr.io/astral-sh/uv:0.12.5 /uv /uvx /usr/local/bin/

RUN groupadd --system --gid 10001 washburn \
 && useradd --system --uid 10001 --gid washburn --home-dir /app --shell /usr/sbin/nologin washburn

WORKDIR /app

# Dependencies first, so source edits do not reinstall them.
COPY pyproject.toml uv.lock .python-version ./
RUN uv sync --locked --no-dev --no-install-project

COPY washburn_web ./washburn_web
RUN uv sync --locked --no-dev

ENV PATH="/app/.venv/bin:$PATH"

USER washburn

EXPOSE 8000 8001

CMD ["sh", "-c", "case \"$WASHBURN_APP\" in public) exec washburn-public ;; admin) exec washburn-admin ;; *) echo \"WASHBURN_APP must be public or admin\" >&2; exit 1 ;; esac"]
