// Serveur Next.js custom : necessaire uniquement pour relayer le WebSocket
// "live" propre de `code-server` (`/workshops/{name}/vscode/...`) — un Route
// Handler standard (`app/workshops/[name]/vscode/[[...path]]/route.ts`, qui
// gere deja tous les assets HTTP normaux) ne peut pas hijacker une connexion
// pour un upgrade WebSocket. Voir `node_modules/next/dist/docs/01-app/02-guides/custom-server.md`.
//
// Le cookie de session (`atelier_session`, JWT Kanidm en clair — voir
// `lib/session.ts`) est lu ici directement dans les en-tetes de la requete
// d'upgrade : aucune API Next (`cookies()`) n'est disponible hors du
// contexte d'une requete geree par le framework.
import { createServer } from "node:http";
import next from "next";
import { WebSocket, WebSocketServer } from "ws";
import { API_SERVER_URL } from "./lib/config";

const port = parseInt(process.env.PORT ?? "3000", 10);
const dev = process.env.NODE_ENV !== "production";
const app = next({ dev });
const handle = app.getRequestHandler();

const SESSION_COOKIE = "atelier_session";
const VSCODE_UPGRADE_PATH = /^\/workshops\/([^/]+)\/vscode(\/.*)?$/;
const API_SERVER_WS_URL = API_SERVER_URL.replace(/^http/, "ws");

function readCookie(header: string | undefined, name: string): string | undefined {
  if (!header) return undefined;
  for (const part of header.split(";")) {
    const separator = part.indexOf("=");
    if (separator === -1) continue;
    if (part.slice(0, separator).trim() === name) {
      return decodeURIComponent(part.slice(separator + 1).trim());
    }
  }
  return undefined;
}

app.prepare().then(() => {
  const server = createServer((req, res) => handle(req, res));
  const wss = new WebSocketServer({ noServer: true });

  server.on("upgrade", (req, socket, head) => {
    const match = VSCODE_UPGRADE_PATH.exec(req.url ?? "");
    const token = readCookie(req.headers.cookie, SESSION_COOKIE);
    if (!match || !token) {
      socket.destroy();
      return;
    }
    const [, name, rest] = match;
    const targetUrl = `${API_SERVER_WS_URL}/v1/workshops/${name}/vscode${rest ?? "/"}`;

    // Ouvre d'abord la connexion amont (vers api-server) et attend sa
    // confirmation *avant* de repondre `101` au navigateur — `wss.handleUpgrade`
    // envoie ce `101` inconditionnellement des l'appel, sans savoir si
    // l'amont a reellement accepte l'upgrade. Bug constate en testant
    // reellement : un navigateur recevait un `101` "reussi" meme quand la
    // connexion vers api-server echouait ensuite (le canal restait mort en
    // silence). Un client WebSocket standard n'envoie de toute facon aucune
    // trame avant d'avoir recu son propre `101`, donc rien n'est perdu a
    // attendre ici.
    const upstream = new WebSocket(targetUrl, {
      headers: { Authorization: `Bearer ${token}` },
    });

    upstream.on("open", () => {
      wss.handleUpgrade(req, socket, head, (browserSocket) => {
        browserSocket.on("message", (data, isBinary) => {
          upstream.send(data, { binary: isBinary });
        });
        upstream.on("message", (data, isBinary) => {
          browserSocket.send(data, { binary: isBinary });
        });

        const closeBoth = () => {
          if (browserSocket.readyState === WebSocket.OPEN) browserSocket.close();
          if (upstream.readyState === WebSocket.OPEN) upstream.close();
        };
        browserSocket.on("close", closeBoth);
        browserSocket.on("error", closeBoth);
        upstream.on("close", closeBoth);
        upstream.on("error", closeBoth);
      });
    });

    // L'amont n'a jamais atteint `open` (refus HTTP non-101, ou erreur
    // reseau) : le navigateur n'a encore rien recu, une vraie reponse
    // d'erreur est donc encore possible (pas de faux "101").
    upstream.on("unexpected-response", (_req, res) => {
      // `end()`, pas `write()` + `destroy()` : `destroy()` immediatement
      // apres `write()` peut fermer la socket avant que l'ecriture ne soit
      // effectivement envoyee (constate en pratique : le navigateur ne
      // recevait rien du tout). `end()` ecrit puis ferme proprement une
      // fois le buffer vide.
      socket.end(`HTTP/1.1 ${res.statusCode} ${res.statusMessage || "Bad Gateway"}\r\n\r\n`);
    });
    upstream.on("error", (err) => {
      console.error("connexion amont vscode (websocket) echouee:", err);
      if (!socket.destroyed) {
        socket.end("HTTP/1.1 502 Bad Gateway\r\n\r\n");
      }
    });
  });

  server.listen(port, () => {
    console.log(`> dashboard en ecoute sur http://localhost:${port} (${dev ? "dev" : "production"})`);
  });
});
