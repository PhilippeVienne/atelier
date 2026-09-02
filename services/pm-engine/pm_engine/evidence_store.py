"""Client S3 minimal pour les preuves du validateur QA post-merge
(docs/specs/09-qa-validation-post-merge.md, tache 5.7.1).

Le Workshop de QA ne parle JAMAIS a S3 lui-meme : l'authentification S3
(SigV4) signe chaque requete a partir de son contenu, contrairement a un
jeton Git ou une cle LLM (un simple en-tete `Authorization` statique,
injectable tel quel par `identity-proxy`) — voir la section 2 de la spec
pour la justification complete de ce choix. `pm_engine` (ce module) est
deja server-side, sur le meme reseau que `S3_ENDPOINT` : il recupere les
preuves via `exec_in_workshop` (base64, meme canal deja utilise par
`RunDevcontainerTests`/`BaseGitProvider.get_diff`), puis les televerse
lui-meme.

`aioboto3` (pas le `boto3` synchrone) : coherent avec le reste du service,
entierement async (asyncpg, httpx...). Meme instance S3/RustFS que
`crates/api-server::storage` (Rust, `aws-sdk-s3`), un bucket DEDIE
(`atelier-qa-evidence`, voir `deploy/dev/s3/README.md`) — jamais
`S3_BUCKET_SNAPSHOTS`, deja un usage distinct (des snapshots RAM
Firecracker, pas des preuves QA lisibles par un humain).
"""

from __future__ import annotations

from dataclasses import dataclass

import aioboto3


@dataclass
class S3Config:
    endpoint: str
    region: str
    access_key_id: str
    secret_access_key: str
    bucket: str
    # RustFS (dev) et la plupart des implementations S3 auto-hebergees
    # exigent le style "path" (`http://host/bucket/cle`) plutot que le
    # style "virtual-hosted" par defaut de boto3 (`http://bucket.host/cle`,
    # qui suppose un DNS wildcard que RustFS de dev n'a pas) — meme
    # necessite deja documentee cote `deploy/dev/s3/README.md`
    # (`S3_FORCE_PATH_STYLE=true`).
    force_path_style: bool = True


def s3_config_from_env() -> S3Config | None:
    """`None` si `S3_ENDPOINT` n'est pas defini — meme convention que
    `crates/api-server::storage::config_from_env` (Rust) : le stockage de
    preuves QA est une fonctionnalite optionnelle, pas une dependance dure
    du service. `QAValidation` degrade alors vers un verdict produit mais
    non accompagne de preuves televersees (voir sa docstring)."""
    import os

    endpoint = os.environ.get("S3_ENDPOINT")
    if not endpoint:
        return None
    bucket = os.environ.get("S3_BUCKET_QA_EVIDENCE")
    if not bucket:
        raise RuntimeError("S3_ENDPOINT est defini mais S3_BUCKET_QA_EVIDENCE est absent")
    region = os.environ.get("S3_REGION")
    if not region:
        raise RuntimeError("S3_ENDPOINT est defini mais S3_REGION est absent")
    access_key_id = os.environ.get("AWS_ACCESS_KEY_ID")
    if not access_key_id:
        raise RuntimeError("S3_ENDPOINT est defini mais AWS_ACCESS_KEY_ID est absent")
    secret_access_key = os.environ.get("AWS_SECRET_ACCESS_KEY")
    if not secret_access_key:
        raise RuntimeError("S3_ENDPOINT est defini mais AWS_SECRET_ACCESS_KEY est absent")
    force_path_style = os.environ.get("S3_FORCE_PATH_STYLE", "true").lower() != "false"

    return S3Config(
        endpoint=endpoint,
        region=region,
        access_key_id=access_key_id,
        secret_access_key=secret_access_key,
        bucket=bucket,
        force_path_style=force_path_style,
    )


async def upload_evidence(config: S3Config, key: str, content: bytes) -> str:
    """Televerse `content` sous `key` dans le bucket de preuves QA, renvoie
    `key` telle quelle (pas d'URL presignee dans cette premiere version —
    voir la section 8 de la spec, l'exposition Dashboard des preuves est
    explicitement hors perimetre)."""
    session = aioboto3.Session()
    async with session.client(
        "s3",
        endpoint_url=config.endpoint,
        region_name=config.region,
        aws_access_key_id=config.access_key_id,
        aws_secret_access_key=config.secret_access_key,
        config=_boto_config(config),
    ) as s3:
        await s3.put_object(Bucket=config.bucket, Key=key, Body=content)
    return key


async def read_evidence(config: S3Config, key: str) -> bytes:
    """Relit un objet deja televerse — sert essentiellement aux tests
    (verifier qu'un objet ecrit est bien relisible tel quel), pas au
    graphe LangGraph lui-meme dans cette premiere version."""
    session = aioboto3.Session()
    async with session.client(
        "s3",
        endpoint_url=config.endpoint,
        region_name=config.region,
        aws_access_key_id=config.access_key_id,
        aws_secret_access_key=config.secret_access_key,
        config=_boto_config(config),
    ) as s3:
        response = await s3.get_object(Bucket=config.bucket, Key=key)
        async with response["Body"] as stream:
            return await stream.read()


def _boto_config(config: S3Config):
    from botocore.config import Config

    return Config(s3={"addressing_style": "path" if config.force_path_style else "auto"})
