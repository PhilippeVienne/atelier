"""Verifie `GitLabProvider.get_issue` contre la vraie API publique GitLab
(depot public bien connu, issue stable dans le temps) — pas de mock.

Memes limites que `test_github.py` : les operations d'ecriture necessitent
un jeton avec des droits sur un projet reel, indisponible dans cet
environnement. Skip si le reseau externe n'est pas joignable.
"""

from __future__ import annotations

import httpx
import pytest

from pm_engine.git_providers import GitLabProvider


@pytest.mark.asyncio
async def test_get_issue_reads_a_real_public_gitlab_issue() -> None:
    provider = GitLabProvider(token="")
    try:
        issue = await provider.get_issue("gitlab-org/gitlab-runner", 1)
    except httpx.HTTPError as exc:
        pytest.skip(f"gitlab.com injoignable pour ce test: {exc}")
    else:
        assert issue.number == 1
        # GitLab a recemment renomme la route web des issues en
        # `/-/work_items/<iid>` ("Work Items") : on verifie le projet et le
        # numero, pas le segment de chemin exact (susceptible de rechanger).
        assert issue.url.startswith("https://gitlab.com/gitlab-org/gitlab-runner/-/")
        assert issue.url.endswith("/1")
        assert issue.author
    finally:
        await provider.aclose()


@pytest.mark.asyncio
async def test_get_file_content_reads_a_real_public_file_and_none_for_absent_ones() -> None:
    """Tache 12.3 : API "Repository files" de GitLab, distincte de l'API
    "Contents" de Forgejo/GitHub — verifiee ici contre un vrai projet
    public."""
    provider = GitLabProvider(token="")
    try:
        content = await provider.get_file_content(
            "gitlab-org/gitlab-runner", "README.md", "main"
        )
    except httpx.HTTPError as exc:
        pytest.skip(f"gitlab.com injoignable pour ce test: {exc}")
    else:
        assert content is not None
        assert "GitLab Runner" in content

        absent = await provider.get_file_content(
            "gitlab-org/gitlab-runner", "n-existe-pas.yaml", "main"
        )
        assert absent is None
    finally:
        await provider.aclose()


def test_git_push_credential_uses_the_oauth2_username_convention() -> None:
    """GitLab, contrairement a Forgejo/GitHub, exige `oauth2` exactement
    comme nom d'utilisateur HTTP Basic pour un jeton de projet/personnel."""
    provider = GitLabProvider("un-jeton-de-test")
    assert provider.git_push_credential() == ("oauth2", "un-jeton-de-test")


def test_git_push_credential_is_none_without_a_token() -> None:
    provider = GitLabProvider("")
    assert provider.git_push_credential() is None
