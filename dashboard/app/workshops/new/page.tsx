import { requireAccessToken } from "@/lib/session";
import { NewWorkshopForm } from "./form";

// Depot HTTPS (pas l'URL SSH du remote local) : l'auth par identifiants git
// cote image-builder (secret OpenBao workshops/<name>/git) se fait par
// utilisateur/mot de passe, pas par cle SSH.
const MINISTACK_PRESET = {
  repo: "https://github.com/PhilippeVienne/atelier.git",
  revision: "main",
  configPath: "demo/ministack-workshop/.devcontainer/devcontainer.json",
};

export default async function NewWorkshopPage({
  searchParams,
}: {
  searchParams: Promise<{ preset?: string }>;
}) {
  // Force la redirection /login si pas de session, avant meme d'afficher le
  // formulaire (la Server Action fait la meme verification a la soumission,
  // mais autant echouer tot).
  await requireAccessToken();
  const { preset } = await searchParams;
  const defaults = preset === "ministack" ? MINISTACK_PRESET : undefined;

  return (
    <main className="flex-1 max-w-lg w-full mx-auto p-8 flex flex-col gap-6">
      <h1 className="text-2xl font-semibold">Nouveau Workshop</h1>
      {defaults && (
        <p className="text-sm text-amber-800 bg-amber-50 border border-amber-200 rounded px-3 py-2">
          Depot prive : le secret OpenBao <code>workshops/&lt;nom&gt;/git</code> (champs{" "}
          <code>username</code>/<code>password</code>) doit etre provisionne manuellement avant la
          creation, sans quoi le clone du depot echouera. Voir{" "}
          <code>demo/ministack-workshop/README.md</code>.
        </p>
      )}
      <NewWorkshopForm defaults={defaults} />
    </main>
  );
}
