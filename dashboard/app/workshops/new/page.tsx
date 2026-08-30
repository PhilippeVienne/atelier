import { requireAccessToken } from "@/lib/session";
import { TopNav } from "@/app/components/top-nav";
import { DEV_EGRESS_ALLOWLIST } from "@/lib/dev-allowlist";
import { NewWorkshopForm } from "./form";

// Depot public dedie (plus un sous-dossier du depot atelier principal) :
// image-builder peut le cloner sans identifiants git. La devcontainer
// (base mcr.microsoft.com + features ghcr.io + apt) a besoin de la
// allowlist "dev" complete pour construire, pas seulement de github.com.
const MINISTACK_PRESET = {
  repo: "https://github.com/PhilippeVienne/atelier-workspace.git",
  revision: "main",
  configPath: ".devcontainer/devcontainer.json",
  egressAllowlist: DEV_EGRESS_ALLOWLIST.join(", "),
};

export default async function NewWorkshopPage({
  searchParams,
}: {
  searchParams: Promise<{ preset?: string; repo?: string }>;
}) {
  // Force la redirection /login si pas de session, avant meme d'afficher le
  // formulaire (la Server Action fait la meme verification a la soumission,
  // mais autant echouer tot).
  await requireAccessToken();
  const { preset, repo } = await searchParams;
  const isMinistack = preset === "ministack";
  // `repo` : lien "Nouveau Workshop" depuis /projects (app/projects/page.tsx),
  // pre-remplit directement l'URL de clone interne du miroir Forgejo choisi
  // (allowlist egress "dev" par defaut, a restreindre si besoin).
  const defaults = repo
    ? { repo, revision: "HEAD", configPath: MINISTACK_PRESET.configPath, egressAllowlist: MINISTACK_PRESET.egressAllowlist }
    : isMinistack
      ? MINISTACK_PRESET
      : undefined;

  return (
    <>
      <TopNav />
      <main className="flex-1 max-w-lg w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <h1 className="text-2xl font-semibold tracking-tight">Nouveau Workshop</h1>
        {isMinistack && (
          <p className="text-sm text-muted bg-accent/10 border border-accent/20 rounded-lg px-3 py-2">
            Depot public (github.com/PhilippeVienne/atelier-workspace) : aucun
            identifiant git a provisionner.
          </p>
        )}
        {repo && (
          <p className="text-sm text-muted bg-accent/10 border border-accent/20 rounded-lg px-3 py-2">
            Depot pre-rempli depuis un projet miroir Forgejo.
          </p>
        )}
        <div className="rounded-xl border border-border bg-surface p-6 shadow-sm">
          <NewWorkshopForm defaults={defaults} />
        </div>
      </main>
    </>
  );
}
