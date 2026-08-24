"""Adaptateurs multi-forges (Jalon M5, tache 5.4.1/5.4.2) : interface
`BaseGitProvider` + implementations `ForgejoProvider`/`GitHubProvider`/
`GitLabProvider`. Voir `base.py` pour le contrat complet.
"""

from __future__ import annotations

from .base import BaseGitProvider, Issue, PullRequest
from .forgejo import ForgejoProvider
from .github import GitHubProvider
from .gitlab import GitLabProvider

__all__ = [
    "BaseGitProvider",
    "Issue",
    "PullRequest",
    "ForgejoProvider",
    "GitHubProvider",
    "GitLabProvider",
]
