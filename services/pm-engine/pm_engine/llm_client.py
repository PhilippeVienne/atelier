"""Client LiteLLM minimal (Jalon M5, tache 5.2.2) : appels REST directs a
l'API OpenAI-compatible exposee par `deploy/dev/llm-proxy` (pas de SDK
`openai`, meme convention "REST brut" que le reste du projet — voir
`pm_engine.git_providers`). Utilise par `AnalyzeIssue`/`PlanParallelTasks`
(completions) et `IndexKnowledge` (embeddings, voir `deploy/dev/ollama` et
la tache 5.0.2 pour le modele `embedding-dev-local`).
"""

from __future__ import annotations

import json
from typing import Any, AsyncIterator

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

    async def chat_stream(
        self, model: str, messages: list[dict[str, str]], **kwargs: Any
    ) -> AsyncIterator[str]:
        """Meme endpoint que `chat()`, mais avec `stream: true` (SSE
        OpenAI-compatible) : cede chaque fragment de texte des qu'il
        arrive, pour le chat interactif du Dashboard (tache 5.5.1) — un
        vrai flux LiteLLM, pas une segmentation artificielle d'une reponse
        deja complete."""
        async with self._client.stream(
            "POST",
            "/v1/chat/completions",
            json={"model": model, "messages": messages, "stream": True, **kwargs},
        ) as response:
            response.raise_for_status()
            async for line in response.aiter_lines():
                if not line.startswith("data: "):
                    continue
                payload = line[len("data: "):]
                if payload == "[DONE]":
                    break
                chunk = json.loads(payload)
                delta = chunk["choices"][0]["delta"].get("content")
                if delta:
                    yield delta

    async def embed(self, model: str, text: str) -> list[float]:
        response = await self._client.post(
            "/v1/embeddings",
            json={"model": model, "input": text},
        )
        response.raise_for_status()
        data = response.json()
        return data["data"][0]["embedding"]
