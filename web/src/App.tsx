export default function App() {
  return (
    <main className="min-h-screen bg-slate-50 text-slate-900 flex items-center justify-center font-sans">
      <div className="max-w-prose px-6">
        <h1 className="text-3xl font-bold mb-2">repo-recall</h1>
        <p className="text-slate-600 mb-4">
          Static React frontend stub. Real UI lands in{" "}
          <a
            className="underline decoration-dotted hover:text-indigo-700"
            href="https://github.com/coilysiren/repo-recall/issues/144"
          >
            #144
          </a>
          .
        </p>
        <p className="text-sm text-slate-500">
          JSON surface lives on the Rust backend (default
          <code className="font-mono mx-1">127.0.0.1:7777</code>). This bundle
          consumes it through caddy in production, through the Vite dev proxy
          locally.
        </p>
      </div>
    </main>
  );
}
