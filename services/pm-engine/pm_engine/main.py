"""Point d'entree FastAPI du PM Engine.

Scaffolding minimal (Jalon M5, tache 5.1.1) : uniquement un endpoint de
sante `/health`. La machine d'etats LangGraph (`AnalyzeIssue`,
`PlanParallelTasks`, ... voir docs/specs/05-devfactory-pm-engine.md,
section 8.2 du plan) n'est PAS implementee ici : elle depend du serveur
MCP externe du Jalon M4 (`/v1/mcp`), pas encore construit, et est donc
hors perimetre de ce lot.

Lancement local :

    uvicorn pm_engine.main:app --reload --port 8100
"""

from __future__ import annotations

from fastapi import FastAPI

app = FastAPI(
    title="Atelier PM Engine",
    description=(
        "Moteur DevFactory & Project Manager autonome d'Atelier "
        "(scaffolding, voir docs/specs/05-devfactory-pm-engine.md)."
    ),
    version="0.1.0",
)


@app.get("/health")
async def health() -> dict[str, str]:
    """Sonde de sante liveness/readiness (pas de dependance externe verifiee ici)."""
    return {"status": "ok"}
