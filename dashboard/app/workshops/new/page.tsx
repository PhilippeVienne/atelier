import { requireAccessToken } from "@/lib/session";
import { NewWorkshopForm } from "./form";

// Depot public dedie (plus un sous-dossier du depot atelier principal) :
// image-builder peut le cloner sans identifiants git.
const MINISTACK_PRESET = {
  repo: "https://github.com/PhilippeVienne/atelier-workspace.git",
  revision: "main",
  configPath: ".devcontainer/devcontainer.json",
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
        <p className="text-sm text-neutral-600 bg-neutral-50 border border-neutral-200 rounded px-3 py-2">
          Depot public (github.com/PhilippeVienne/atelier-workspace) : aucun
          identifiant git a provisionner.
        </p>
      )}
      <NewWorkshopForm defaults={defaults} />
    </main>
  );
}
