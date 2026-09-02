"""Regression (2026-09-02) : un agent delegue peut terminer avec exit_code 0,
ecrire un code entierement correct, faire passer sa propre suite de tests —
et neanmoins ne jamais executer `git commit`/`git push`. Constate en
Workshop reel : `OpenPullRequest` ouvrait alors une PR au diff vide malgre
un travail par ailleurs correct et teste.

`delegate_to_opencode` n'attend donc plus cela de la seule diligence de
l'agent : il enchaine desormais un second `exec_in_workshop`, construit par
pm-engine lui-meme, qui garantit le commit ET le push quel que soit ce que
l'agent a fait ou pas fait.

Le test execute reellement la commande produite via `bash -c`, dans un vrai
depot git jetable — pas de simulation, c'est bien git/bash qui tranchent.
"""

from __future__ import annotations

import shlex
import subprocess


def _build_commit_command(title: str) -> str:
    """Reproduit la construction de `pm_engine.nodes.delegate_to_opencode`."""
    return (
        "git add -A && "
        f"(git diff --cached --quiet || git commit -m {shlex.quote(title)}) "
        "&& git push origin HEAD"
    )


def _init_repo_with_remote(tmp_path):
    remote = tmp_path / "remote.git"
    subprocess.run(["git", "init", "--bare", "-b", "main", str(remote)], check=True)

    workdir = tmp_path / "work"
    workdir.mkdir()
    subprocess.run(["git", "init", "-b", "main"], cwd=workdir, check=True)
    subprocess.run(["git", "config", "user.email", "agent@atelier.local"], cwd=workdir, check=True)
    subprocess.run(["git", "config", "user.name", "agent"], cwd=workdir, check=True)
    (workdir / "README.md").write_text("initial\n")
    subprocess.run(["git", "add", "-A"], cwd=workdir, check=True)
    subprocess.run(["git", "commit", "-m", "initial"], cwd=workdir, check=True)
    subprocess.run(["git", "remote", "add", "origin", str(remote)], cwd=workdir, check=True)
    subprocess.run(["git", "push", "-u", "origin", "main"], cwd=workdir, check=True)
    return workdir, remote


def _remote_head_matches(remote, workdir) -> bool:
    remote_head = subprocess.run(
        ["git", "rev-parse", "main"], cwd=remote, capture_output=True, text=True, check=True
    ).stdout.strip()
    local_head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=workdir, capture_output=True, text=True, check=True
    ).stdout.strip()
    return remote_head == local_head


def test_uncommitted_work_left_by_the_agent_is_committed_and_pushed(tmp_path):
    """Le cas exact du bug : l'agent a ecrit des fichiers, ne les a jamais
    committes."""
    workdir, remote = _init_repo_with_remote(tmp_path)
    (workdir / "server.js").write_text("// travail de l'agent, jamais commite\n")

    result = subprocess.run(
        _build_commit_command("feat: raccourcisseur d'URL"),
        shell=True,
        cwd=workdir,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"stderr={result.stderr}"
    assert _remote_head_matches(remote, workdir)


def test_agent_committed_but_forgot_to_push(tmp_path):
    """L'autre moitie du meme oubli : commit local present, jamais pousse.
    Sans le `push` inconditionnel, ce cas serait invisible (rien a
    committer, donc rien ne se passe)."""
    workdir, remote = _init_repo_with_remote(tmp_path)
    (workdir / "server.js").write_text("// commite localement, jamais pousse\n")
    subprocess.run(["git", "add", "-A"], cwd=workdir, check=True)
    subprocess.run(["git", "commit", "-m", "wip"], cwd=workdir, check=True)
    assert not _remote_head_matches(remote, workdir), "precondition : le push n'a pas eu lieu"

    result = subprocess.run(
        _build_commit_command("feat: raccourcisseur d'URL"),
        shell=True,
        cwd=workdir,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"stderr={result.stderr}"
    assert _remote_head_matches(remote, workdir)


def test_agent_already_committed_and_pushed_is_a_harmless_noop(tmp_path):
    """L'agent a fait exactement ce qu'on lui demandait : la commande ne
    doit rien casser, ni produire de commit vide."""
    workdir, remote = _init_repo_with_remote(tmp_path)
    (workdir / "server.js").write_text("// deja commite et pousse par l'agent\n")
    subprocess.run(["git", "add", "-A"], cwd=workdir, check=True)
    subprocess.run(["git", "commit", "-m", "feat: deja fait"], cwd=workdir, check=True)
    subprocess.run(["git", "push", "origin", "main"], cwd=workdir, check=True)
    head_before = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=workdir, capture_output=True, text=True, check=True
    ).stdout.strip()

    result = subprocess.run(
        _build_commit_command("feat: raccourcisseur d'URL"),
        shell=True,
        cwd=workdir,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"stderr={result.stderr}"
    head_after = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=workdir, capture_output=True, text=True, check=True
    ).stdout.strip()
    assert head_after == head_before, "aucun commit vide ne doit avoir ete ajoute"
    assert _remote_head_matches(remote, workdir)
