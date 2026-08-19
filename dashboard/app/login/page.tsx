import { getAccessToken } from "@/lib/session";
import { redirect } from "next/navigation";

export default async function LoginPage({
  searchParams,
}: {
  searchParams: Promise<{ error?: string }>;
}) {
  if (await getAccessToken()) {
    redirect("/");
  }
  const { error } = await searchParams;

  return (
    <main className="flex-1 flex flex-col items-center justify-center gap-6 p-8 text-center">
      <h1 className="text-3xl font-semibold">Atelier</h1>
      <p className="text-neutral-500 max-w-md">
        Connexion via l&apos;identite Kanidm de votre organisation.
      </p>
      {error && (
        <p className="text-sm text-red-600 max-w-md border border-red-200 bg-red-50 rounded px-4 py-2">
          {error}
        </p>
      )}
      <a
        href="/api/auth/login"
        className="rounded-full bg-foreground text-background px-6 py-2.5 font-medium hover:opacity-90 transition-opacity"
      >
        Se connecter avec Kanidm
      </a>
    </main>
  );
}
