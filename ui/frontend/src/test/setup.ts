import "@testing-library/jest-dom/vitest";

// Node 22+ は実験的な global localStorage を持ち(--localstorage-file 未指定では undefined)、
// vitest の jsdom 環境は既存の Node グローバルを上書きしないため、
// jsdom 実装がテストに届かない。そのためここで補う。
if (!globalThis.localStorage) {
  const store: Record<string, string> = {};
  Object.defineProperty(globalThis, "localStorage", {
    value: {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, value: string) => {
        store[key] = value;
      },
      removeItem: (key: string) => {
        delete store[key];
      },
      clear: () => {
        Object.keys(store).forEach((key) => {
          delete store[key];
        });
      },
      get length() {
        return Object.keys(store).length;
      },
      key: (index: number) => {
        const keys = Object.keys(store);
        return keys[index] ?? null;
      },
    } as Storage,
    configurable: true,
  });
}
