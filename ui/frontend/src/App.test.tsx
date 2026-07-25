import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";

// userEvent は @testing-library/user-event。devDependencies に "^14.5.2" で追加すること。
test("shows dashboard placeholder by default and switches tabs", async () => {
  render(<App />);
  expect(screen.getByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Logs" }));
  expect(screen.getByText("準備中")).toBeInTheDocument();
});
