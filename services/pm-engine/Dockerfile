# syntax=docker/dockerfile:1
#
# Image de production du PM Engine (Jalon M5, tache 5.1.2). Multi-stage :
# le stage `builder` installe les dependances via `uv` dans un
# environnement virtuel isole, copie ensuite dans l'image finale minimale
# (`python:3.12-slim`, sans toolchain de compilation ni `uv`).

FROM python:3.12-slim AS builder

# `uv` accelere l'installation et resout les dependances de maniere
# deterministe (pas de dependance sur pip/setuptools dans l'image finale).
COPY --from=ghcr.io/astral-sh/uv:0.5 /uv /usr/local/bin/uv

WORKDIR /build

# Deux couches Docker separees (metadonnees du projet, puis code source) :
# le cache d'installation des dependances reste valide tant que
# pyproject.toml/README.md ne changent pas, meme si pm_engine/ change.
COPY pyproject.toml README.md ./
COPY pm_engine ./pm_engine

RUN uv venv /opt/venv --python 3.12 \
    && uv pip install --python /opt/venv/bin/python --no-cache .

FROM python:3.12-slim AS runtime

# Utilisateur non-root : aucune raison pour ce service reseau simple de
# tourner en root dans le conteneur final.
RUN useradd --create-home --uid 10001 pmengine
COPY --from=builder /opt/venv /opt/venv
WORKDIR /app
COPY pm_engine ./pm_engine

ENV PATH="/opt/venv/bin:${PATH}" \
    PYTHONUNBUFFERED=1

USER pmengine
EXPOSE 8100

# Pas de --reload en production : rechargement a chaud reserve au
# developpement local (voir README.md).
CMD ["uvicorn", "pm_engine.main:app", "--host", "0.0.0.0", "--port", "8100"]
