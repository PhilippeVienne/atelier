"""Regression : le prompt passe a Claude Code doit traverser le shell du
Workshop INTACT, sans qu'aucun de ses fragments ne soit interprete.

Bug reel (2026-08-30) : `delegate_to_claude_code` construisait sa commande
avec `json.dumps(prompt)`, qui produit une chaine entre guillemets DOUBLES.
Bash y interprete encore les backticks, `$(...)` et `$VAR` — or ce prompt
contient du texte genere par un LLM a partir du ticket, backticks compris.
Consequences observees : des fragments du prompt executes comme des
commandes dans la microVM (`api/: No such file or directory`, `fatal: not a
git repository`) et un prompt tronque cote Claude Code. C'est aussi une
injection de commande : le corps d'un ticket est une entree non fiable.

Le test execute reellement la commande produite via `bash -c`, avec un faux
`claude` sur le PATH qui se contente de recopier l'argument recu — pas de
simulation du shell, c'est bien bash qui tranche.
"""

from __future__ import annotations

import shlex
import subprocess


def _build_command(prompt: str) -> str:
    """Reproduit la construction de `pm_engine.nodes.delegate_to_claude_code`."""
    return f"claude --print --permission-mode acceptEdits {shlex.quote(prompt)}"


def _run_through_bash(command: str, tmp_path) -> str:
    """Execute `command` avec un faux `claude` qui ecrit son dernier argument
    dans un fichier, et renvoie ce qui lui est reellement parvenu."""
    received = tmp_path / "received.txt"
    fake_claude = tmp_path / "claude"
    fake_claude.write_text(
        "#!/usr/bin/env bash\nprintf '%s' \"${!#}\" > " + shlex.quote(str(received)) + "\n"
    )
    fake_claude.chmod(0o755)

    result = subprocess.run(
        command,
        shell=True,
        cwd=tmp_path,
        env={"PATH": f"{tmp_path}:/usr/bin:/bin"},
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"stderr={result.stderr}"
    # Aucune sortie parasite : un fragment interprete par bash se serait
    # signale ici (ex: "api/: No such file or directory").
    assert result.stderr == "", f"le shell a interprete une partie du prompt: {result.stderr}"
    return received.read_text()


def test_prompt_with_backticks_reaches_claude_code_intact(tmp_path):
    """Cas exact du bug : backticks (substitution de commande en guillemets
    doubles) et retours a la ligne, tels qu'en produisent l'analyse LLM et la
    consigne de commit."""
    prompt = (
        "Implementer l'API REST\n\n"
        "Modifie uniquement `api/` et `web/`.\n\n"
        'Puis : `git add -A && git commit -m "feat: api" && git push origin HEAD`.'
    )
    assert _run_through_bash(_build_command(prompt), tmp_path) == prompt


def test_prompt_with_shell_expansions_is_not_expanded(tmp_path):
    """Injection de commande : rien ne doit etre substitue ni execute."""
    prompt = "scope: $(id -u) et ${HOME} et `whoami`"
    assert _run_through_bash(_build_command(prompt), tmp_path) == prompt
