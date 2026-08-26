import { Fragment, type ReactNode } from "react";

// Rendu Markdown minimal (pas de dependance ajoutee) pour les reponses du
// PM Engine : blocs de code, listes, gras/italique/code inline, liens.
// Suffisant pour du texte de LLM typique - pas un moteur CommonMark
// complet (pas de tableaux, citations, titres imbriques...).

function renderInline(text: string, keyPrefix: string): ReactNode[] {
  // Ordre important : le code inline (`...`) est extrait en premier pour
  // ne jamais interpreter ** ou [ ] a l'interieur d'un span de code.
  const tokens: ReactNode[] = [];
  const regex = /`([^`]+)`|\*\*([^*]+)\*\*|\*([^*]+)\*|\[([^\]]+)\]\(([^)]+)\)/g;
  let last = 0;
  let match: RegExpExecArray | null;
  let i = 0;
  while ((match = regex.exec(text))) {
    if (match.index > last) tokens.push(text.slice(last, match.index));
    if (match[1] !== undefined) {
      tokens.push(
        <code
          key={`${keyPrefix}-${i}`}
          className="rounded bg-background border border-border px-1.5 py-0.5 text-[0.85em] font-mono"
        >
          {match[1]}
        </code>,
      );
    } else if (match[2] !== undefined) {
      tokens.push(<strong key={`${keyPrefix}-${i}`}>{match[2]}</strong>);
    } else if (match[3] !== undefined) {
      tokens.push(<em key={`${keyPrefix}-${i}`}>{match[3]}</em>);
    } else if (match[4] !== undefined && match[5] !== undefined) {
      tokens.push(
        <a
          key={`${keyPrefix}-${i}`}
          href={match[5]}
          target="_blank"
          rel="noreferrer noopener"
          className="text-accent underline underline-offset-2 hover:no-underline"
        >
          {match[4]}
        </a>,
      );
    }
    last = regex.lastIndex;
    i += 1;
  }
  if (last < text.length) tokens.push(text.slice(last));
  return tokens;
}

interface Block {
  type: "code" | "ul" | "ol" | "p";
  content: string;
  lang?: string;
  items?: string[];
}

function parseBlocks(source: string): Block[] {
  const lines = source.split("\n");
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.startsWith("```")) {
      const lang = line.slice(3).trim();
      const codeLines: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i].startsWith("```")) {
        codeLines.push(lines[i]);
        i += 1;
      }
      i += 1; // saute la cloture ```
      blocks.push({ type: "code", content: codeLines.join("\n"), lang });
      continue;
    }
    if (/^\s*[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*]\s+/, ""));
        i += 1;
      }
      blocks.push({ type: "ul", content: "", items });
      continue;
    }
    if (/^\s*\d+[.)]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+[.)]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+[.)]\s+/, ""));
        i += 1;
      }
      blocks.push({ type: "ol", content: "", items });
      continue;
    }
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    const paraLines: string[] = [];
    while (i < lines.length && lines[i].trim() !== "" && !lines[i].startsWith("```")) {
      if (/^\s*[-*]\s+/.test(lines[i]) || /^\s*\d+[.)]\s+/.test(lines[i])) break;
      paraLines.push(lines[i]);
      i += 1;
    }
    blocks.push({ type: "p", content: paraLines.join("\n") });
  }
  return blocks;
}

export function MarkdownLite({ text }: { text: string }) {
  const blocks = parseBlocks(text);
  return (
    <div className="flex flex-col gap-3">
      {blocks.map((block, bi) => {
        const key = `b-${bi}`;
        if (block.type === "code") {
          return (
            <pre
              key={key}
              className="rounded-lg bg-background border border-border p-3 overflow-x-auto text-[0.85em] font-mono"
            >
              <code>{block.content}</code>
            </pre>
          );
        }
        if (block.type === "ul") {
          return (
            <ul key={key} className="list-disc pl-5 flex flex-col gap-1">
              {block.items?.map((item, ii) => (
                <li key={ii}>{renderInline(item, `${key}-${ii}`)}</li>
              ))}
            </ul>
          );
        }
        if (block.type === "ol") {
          return (
            <ol key={key} className="list-decimal pl-5 flex flex-col gap-1">
              {block.items?.map((item, ii) => (
                <li key={ii}>{renderInline(item, `${key}-${ii}`)}</li>
              ))}
            </ol>
          );
        }
        return (
          <p key={key} className="whitespace-pre-wrap leading-relaxed">
            {block.content.split("\n").map((l, li, arr) => (
              <Fragment key={li}>
                {renderInline(l, `${key}-${li}`)}
                {li < arr.length - 1 && <br />}
              </Fragment>
            ))}
          </p>
        );
      })}
    </div>
  );
}
