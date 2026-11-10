import fs from "node:fs"
import path from "node:path"
import { parse as parseJavaScript, type ParserPlugin } from "@babel/parser"
import traverseModule from "@babel/traverse"
import { normalizePath, type Plugin } from "vite"

// @ts-expect-error - CJS/ESM interop.
const traverse = traverseModule.default ?? traverseModule

// Every user-facing string goes through the global t("text", params). This plugin checks each
// call (static English text, params matching the {placeholders}) and imports t from the runtime.
// Chinese text is rejected: the interface is English only, and text merged from upstream must be
// translated before it ships.
const CHINESE_PATTERN = /[\u3000-\u303f\u3400-\u9fff\uff00-\uffef]/u
const PLACEHOLDER_PATTERN = /\{([A-Za-z_][A-Za-z0-9_]*)\}/g
const AUTO_IMPORT_NAME = "__staticI18nT"
const AUTO_IMPORT = `import { t as ${AUTO_IMPORT_NAME} } from "/src/i18n/runtime";\n`

const BABEL_PLUGINS = [
  "jsx",
  "typescript",
  ["classProperties", { decoratorsBeforeExport: false }],
  ["classPrivateProperties", { decoratorsBeforeExport: false }],
  "classPrivateMethods",
  "topLevelAwait",
  "importAttributes"
] as unknown as ParserPlugin[]

interface CallReplacement {
  start: number
  end: number
}

interface BabelArgument {
  type: string
  value?: string
  properties?: BabelObjectProperty[]
}

interface BabelObjectProperty {
  type: string
  computed?: boolean
  key?: { type: string; name?: string; value?: string }
}

interface BabelCallNode {
  callee: { type: string; name?: string; start?: number | null; end?: number | null }
  arguments: BabelArgument[]
  loc?: { start: { line: number; column: number } | null } | null
}

interface BabelCallPath {
  node: BabelCallNode
  scope: { getBinding(name: string): unknown }
}

function stripQuery(id: string): string {
  return id.split("?")[0]
}

function isSourceFile(id: string): boolean {
  return /\.(?:js|jsx|ts|tsx)$/.test(stripQuery(id))
}

function isExcludedFile(sourceRoot: string, id: string): boolean {
  const cleanID = normalizePath(stripQuery(id))
  if (cleanID.includes("/node_modules/")) return true
  const relativePath = normalizePath(path.relative(sourceRoot, cleanID))
  if (relativePath.startsWith("..")) return true
  return relativePath.startsWith("i18n/")
}

function placeholderNames(message: string): string[] {
  return Array.from(message.matchAll(PLACEHOLDER_PATTERN), (match) => match[1])
}

function objectParameterNames(argument: BabelArgument, location: string): string[] {
  if (argument.type !== "ObjectExpression") {
    throw new Error(`[static-i18n] ${location}: t params must be an object literal`)
  }
  const names: string[] = []
  for (const property of argument.properties ?? []) {
    if (property.type !== "ObjectProperty" || property.computed) {
      throw new Error(`[static-i18n] ${location}: t params cannot use spreads, computed keys or methods`)
    }
    const key = property.key
    const name =
      key?.type === "Identifier" ? key.name : key?.type === "StringLiteral" ? key.value : undefined
    if (!name) throw new Error(`[static-i18n] ${location}: t params keys must be static names`)
    if (names.includes(name)) throw new Error(`[static-i18n] ${location}: t params repeat ${name}`)
    names.push(name)
  }
  return names
}

function validateCall(source: string, args: BabelArgument[], location: string): void {
  if (CHINESE_PATTERN.test(source)) {
    throw new Error(`[static-i18n] ${location}: t text must be English: ${JSON.stringify(source)}`)
  }
  const placeholders = placeholderNames(source)
  const duplicate = placeholders.find((name, index) => placeholders.indexOf(name) !== index)
  if (duplicate) {
    throw new Error(`[static-i18n] ${location}: placeholder {${duplicate}} is used twice`)
  }
  if (!placeholders.length) {
    if (args.length !== 1) throw new Error(`[static-i18n] ${location}: text without placeholders takes no params`)
    return
  }
  if (args.length !== 2) {
    throw new Error(`[static-i18n] ${location}: placeholders need one params object`)
  }
  const parameters = objectParameterNames(args[1], location)
  const missing = placeholders.filter((name) => !parameters.includes(name))
  const extra = parameters.filter((name) => !placeholders.includes(name))
  if (missing.length || extra.length) {
    throw new Error(
      `[static-i18n] ${location}: params do not match the placeholders` +
      `${missing.length ? `; missing: ${missing.join(", ")}` : ""}` +
      `${extra.length ? `; extra: ${extra.join(", ")}` : ""}`
    )
  }
}

function analyzeTCalls(code: string, filePath: string, sourceRoot: string): CallReplacement[] {
  const replacements: CallReplacement[] = []
  let ast
  try {
    ast = parseJavaScript(code, { sourceType: "module", sourceFilename: filePath, plugins: [...BABEL_PLUGINS] })
  } catch (error) {
    throw new Error(`[static-i18n] Failed to parse ${filePath}: ${(error as Error).message}`)
  }
  traverse(ast, {
    CallExpression(callPath: BabelCallPath) {
      const node = callPath.node
      if (node.callee.type !== "Identifier" || node.callee.name !== "t") return
      const start = node.loc?.start
      const location = `${normalizePath(path.relative(sourceRoot, filePath))}:${start?.line ?? 1}:${(start?.column ?? 0) + 1}`
      if (callPath.scope.getBinding("t")) {
        throw new Error(`[static-i18n] ${location}: t is a reserved global; remove the local declaration or import`)
      }
      const firstArg = node.arguments[0]
      if (firstArg?.type !== "StringLiteral" || firstArg.value === undefined) {
        throw new Error(`[static-i18n] ${location}: t text must be a static string literal`)
      }
      validateCall(firstArg.value, node.arguments, location)
      const { start: calleeStart, end: calleeEnd } = node.callee
      if (calleeStart == null || calleeEnd == null) throw new Error(`[static-i18n] ${location}: cannot locate the t call`)
      replacements.push({ start: calleeStart, end: calleeEnd })
    }
  })
  return replacements
}

function walkSourceFiles(dirPath: string, visitor: (filePath: string) => void): void {
  for (const entry of fs.readdirSync(dirPath, { withFileTypes: true })) {
    const nextPath = path.join(dirPath, entry.name)
    if (entry.isDirectory()) {
      if (entry.name !== "node_modules") walkSourceFiles(nextPath, visitor)
    } else {
      visitor(nextPath)
    }
  }
}

function injectRuntimeImport(code: string, replacements: CallReplacement[]): string {
  let transformed = code
  for (const replacement of replacements.sort((a, b) => b.start - a.start)) {
    transformed = transformed.slice(0, replacement.start) + AUTO_IMPORT_NAME + transformed.slice(replacement.end)
  }
  return AUTO_IMPORT + transformed
}

export function staticI18nPlugin(): Plugin {
  let sourceRoot = path.join(process.cwd(), "src")

  return {
    name: "cursor-byok-static-i18n",
    enforce: "pre",
    configResolved(config) {
      sourceRoot = path.join(config.root, "src")
    },
    buildStart() {
      // Check every file up front, including ones the current entry does not import.
      walkSourceFiles(sourceRoot, (filePath) => {
        const cleanPath = normalizePath(filePath)
        if (!isSourceFile(cleanPath) || isExcludedFile(sourceRoot, cleanPath)) return
        analyzeTCalls(fs.readFileSync(cleanPath, "utf8"), cleanPath, sourceRoot)
      })
    },
    transform(code, id) {
      if (!isSourceFile(id) || isExcludedFile(sourceRoot, id)) return null
      const replacements = analyzeTCalls(code, normalizePath(stripQuery(id)), sourceRoot)
      if (!replacements.length) return null
      return { code: injectRuntimeImport(code, replacements), map: null }
    }
  }
}
