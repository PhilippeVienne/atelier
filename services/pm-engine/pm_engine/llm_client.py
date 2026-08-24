"""Client LiteLLM minimal (Jalon M5, tache 5.2.2) : appels REST directs a
l'API OpenAI-compatible exposee par `deploy/dev/llm-proxy` (pas de SDK
`openai`, meme convention "REST brut" que le reste du projet — voir
`pm_engine.git_providers`). Utilise par `AnalyzeIssue`/`PlanParallelTasks`
(completions) et `IndexKnowledge` (embeddings, voir `deploy/dev/ollama` et
la tache 5.0.2 pour le modele `embedding-dev-local`).
"""

from __future__ import annotations

from typing import Any

import httpx


class LlmClient:
    def __init__(self, base_url: str, api_key: str) -> None:
        self._client = httpx.AsyncClient(
            base_url=base_url.rstrip("/"),
            headers={"Authorization": f"Bearer {api_key}"},
            timeout=120.0,
        )

    async def aclose(self) -> None:
        await self._client.aclose()

    async def chat(self, model: str, messages: list[dict[str, str]], **kwargs: Any) -> str:
        """Renvoie le contenu texte du premier choix — suffisant pour les
        noeuds de ce graphe (pas de function calling/streaming, voir
        `AnalyzeIssue`/`PlanParallelTasks`)."""
        response = await self._client.post(
            "/v1/chat/completions",
            json={"model": model, "messages": messages, **kwargs},
        )
        response.raise_for_status()
        data = response.json()
        return data["choices"][0]["message"]["content"]

    async def embed(self, model: str, text: str) -> list[float]:
        response = await self._client.post(
            "/v1/embeddings",
            json={"model": model, "input": text},
        )
        response.raise_for_status()
        data = response.json()
        return data["data"][0]["embedding"]
