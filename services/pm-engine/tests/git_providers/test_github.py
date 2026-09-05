"""Verifie `GitHubProvider.get_issue` contre la vraie API publique GitHub
(depot public bien connu, issue stable dans le temps) — pas de mock.

`post_comment`/`create_branch`/`create_pr`/`merge_pr` ne sont PAS testes de
bout en bout ici : ce sont des operations d'ecriture qui necessiteraient un
jeton avec des droits sur un depot reel, indisponible dans cet
environnement (contrairement a Forgejo, ou une instance de dev complete
existe — voir tests/git_providers/test_forgejo.py, qui couvre le contrat
complet). Skip si le reseau externe n'est pas joignable, jamais de mock en
remplacement.
"""

from __future__ import annotations

import httpx
import pytest

from pm_engine.git_providers import GitHubProvider


@pytest.mark.asyncio
async def test_get_issue_reads_a_real_public_github_issue() -> None:
    provider = GitHubProvider(token="")
    try:
        issue = await provider.get_issue("octocat/Hello-World", 1)
    except httpx.HTTPError as exc:
        pytest.skip(f"api.github.com injoignable pour ce test: {exc}")
    else:
        assert issue.number == 1
        assert issue.url.startswith("https://github.com/octocat/Hello-World/")
        assert issue.author
    finally:
        await provider.aclose()


@pytest.mark.asyncio
async def test_get_file_content_reads_a_real_public_file_and_none_for_absent_ones() -> None:
    """Tache 12.3 : meme methode que `ForgejoProvider.get_file_content`,
    verifiee ici contre l'API publique reelle (aucun jeton necessaire pour
    un depot public)."""
    provider = GitHubProvider(token="")
    try:
        content = await provider.get_file_content("octocat/Hello-World", "README", "master")
    except httpx.HTTPError as exc:
        pytest.skip(f"api.github.com injoignable pour ce test: {exc}")
    else:
        assert content is not None
        assert "Hello World" in content

        absent = await provider.get_file_content(
            "octocat/Hello-World", "n-existe-pas.yaml", "master"
        )
        assert absent is None
    finally:
        await provider.aclose()


def test_git_push_credential_reuses_the_same_token_as_a_basic_auth_password() -> None:
    """Meme raisonnement que pour Forgejo : reexpose sans appel reseau le
    jeton deja fourni a la construction."""
    provider = GitHubProvider("un-jeton-de-test")
    assert provider.git_push_credential() == ("x-access-token", "un-jeton-de-test")


def test_git_push_credential_is_none_without_a_token() -> None:
    """Un jeton vide n'est PAS equivalent a l'omettre pour l'API GitHub
    elle-meme (voir le commentaire de `__init__`) : `git_push_credential`
    doit refleter cette meme absence plutot que de fournir un mot de passe
    vide, inutilisable."""
    provider = GitHubProvider("")
    assert provider.git_push_credential() is None
