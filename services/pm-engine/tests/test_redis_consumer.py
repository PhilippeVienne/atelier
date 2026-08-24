"""Verifie empiriquement, contre une vraie instance Redis, la garantie
at-least-once du consommateur de Streams (Jalon M5, tache 5.4.3) : lecture
via un groupe de consommateurs, acquittement explicite (XACK), et reprise
sur incident (XAUTOCLAIM) d'un message jamais acquitte par un consommateur
mort.

Necessite l'instance Redis de dev reelle du depot (voir
deploy/dev/redis/README.md). Skip si non disponible.
"""

from __future__ import annotations

import os
import uuid

import pytest
import redis.asyncio as redis

from pm_engine.redis_consumer import RedisStreamConsumer

REDIS_URL = os.environ.get("REDIS_URL", "redis://127.0.0.1:6379/0")


async def _skip_if_unavailable() -> redis.Redis:
    client = redis.from_url(REDIS_URL, decode_responses=True)
    try:
        await client.ping()
    except OSError as exc:
        pytest.skip(f"Redis indisponible pour ce test: {exc}")
    return client


@pytest.mark.asyncio
async def test_consumer_reads_and_acks_a_real_message() -> None:
    admin = await _skip_if_unavailable()
    stream = f"test-stream-{uuid.uuid4().hex[:8]}"
    group = "test-group"
    try:
        consumer = RedisStreamConsumer(REDIS_URL, "consumer-a", stream=stream, group=group)
        await consumer.ensure_group()

        await admin.xadd(stream, {"payload": "hello"})
        entries = await consumer.read(count=10, block_ms=1000)
        assert len(entries) == 1
        message_id, fields = entries[0]
        assert fields["payload"] == "hello"

        # Toujours dans le PEL (Pending Entries List) tant que non acquitte.
        pending = await admin.xpending(stream, group)
        assert pending["pending"] == 1

        await consumer.ack(message_id)
        pending_after = await admin.xpending(stream, group)
        assert pending_after["pending"] == 0

        await consumer.aclose()
    finally:
        await admin.delete(stream)
        await admin.aclose()


@pytest.mark.asyncio
async def test_consumer_reclaims_a_message_never_acked_by_a_dead_consumer() -> None:
    """Simule un crash : `consumer-crashed` lit un message (XREADGROUP) mais
    ne l'acquitte jamais — `consumer-b` doit pouvoir le reprendre via
    XAUTOCLAIM des que le delai d'inactivite minimal est depasse."""
    admin = await _skip_if_unavailable()
    stream = f"test-stream-{uuid.uuid4().hex[:8]}"
    group = "test-group"
    try:
        crashed = RedisStreamConsumer(REDIS_URL, "consumer-crashed", stream=stream, group=group)
        await crashed.ensure_group()

        await admin.xadd(stream, {"payload": "never-acked"})
        entries = await crashed.read(count=10, block_ms=1000)
        assert len(entries) == 1
        original_message_id = entries[0][0]
        # Le "crash" : `crashed` ne repond plus jamais, le message reste
        # dans le PEL sans jamais etre acquitte.

        rescuer = RedisStreamConsumer(REDIS_URL, "consumer-b", stream=stream, group=group)
        # min_idle_time_ms=0 : reprend immediatement pour ce test (pas
        # besoin d'attendre le delai reel de production, voir
        # DEFAULT_MIN_IDLE_TIME_MS).
        reclaimed = await rescuer.claim_stale_messages(min_idle_time_ms=0)
        assert len(reclaimed) == 1
        assert reclaimed[0][0] == original_message_id
        assert reclaimed[0][1]["payload"] == "never-acked"

        await rescuer.ack(original_message_id)
        pending_after = await admin.xpending(stream, group)
        assert pending_after["pending"] == 0

        await crashed.aclose()
        await rescuer.aclose()
    finally:
        await admin.delete(stream)
        await admin.aclose()


@pytest.mark.asyncio
async def test_run_forever_processes_messages_and_stops_on_cancellation() -> None:
    """`run_forever` : verifie de bout en bout (pas seulement les
    primitives individuelles) que le handler est appele avec le contenu du
    message et que l'acquittement suit reellement son execution."""
    admin = await _skip_if_unavailable()
    stream = f"test-stream-{uuid.uuid4().hex[:8]}"
    group = "test-group"
    try:
        await admin.xadd(stream, {"payload": "run-forever-test"})
        consumer = RedisStreamConsumer(REDIS_URL, "consumer-c", stream=stream, group=group)

        processed: list[dict[str, str]] = []

        async def handler(fields: dict[str, str]) -> None:
            processed.append(fields)

        import asyncio

        task = asyncio.create_task(consumer.run_forever(handler, poll_interval_s=0.1))
        for _ in range(50):
            if processed:
                break
            await asyncio.sleep(0.1)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

        assert processed == [{"payload": "run-forever-test"}]
        pending_after = await admin.xpending(stream, group)
        assert pending_after["pending"] == 0

        await consumer.aclose()
    finally:
        await admin.delete(stream)
        await admin.aclose()
