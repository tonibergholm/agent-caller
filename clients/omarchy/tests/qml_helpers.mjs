// Test helpers for Panel.qml: a small structural QML parser, and a way to
// EXECUTE Panel.qml's JavaScript functions against stubs. Not a test file.

function endOfString(s, i) {
  const q = s[i];
  let j = i + 1;
  while (j < s.length && s[j] !== q) {
    if (s[j] === "\\") j++;
    j++;
  }
  return Math.min(j + 1, s.length);
}

/** Comments become spaces (same length and lines); string literals are kept. */
export function stripComments(src) {
  let out = "";
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    const n = src[i + 1];
    if (c === '"' || c === "'" || c === "`") {
      const j = endOfString(src, i);
      out += src.slice(i, j);
      i = j;
    } else if (c === "/" && n === "/") {
      while (i < src.length && src[i] !== "\n") {
        out += " ";
        i++;
      }
    } else if (c === "/" && n === "*") {
      const j = src.indexOf("*/", i + 2);
      const e = j < 0 ? src.length : j + 2;
      out += src.slice(i, e).replace(/[^\n]/g, " ");
      i = e;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

// s[i] === "{": the index just after its matching "}".
function skipBlock(s, i) {
  let d = 0;
  for (; i < s.length; i++) {
    const c = s[i];
    if (c === '"' || c === "'" || c === "`") {
      i = endOfString(s, i) - 1;
    } else if (c === "{") {
      d++;
    } else if (c === "}") {
      d--;
      if (d === 0) return i + 1;
    }
  }
  return s.length;
}

function parseBody(s, i, node, top) {
  while (i < s.length) {
    const c = s[i];
    if (c === "}" && !top) return i + 1;
    if (c === '"' || c === "'" || c === "`") {
      const j = endOfString(s, i);
      node.props += s.slice(i, j);
      i = j;
      continue;
    }
    if (c === "{") {
      const j = skipBlock(s, i);
      node.props += s.slice(i, j);
      i = j;
      continue;
    }
    if (/[A-Za-z]/.test(c) && (i === 0 || !/[\w.]/.test(s[i - 1]))) {
      const rest = s.slice(i, i + 160);
      const comp = /^component\s+(\w+)\s*:\s*([A-Z][\w.]*)\s*\{/.exec(rest);
      const plain = comp ? null : /^([A-Z][\w.]*)\s*\{/.exec(rest);
      if (comp || plain) {
        const child = { type: comp ? comp[2] : plain[1], name: comp ? comp[1] : null, props: "", children: [], parent: node };
        node.children.push(child);
        i = parseBody(s, i + (comp ?? plain)[0].length, child, false);
        continue;
      }
    }
    node.props += c;
    i++;
  }
  return i;
}

/** The QML object tree: {type, name (inline component), props (own text, children cut out), children, parent}. */
export function parseQml(src) {
  const root = { type: "file", name: null, props: "", children: [], parent: null };
  parseBody(stripComments(src), 0, root, true);
  return root;
}

export const idOf = (n) => /(?:^|[\n;{])\s*id\s*:\s*(\w+)/.exec(n.props)?.[1] ?? null;

export function findAll(node, pred, out = []) {
  if (pred(node)) out.push(node);
  for (const c of node.children) findAll(c, pred, out);
  return out;
}

/**
 * Every Item that can cut or resize `label` on its way to the Flickable
 * `flickId`: its lexical parents, and, across an inline component boundary,
 * every place the component is instantiated. `reached` is false when some
 * path does not end in that Flickable.
 */
export function ancestorsToFlick(tree, label, flickId) {
  const chain = [];
  let reached = true;
  let anyPath = false;
  const walk = (start) => {
    for (let cur = start; cur && cur.type !== "file"; cur = cur.parent) {
      if (idOf(cur) === flickId) {
        anyPath = true;
        return;
      }
      chain.push(cur);
      if (cur.name) {
        const uses = findAll(tree, (n) => n.type === cur.name);
        if (uses.length === 0) reached = false;
        for (const u of uses) walk(u);
        return;
      }
    }
    reached = false;
  };
  walk(label.parent);
  return { chain, reached: reached && anyPath };
}

/** Source of `function name(...) {...}` (comments stripped), or null. */
export function extractFn(src, name) {
  const s = stripComments(src);
  const at = s.search(new RegExp(`\\bfunction ${name}\\(`));
  if (at < 0) return null;
  const open = s.indexOf("{", s.indexOf(")", at));
  return s.slice(at, skipBlock(s, open));
}

/**
 * Compiles Panel.qml's functions `names` and stores them on `ctx`. Inside
 * them every free name resolves against `ctx` (like QML's component scope),
 * so assignments to `keyState` and calls to `cardFits` hit the stubs.
 */
export function compileFns(src, names, ctx) {
  for (const n of names) {
    const code = extractFn(src, n);
    if (code === null) throw new Error(`function ${n} not found`);
    ctx[n] = new Function("ctx", `with (ctx) { return (${code}); }`)(ctx);
  }
  return ctx;
}
