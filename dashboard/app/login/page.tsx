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
      <div className="flex flex-col items-center gap-6 rounded-2xl border border-border bg-surface p-10 shadow-lg max-w-sm w-full">
        <span className="inline-flex h-12 w-12 items-center justify-center rounded-xl bg-accent text-accent-foreground text-xl font-bold">
          A
        </span>
        <div className="flex flex-col gap-1.5">
          <h1 className="text-2xl font-semibold tracking-tight">Atelier</h1>
          <p className="text-muted text-sm">
            Connexion via l&apos;identite Kanidm de votre organisation.
          </p>
        </div>
        {error && (
          <p className="text-sm text-red-600 dark:text-red-400 w-full border border-red-500/30 bg-red-500/10 rounded-lg px-4 py-2">
            {error}
          </p>
        )}
        <a
          href="/api/auth/login"
          className="w-full rounded-full bg-accent text-accent-foreground px-6 py-2.5 font-medium hover:bg-accent-hover transition-colors"
        >
          Se connecter avec Kanidm
        </a>
      </div>
    </main>
  );
}
