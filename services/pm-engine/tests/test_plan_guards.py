"""Garde-fous du decoupage en sous-taches (`plan_parallel_tasks`).

Les cas testes ici ne sont pas imagines : ils viennent des 21 plans
reellement produits par le PM et retrouves dans le checkpointer PostgreSQL
le 2026-09-01. Le symptome connu — « deux implementations de la meme
fonctionnalite » — s'y lit noir sur blanc, et c'est ce que ces garde-fous
doivent desormais empecher.
"""

from __future__ import annotations

from pm_engine.nodes import _describe_root, _is_greenfield, _plan_is_credible


class TestGreenfield:
    """Un depot sans socle ne se decoupe pas : chaque agent partirait de
    `main` dans son propre Workshop et devrait inventer le sien."""

    def test_an_empty_repository_is_greenfield(self):
        assert _is_greenfield([]) is True

    def test_readme_and_licence_alone_are_not_a_scaffolding(self):
        assert _is_greenfield(["README.md", "LICENSE", ".gitignore"]) is True

    def test_a_manifest_is_a_scaffolding(self):
        assert _is_greenfield(["README.md", "package.json"]) is False

    def test_any_source_directory_is_a_scaffolding(self):
        assert _is_greenfield(["src", "README.md"]) is False

    def test_dotfiles_alone_do_not_count(self):
        # `.github/`, `.gitignore` : de l'outillage, pas un socle sur lequel
        # un agent peut construire.
        assert _is_greenfield([".github", ".gitignore", ".editorconfig"]) is True


class TestPlanCredibility:
    """Le prompt exige des perimetres disjoints ; personne ne le verifiait."""

    def test_a_single_task_is_always_credible(self):
        # Le repli lui-meme prend `**` : le refuser bouclerait.
        assert _plan_is_credible([{"id": "task-1", "scope": ["**"]}]) is None

    def test_disjoint_scopes_are_credible(self):
        plan = [
            {"id": "task-1", "scope": ["api/**"]},
            {"id": "task-2", "scope": ["public/**"]},
        ]
        assert _plan_is_credible(plan) is None

    def test_a_catch_all_scope_beside_siblings_is_rejected(self):
        plan = [
            {"id": "task-1", "scope": ["**"]},
            {"id": "task-2", "scope": ["public/**"]},
        ]
        reason = _plan_is_credible(plan)
        assert reason is not None and "task-1" in reason

    def test_the_same_file_claimed_twice_is_rejected(self):
        # Vu en vrai : `package.json` revendique par deux sous-taches.
        plan = [
            {"id": "task-1", "scope": ["api/**", "package.json"]},
            {"id": "task-2", "scope": ["public/**", "package.json"]},
        ]
        reason = _plan_is_credible(plan)
        assert reason is not None and "package.json" in reason

    def test_a_trailing_slash_does_not_hide_a_collision(self):
        plan = [
            {"id": "task-1", "scope": ["api/"]},
            {"id": "task-2", "scope": ["api"]},
        ]
        assert _plan_is_credible(plan) is not None


class TestContractDependencies:
    """Tache 12.3 (spec docs/specs/16-escouades-multi-agents-swarms-mesh.md
    §3.1/§3.2) : `depends_on` doit toujours reference une sous-tache DEJA
    VUE plus haut dans le plan — `DelegateToOpencode` traite les sous-taches
    dans l'ordre, le contrat d'une sous-tache pas encore executee n'existe
    pas encore sur sa branche."""

    def test_a_backend_frontend_split_in_the_right_order_is_credible(self):
        plan = [
            {
                "id": "backend",
                "scope": ["api/**"],
                "service_port": 8080,
                "contract_path": "openapi.yaml",
            },
            {"id": "frontend", "scope": ["public/**"], "depends_on": "backend"},
        ]
        assert _plan_is_credible(plan) is None

    def test_depends_on_pointing_forward_is_rejected(self):
        # L'ordre est inverse : "backend" n'a pas encore ete traite quand
        # "frontend" en aurait besoin.
        plan = [
            {"id": "frontend", "scope": ["public/**"], "depends_on": "backend"},
            {"id": "backend", "scope": ["api/**"], "service_port": 8080},
        ]
        reason = _plan_is_credible(plan)
        assert reason is not None and "frontend" in reason

    def test_depends_on_an_unknown_task_is_rejected(self):
        plan = [
            {"id": "task-1", "scope": ["api/**"]},
            {"id": "task-2", "scope": ["public/**"], "depends_on": "task-inexistant"},
        ]
        reason = _plan_is_credible(plan)
        assert reason is not None and "inconnue" in reason

    def test_plans_without_any_dependency_are_unaffected(self):
        # Non-regression : la grande majorite des plans n'utilisent jamais
        # `depends_on`, le nouveau garde-fou ne doit rien y changer.
        plan = [
            {"id": "task-1", "scope": ["api/**"]},
            {"id": "task-2", "scope": ["public/**"]},
        ]
        assert _plan_is_credible(plan) is None


class TestDescribeRoot:
    """« Je ne sais pas » et « c'est vide » ne se disent pas pareil : les
    confondre ferait tout reecrire sur un depot parfaitement fourni."""

    def test_unknown_is_not_empty(self):
        assert "inconnu" in _describe_root(None)
        assert "VIDE" in _describe_root([])

    def test_entries_are_listed(self):
        assert _describe_root(["src", "package.json"]) == "package.json, src"
