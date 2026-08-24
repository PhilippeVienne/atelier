"""Consommateur Redis Streams a garantie at-least-once (Jalon M5, tache
5.4.3) : voir `docs/specs/05-devfactory-pm-engine.md`, section 2 ("Garantie
At-Least-Once & Resilience Redis Streams").

Les webhooks (issues/PR des adaptateurs `pm_engine.git_providers`) sont
empiles dans un Stream Redis avec un groupe de consommateurs
(`XREADGROUP`) — chaque message n'est acquitte (`XACK`) qu'une fois le
graphe LangGraph associe termine (ou avance jusqu'a son prochain
checkpoint PostgreSQL, taches 5.2.x/5.3.3). En cas de crash du worker
avant l'acquittement, `XAUTOCLAIM` reprend les messages restes "en
attente" (PEL — Pending Entries List) au-dela d'un delai d'inactivite,
pour qu'aucun webhook ne soit jamais silencieusement perdu.
"""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable

import redis.asyncio as redis

logger = logging.getLogger(__name__)

STREAM_NAME = "atelier:webhooks"
GROUP_NAME = "pm-engine-workers"

# Un message dont personne n'a accuse reception depuis plus longtemps que
# ceci est considere abandonne par son consommateur d'origine (crash) et
# repris par XAUTOCLAIM — voir docs/specs/05-devfactory-pm-engine.md.
DEFAULT_MIN_IDLE_TIME_MS = 60_000

WebhookHandler = Callable[[dict[str, str]], Awaitable[None]]


class RedisStreamConsumer:
    """Un objet par worker `atelier-pm-engine` (`consumer_name` doit etre
    unique par worker actif — ex: hostname/PID — pour que Redis les
    distingue correctement dans le groupe)."""

    def __init__(
        self,
        redis_url: str,
        consumer_name: str,
        *,
        stream: str = STREAM_NAME,
        group: str = GROUP_NAME,
    ) -> None:
        self._client: redis.Redis = redis.from_url(redis_url, decode_responses=True)
        self._consumer_name = consumer_name
        self._stream = stream
        self._group = group

    async def ensure_group(self) -> None:
        """Cree le groupe de consommateurs s'il n'existe pas deja
        (idempotent : `BUSYGROUP` est ignore, toute autre erreur Redis est
        remontee). `mkstream=True` cree aussi le Stream lui-meme s'il
        n'existe pas encore (premier demarrage sur un Redis vierge)."""
        try:
            await self._client.xgroup_create(self._stream, self._group, id="0", mkstream=True)
        except redis.ResponseError as exc:
            if "BUSYGROUP" not in str(exc):
                raise

    async def claim_stale_messages(
        self, min_idle_time_ms: int = DEFAULT_MIN_IDLE_TIME_MS
    ) -> list[tuple[str, dict[str, str]]]:
        """Reprend (XAUTOCLAIM) les messages du groupe restes non-acquittes
        depuis plus de `min_idle_time_ms`, quel que soit le consommateur
        d'origine — c'est le mecanisme de reprise sur incident."""
        _cursor, entries, _deleted = await self._client.xautoclaim(
            self._stream,
            self._group,
            self._consumer_name,
            min_idle_time_ms,
            start_id="0-0",
        )
        return entries

    async def read(
        self, *, count: int = 10, block_ms: int = 5000
    ) -> list[tuple[str, dict[str, str]]]:
        """Lit jusqu'a `count` nouveaux messages jamais distribues a aucun
        consommateur de ce groupe (`>`), en bloquant jusqu'a `block_ms` si
        le Stream est vide."""
        response = await self._client.xreadgroup(
            self._group,
            self._consumer_name,
            {self._stream: ">"},
            count=count,
            block=block_ms,
        )
        if not response:
            return []
        _stream_name, entries = response[0]
        return entries

    async def ack(self, message_id: str) -> None:
        await self._client.xack(self._stream, self._group, message_id)

    async def run_forever(
        self,
        handler: WebhookHandler,
        *,
        min_idle_time_ms: int = DEFAULT_MIN_IDLE_TIME_MS,
        poll_interval_s: float = 1.0,
    ) -> None:
        """Boucle principale du worker : reprend d'abord les messages
        abandonnes (XAUTOCLAIM), puis lit les nouveaux (XREADGROUP) — un
        message n'est acquitte qu'apres l'execution reussie de `handler`
        (une exception laisse le message dans le PEL, repris au prochain
        cycle XAUTOCLAIM par ce worker ou un autre — jamais perdu, jamais
        acquitte a tort)."""
        await self.ensure_group()
        while True:
            stale = await self.claim_stale_messages(min_idle_time_ms)
            for message_id, fields in stale:
                await self._handle_and_ack(message_id, fields, handler)

            entries = await self.read()
            for message_id, fields in entries:
                await self._handle_and_ack(message_id, fields, handler)

            if not stale and not entries:
                await asyncio.sleep(poll_interval_s)

    async def _handle_and_ack(
        self, message_id: str, fields: dict[str, str], handler: WebhookHandler
    ) -> None:
        try:
            await handler(fields)
        except Exception:
            logger.exception(
                "traitement du webhook %s echoue, message conserve dans le PEL pour reprise",
                message_id,
            )
            return
        await self.ack(message_id)

    async def aclose(self) -> None:
        await self._client.aclose()
