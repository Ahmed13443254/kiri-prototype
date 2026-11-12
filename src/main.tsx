import React from "react";
import ReactDOM from "react-dom/client";
import { Shell } from "./components/Shell";

import "./tokens.css";
import "./styles.css";
import "./css/shell.css";
import "./css/transcript.css";
import "./css/primitives.css";
import "./css/workspace.css";

try {
  const stored = localStorage.getItem("kiri.theme");
  if (stored === "light" || stored === "dark" || stored === "system") {
    document.documentElement.setAttribute("data-theme", stored);
  }
} catch {
  /* storage can be unavailable; the default light theme still applies */
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Shell />
  </React.StrictMode>,
);
