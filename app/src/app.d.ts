declare module "*.svelte" {
  import type { Component } from "svelte";

  const component: Component;
  export default component;
}

declare module "*.svelte?raw" {
  const source: string;
  export default source;
}

declare module "*.css";
