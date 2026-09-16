// Generate the bundled skill's direct-call API from the packaged native SDK.
// This runs while packaging, never as a discovery step in an agent task.
import { $ } from "bun";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

type Schema =
  | boolean
  | {
      type?: string | string[];
      properties?: Record<string, Schema>;
      required?: string[];
      items?: Schema | Schema[];
      additionalProperties?: Schema;
      enum?: unknown[];
      const?: unknown;
      oneOf?: Schema[];
      anyOf?: Schema[];
      allOf?: Schema[];
      $ref?: string;
      [key: string]: unknown;
    };

export type CuaCatalog = {
  tools: Array<{ name: string; inputSchema: Schema }>;
};

const begin = "<!-- BEGIN NATIVE API -->";
const end = "<!-- END NATIVE API -->";

function propertyName(name: string): string {
  return /^[A-Za-z_$][\w$]*$/.test(name) ? name : JSON.stringify(name);
}

function typeScript(
  schema: Schema,
  root: Schema = schema,
  refs = new Set<string>(),
): string {
  if (schema === false) return "never";
  if (schema === true) return "unknown";
  if (schema.$ref) {
    if (refs.has(schema.$ref)) return "unknown";
    let target: unknown = root;
    if (!schema.$ref.startsWith("#/"))
      throw new Error(`Unsupported Cua schema reference: ${schema.$ref}`);
    for (const part of schema.$ref.slice(2).split("/")) {
      target = (target as Record<string, unknown>)?.[
        part.replaceAll("~1", "/").replaceAll("~0", "~")
      ];
    }
    if (target === undefined)
      throw new Error(`Missing Cua schema reference: ${schema.$ref}`);
    return typeScript(target as Schema, root, new Set([...refs, schema.$ref]));
  }
  if ("const" in schema) return JSON.stringify(schema.const);
  if (schema.enum)
    return (
      schema.enum.map((value) => JSON.stringify(value)).join(" | ") || "never"
    );
  if (Array.isArray(schema.type)) {
    return schema.type
      .map((type) => typeScript({ ...schema, type }, root, refs))
      .join(" | ");
  }
  const alternatives = schema.oneOf ?? schema.anyOf;
  if (alternatives) {
    // Constraints such as required/not without a value shape remain native
    // validation rules; don't turn those into invented argument types.
    const shaped = alternatives.filter(
      (option) =>
        typeof option === "boolean" ||
        option.type ||
        option.properties ||
        option.$ref ||
        option.enum ||
        "const" in option,
    );
    if (shaped.length) {
      const union = shaped
        .map((option) => typeScript(option, root, refs))
        .join(" | ");
      if (!schema.properties) return union;
      return `(${typeScript({ ...schema, oneOf: undefined, anyOf: undefined }, root, refs)}) & (${union})`;
    }
  }
  if (schema.allOf)
    return schema.allOf
      .map((option) => `(${typeScript(option, root, refs)})`)
      .join(" & ");
  switch (schema.type) {
    case "integer":
    case "number":
      return "number";
    case "string":
      return "string";
    case "boolean":
      return "boolean";
    case "null":
      return "null";
    case "array":
      return Array.isArray(schema.items)
        ? `[${schema.items.map((item) => typeScript(item, root, refs)).join(", ")}]`
        : `Array<${typeScript(schema.items ?? true, root, refs)}>`;
  }
  if (schema.type === "object" || schema.properties) {
    const required = new Set(schema.required ?? []);
    const properties = Object.entries(schema.properties ?? {}).map(
      ([key, value]) =>
        `${propertyName(key)}${required.has(key) ? "" : "?"}: ${typeScript(value, root, refs)}`,
    );
    if (schema.additionalProperties !== false) {
      properties.push(
        `[key: string]: ${typeScript(schema.additionalProperties ?? true, root, refs)}`,
      );
    }
    return properties.length
      ? `{ ${properties.join("; ")} }`
      : "Record<string, never>";
  }
  return "unknown";
}

export function renderCuaApi(
  catalog: CuaCatalog,
  platform = process.platform,
): string {
  const lines = [
    "## API surface",
    "",
    "Call these methods directly. Arguments use the native names shown below; conditional constraints are validated by the runtime.",
    "",
    "```ts",
    "type CuaResult = {",
    "  content: Array<{ type: 'text'; text: string } | { type: 'image'; data: string; mimeType: string }>;",
    "  structuredContent?: any;",
    "  isError?: boolean;",
    "};",
    "",
    "declare const cua: {",
    `  readonly platform: ${JSON.stringify(platform === "darwin" ? "macos" : platform === "win32" ? "windows" : platform)};`,
  ];
  for (const tool of catalog.tools) {
    if (tool.name === "bring_to_front") continue;
    const schema = tool.inputSchema;
    const required =
      typeof schema === "object" && (schema.required?.length ?? 0) > 0;
    lines.push(
      `  ${propertyName(tool.name)}(args${required ? "" : "?"}: ${typeScript(schema)}): Promise<CuaResult>;`,
    );
  }
  lines.push("};", "```", "");
  return lines.join("\n");
}

export function renderCuaSkill(
  source: string,
  catalog: CuaCatalog,
  platform = process.platform,
): string {
  const start = source.indexOf(begin);
  const finish = source.indexOf(end, start);
  if (start < 0 || finish < 0)
    throw new Error("Computer Use skill is missing its native API section");
  return `${source.slice(0, start)}${begin}\n${renderCuaApi(catalog, platform)}${source.slice(finish)}`;
}

export async function writeCuaSkill(
  helper: string,
  destination: string,
): Promise<void> {
  const catalog = JSON.parse(
    await $`${helper} list-tools`.quiet().text(),
  ) as CuaCatalog;
  const source = await readFile(
    new URL("../resources/computer-use/SKILL.md", import.meta.url),
    "utf8",
  );
  await mkdir(dirname(destination), { recursive: true });
  await writeFile(destination, renderCuaSkill(source, catalog));
}

if (import.meta.main) {
  const [helper, destination] = process.argv.slice(2);
  if (!helper || !destination)
    throw new Error(
      "Usage: bun scripts/cua-api.ts <native-helper> <skill-path>",
    );
  await writeCuaSkill(helper, destination);
}
