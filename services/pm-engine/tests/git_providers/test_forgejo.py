"""Verifie empiriquement, contre une vraie instance Forgejo, l'integralite
du contrat `BaseGitProvider` (Jalon M5, tache 5.4.2).

Necessite l'instance Forgejo de dev reelle du depot (voir
deploy/dev/forgejo/README.md), exposee sur FORGEJO_URL, avec un jeton
d'acces personnel dans FORGEJO_TOKEN (`forgejo admin user
generate-access-token ... --scopes all`). Skip si non disponible (pas de
mock : un skip explicite, jamais un succes factice).
"""

from __future__ import annotations

import os
import uuid

import httpx
import pytest

from pm_engine.git_providers import ForgejoProvider

FORGEJO_URL = os.environ.get("FORGEJO_URL", "http://127.0.0.1:3000")
FORGEJO_TOKEN = os.environ.get("FORGEJO_TOKEN")
FORGEJO_OWNER = os.environ.get("FORGEJO_OWNER", "atelier_admin")


def _skip_if_unavailable() -> None:
    if not FORGEJO_TOKEN:
        pytest.skip("FORGEJO_TOKEN non defini (voir deploy/dev/forgejo/README.md), test ignore")


@pytest.fixture
async def test_repo():
    """Cree un depot Forgejo jetable (avec un commit initial, pour avoir une
    branche de base a partir de laquelle creer des branches), le supprime a
    la fin du test."""
    _skip_if_unavailable()
    repo_name = f"pm-engine-test-{uuid.uuid4().hex[:8]}"
    async with httpx.AsyncClient(
        base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
        headers={"Authorization": f"token {FORGEJO_TOKEN}"},
        timeout=30.0,
    ) as admin_client:
        try:
            response = await admin_client.post(
                "/user/repos",
                json={"name": repo_name, "auto_init": True, "default_branch": "main"},
            )
            response.raise_for_status()
            yield f"{FORGEJO_OWNER}/{repo_name}"
        finally:
            await admin_client.delete(f"/repos/{FORGEJO_OWNER}/{repo_name}")


@pytest.mark.asyncio
async def test_forgejo_provider_full_issue_to_merged_pr_lifecycle(test_repo: str) -> None:
    provider = ForgejoProvider(FORGEJO_URL, FORGEJO_TOKEN)
    try:
        # 1. Creation directe d'une issue via l'API admin (BaseGitProvider
        #    n'expose pas de `create_issue` : le PM ne cree jamais de
        #    ticket lui-meme, il repond a des tickets deja ouverts par un
        #    humain — voir docs/specs/05-devfactory-pm-engine.md).
        async with httpx.AsyncClient(
            base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
            headers={"Authorization": f"token {FORGEJO_TOKEN}"},
            timeout=30.0,
        ) as admin_client:
            created = await admin_client.post(
                f"/repos/{test_repo}/issues",
                json={"title": "Test issue", "body": "corps du ticket"},
            )
            created.raise_for_status()
            issue_number = created.json()["number"]

        # 2. get_issue
        issue = await provider.get_issue(test_repo, issue_number)
        assert issue.number == issue_number
        assert issue.title == "Test issue"
        assert issue.body == "corps du ticket"
        assert issue.author == FORGEJO_OWNER

        # 3. post_comment
        await provider.post_comment(test_repo, issue_number, "commentaire du PM")

        # 4. create_branch
        await provider.create_branch(test_repo, "feature/task-1", "main")

        # Un commit reel sur la nouvelle branche (via l'API "contents", pas
        # de clone local) : sans divergence avec `main`, Forgejo refuse
        # d'ouvrir une PR ("no changes between head and base").
        async with httpx.AsyncClient(
            base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
            headers={"Authorization": f"token {FORGEJO_TOKEN}"},
            timeout=30.0,
        ) as admin_client:
            commit = await admin_client.post(
                f"/repos/{test_repo}/contents/task.txt",
                json={
                    "content": "dGVzdA==",  # "test" en base64
                    "message": "ajoute task.txt",
                    "branch": "feature/task-1",
                },
            )
            commit.raise_for_status()

        # 5. create_pr
        pr = await provider.create_pr(
            test_repo,
            title="Task 1",
            body="resout le ticket",
            head_branch="feature/task-1",
            base_branch="main",
        )
        assert pr.head_branch == "feature/task-1"
        assert pr.base_branch == "main"
        assert pr.state == "open"

        # 6. changed_file_count : garde-fou d'`OpenPullRequest` contre les PR
        #    vides. Ici la PR porte exactement un fichier (`task.txt`).
        assert await provider.changed_file_count(test_repo, pr.number) == 1

        # 7. merge_pr
        await provider.merge_pr(test_repo, pr.number)
    finally:
        await provider.aclose()


@pytest.mark.asyncio
async def test_forgejo_provider_create_mirror() -> None:
    """`create_mirror` (Jalon M5, "Projets" — outil `setup_mirror_project`
    du chat PM) contre un vrai depot public GitHub, sans jeton : couvre la
    resolution dynamique du proprietaire (`_whoami`) et la detection du
    `service` (`_migration_auth`)."""
    _skip_if_unavailable()
    provider = ForgejoProvider(FORGEJO_URL, FORGEJO_TOKEN)
    repo_name = f"pm-engine-mirror-test-{uuid.uuid4().hex[:8]}"
    try:
        project = await provider.create_mirror(
            name=repo_name,
            source_url="https://github.com/octocat/Hello-World.git",
            private=False,
        )
        assert project.name == repo_name
        assert project.owner == FORGEJO_OWNER
        assert project.original_url == "https://github.com/octocat/Hello-World.git"
        assert project.private is False
        assert project.clone_url.endswith(f"/{FORGEJO_OWNER}/{repo_name}.git")
    finally:
        await provider.aclose()
        async with httpx.AsyncClient(
            base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
            headers={"Authorization": f"token {FORGEJO_TOKEN}"},
            timeout=30.0,
        ) as admin_client:
            await admin_client.delete(f"/repos/{FORGEJO_OWNER}/{repo_name}")


@pytest.mark.asyncio
async def test_list_root_entries_sees_what_the_planner_needs(test_repo):
    """`plan_parallel_tasks` decide de decouper ou non selon ce qu'il y a
    DEJA dans le depot : un depot vierge ne se decoupe pas, chaque agent
    devant y inventer son propre socle. Cette lecture est donc la donnee dont
    depend tout le decoupage, elle doit venir de la vraie forge."""
    _skip_if_unavailable()
    provider = ForgejoProvider(FORGEJO_URL, FORGEJO_TOKEN or "")
    try:
        entries = await provider.list_root_entries(test_repo, "main")
        # La fixture cree le depot avec un README initial : c'est exactement
        # l'etat « vierge » qui doit interdire un decoupage.
        assert entries is not None
        assert any(name.lower().startswith("readme") for name in entries)

        # Un depot inexistant renvoie `[]` (vide), jamais `None` (« je ne
        # sais pas ») : confondre les deux ferait decouper a l'aveugle.
        absent = await provider.list_root_entries(
            f"{FORGEJO_OWNER}/depot-qui-n-existe-pas-{uuid.uuid4().hex[:8]}", "main"
        )
        assert absent == []
    finally:
        await provider.aclose()


def test_git_push_credential_reuses_the_same_token_as_a_basic_auth_password() -> None:
    """Pas d'appel reseau : `git_push_credential` ne fait que reexposer le
    jeton deja fourni a la construction — c'est CE MEME jeton, deja utilise
    pour ouvrir des PR/creer des branches, qui doit permettre a l'agent
    delegue d'authentifier son propre `git push` (voir la docstring de
    `BaseGitProvider.git_push_credential`)."""
    provider = ForgejoProvider("http://forge.invalid", "un-jeton-de-test")
    assert provider.git_push_credential() == ("x-access-token", "un-jeton-de-test")
