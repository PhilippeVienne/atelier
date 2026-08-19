import { requireAccessToken } from "@/lib/session";
import { NewWorkshopForm } from "./form";

export default async function NewWorkshopPage() {
  // Force la redirection /login si pas de session, avant meme d'afficher le
  // formulaire (la Server Action fait la meme verification a la soumission,
  // mais autant echouer tot).
  await requireAccessToken();

  return (
    <main className="flex-1 max-w-lg w-full mx-auto p-8 flex flex-col gap-6">
      <h1 className="text-2xl font-semibold">Nouveau Workshop</h1>
      <NewWorkshopForm />
    </main>
  );
}
