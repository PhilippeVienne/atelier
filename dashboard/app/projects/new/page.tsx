import { requireAccessToken } from "@/lib/session";
import { TopNav } from "@/app/components/top-nav";
import { NewProjectForm } from "./form";

export default async function NewProjectPage() {
  await requireAccessToken();

  return (
    <>
      <TopNav />
      <main className="flex-1 max-w-lg w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <h1 className="text-2xl font-semibold tracking-tight">Importer un projet</h1>
        <div className="rounded-xl border border-border bg-surface p-6 shadow-sm">
          <NewProjectForm />
        </div>
      </main>
    </>
  );
}
