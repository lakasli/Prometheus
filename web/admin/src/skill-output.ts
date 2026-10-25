import type { SkillDraft } from "./types";

export function skillCandidate(content: string): SkillDraft | null {
  if (content.length > 65536) return null;
  const block = /```json\s*\n([\s\S]*?)\n```/i.exec(content);
  if (!block) return null;
  try {
    const value = JSON.parse(block[1]!);
    const bytes = (text: string) => new TextEncoder().encode(text).length;
    if (
      typeof value.name !== "string" ||
      !value.name.trim() ||
      bytes(value.name) > 128 ||
      typeof value.description !== "string" ||
      bytes(value.description) > 1024 ||
      typeof value.instructions !== "string" ||
      !value.instructions.trim() ||
      bytes(value.instructions) > 16000 ||
      (value.kind && value.kind !== "instruction")
    )
      return null;
    return {
      name: value.name,
      description: value.description,
      instructions: value.instructions,
      kind: "instruction",
      enabled: false,
    };
  } catch {
    return null;
  }
}

export function drawingImage(content: string): string | null {
  if (content.length > 6 * 1024 * 1024) return null;
  return (
    /^!\[[^\]\r\n]*\]\((data:image\/png;base64,iVBORw0KGgo[A-Za-z0-9+/=]+)\)\s*$/.exec(
      content,
    )?.[1] || null
  );
}

// An inert template prevents resource loading while removing navigation and
// active elements. A unique-origin sandbox and a restrictive CSP add independent
// browser enforcement. Output is never inserted into the management document.
export function drawingHtml(content: string): string | null {
  if (content.length > 65536) return null;
  const code = /```html\s*\n([\s\S]*?)\n```/i.exec(content)?.[1];
  if (!code) return null;
  const template = document.createElement("template");
  template.innerHTML = code;
  const allowed = new Set(
    "html head body title style div span main section article header footer p h1 h2 h3 h4 br hr strong em b i small pre code figure figcaption ul ol li table thead tbody tr th td svg g path circle ellipse rect line polyline polygon text tspan defs lineargradient radialgradient stop clippath mask pattern use image".split(
      " ",
    ),
  );
  for (const element of [...template.content.querySelectorAll("*")]) {
    if (!allowed.has(element.localName.toLowerCase())) {
      element.remove();
      continue;
    }
    for (const attribute of [...element.attributes]) {
      const name = attribute.name.toLowerCase();
      if (
        ["href", "xlink:href"].includes(name) &&
        element.localName.toLowerCase() === "use" &&
        /^#[A-Za-z_][\w:.-]*$/.test(attribute.value)
      )
        continue;
      if (
        name.startsWith("on") ||
        [
          "href",
          "xlink:href",
          "src",
          "srcset",
          "action",
          "formaction",
          "target",
          "srcdoc",
          "is",
          "contenteditable",
        ].includes(name)
      )
        element.removeAttribute(attribute.name);
    }
  }
  const policy =
    "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${policy}"><meta name="referrer" content="no-referrer"><meta name="viewport" content="width=device-width, initial-scale=1"><style>body{margin:0;overflow-wrap:anywhere}svg{max-width:100%;height:auto}*{box-sizing:border-box}</style></head><body>${template.innerHTML}</body></html>`;
}
