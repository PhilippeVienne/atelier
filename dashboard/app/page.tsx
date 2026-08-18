export default function Home() {
  return (
    <main className="flex-1 flex flex-col items-center justify-center gap-4 p-8 text-center">
      <h1 className="text-3xl font-semibold">Atelier</h1>
      <p className="text-neutral-500 max-w-md">
        Dashboard d&apos;administration et d&apos;utilisation des environnements
        Atelier (Workshops). CRUD, connexion SSH/VS Code et supervision a
        implementer sur l&apos;API definie dans <code>crates/api-server</code>.
      </p>
    </main>
  );
}
