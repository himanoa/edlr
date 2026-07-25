import "@testing-library/jest-dom/vitest";

// localStorage is not automatically available in this jsdom setup,
// so we provide a mock implementation for test environments
if (typeof window !== "undefined" && !window.localStorage) {
  const store: Record<string, string> = {};
  window.localStorage = {
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
    length: 0,
    key: () => null,
  } as Storage;
}
