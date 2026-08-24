"""Verifie que le scaffolding FastAPI demarre reellement (pas de mock)."""

from fastapi.testclient import TestClient

from pm_engine.main import app


def test_health() -> None:
    client = TestClient(app)
    response = client.get("/health")
    assert response.status_code == 200
    assert response.json() == {"status": "ok"}
