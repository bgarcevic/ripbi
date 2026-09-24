import { invoke, isTauri } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./style.css";

type Finding = {
  type: string;
  id: string;
  table: string | null;
  used_by: { id: string; provenance: string; also_unused: boolean }[];
  named_in_power_query: string[];
};
type Result = {
  target: string;
  reports: string[];
  summary: {
    objects: number;
    reachable: number;
    roots: number;
    unused_total: number;
    ignored: number;
    broken: number;
    broken_artifacts: number;
  };
  unused: Finding[];
  broken: { target: string; reason: string; provenance: string }[];
  broken_artifacts: {
    id: string;
    type: string;
    unresolved_references: string[];
  }[];
  auto_date_time: {
    id: string;
    verdict: string;
    source_column: string | null;
    finding: Finding | null;
  }[];
  skips: { count: number; notices: { path: string; detail: string }[] };
};
type Analysis = { result: Result; diagnostics: string };
const app = document.querySelector<HTMLDivElement>("#app")!;
let model = "";
let reports: string[] = [];
let step = 1;
let renderedStep = 1;
let busy = false;
let error = "";
let saved = false;
let analysis: Analysis | undefined;
let query = "";
let kind = "all";
let tab = "unused";
const escape = (value: string) =>
  value.replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ]!,
  );
const name = (path: string) =>
  path
    .replace(/[\\/]$/, "")
    .split(/[\\/]/)
    .pop() || path;
const number = (value: number) => value.toLocaleString();
const label = (value: string) => value.replaceAll("_", " ");
const icons: Record<string, string> = {
  model:
    '<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 4 16 4 16 0V5M4 12c0 4 16 4 16 0"/>',
  folder: '<path d="M3 7V5h7l2 3h9v12H3Z"/>',
  chart: '<path d="M4 20V11M12 20V4M20 20V8"/>',
  arrow: '<path d="M5 12h14m-6-6 6 6-6 6"/>',
  check: '<path d="m5 12 4 4L19 6"/>',
};
const icon = (key: string) =>
  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[key] || icons.chart}</svg>`;

function render() {
  app.innerHTML = `<aside><a class="brand" href="#" aria-label="ripbi home"><span class="brand-icon">${icon("chart")}</span>ripbi<span class="desktop">DESKTOP</span></a>
    <div class="workspace-label">WORKSPACE</div><div class="nav-active">${icon("model")} Model health</div>
    <div class="rail-bottom"><span class="dot"></span> Local analysis<p>Your model stays on your machine.</p></div></aside>
    <main><header><span>Workspace <span class="slash">/</span> Model health</span><span class="local-badge">${icon("check")} Read-only analysis</span></header>
    <div class="content"><div class="eyebrow">A LEANER MODEL STARTS HERE</div><div class="title-row"><div><h1>${step === 3 ? "Understand your model." : "Make room for what matters."}</h1><p class="subtitle">${step === 3 ? "A clearer picture of what your reports use — and what they leave behind." : "Connect a semantic model and its reports to uncover unused objects."}</p></div></div>
    <nav class="steps" aria-label="Analysis progress">${["Select model", "Connect reports", "Review results"].map((text, i) => `<button data-step="${i + 1}" ${busy || i + 1 > step ? "disabled" : ""} ${i + 1 === step ? 'aria-current="step"' : ""} class="step ${i + 1 === step ? "current" : ""} ${i + 1 < step ? "complete" : ""}"><span>${i + 1 < step ? "✓" : `0${i + 1}`}</span>${text}</button>`).join('<div class="step-line"></div>')}</nav>
    ${error ? `<div role="alert" class="error">${escape(error)}</div>` : ""}
    ${saved ? '<div role="status" class="notice">Analysis exported successfully.</div>' : ""}
    ${!isTauri() ? '<div class="notice">Browser preview · Launch with <code>npm run tauri dev</code> to select local files and run analysis.</div>' : ""}
    ${step === 1 ? modelView() : step === 2 ? reportsView() : resultsView()}
    <footer><span>ripbi <span class="slash">/</span> Power BI, with less baggage.</span><span>Static analysis · No model changes</span></footer></div></main>`;
  bind();
  if (step !== renderedStep) {
    window.scrollTo(0, 0);
    const heading = app.querySelector("h1");
    if (heading) {
      heading.tabIndex = -1;
      heading.focus({ preventScroll: true });
    }
    renderedStep = step;
  }
}

function modelView() {
  return `<section class="panel selection"><div class="section-heading"><span class="section-icon">${icon("model")}</span><div><h2>Start with your semantic model</h2><p>Choose the model you want to understand.</p></div><span class="step-count">STEP 01 / 03</span></div>
    <div class="drop-zone"><div class="large-icon">${icon("model")}</div><h3>${model ? escape(name(model)) : "One model. The whole picture."}</h3><p>${model ? escape(model) : "Select a .SemanticModel or TMDL folder, or a .pbip project."}</p><div class="button-row"><button class="primary" id="model-folder">${icon("folder")} ${model ? "Change folder" : "Choose model folder"}</button><button id="model-file">Choose .pbip file</button></div><span class="micro">PBIP & TMDL supported · Export PBIX models as a Power BI project first</span></div>
    <div class="panel-bottom"><span>${model ? "✓ Model selected" : "Select a model to continue"}</span><button class="primary" id="next" ${!model ? "disabled" : ""}>Connect reports ${icon("arrow")}</button></div></section>
    <div class="benefits">${[
      [
        "model",
        "See what is really used",
        "Trace model dependencies from report visuals, filters, and slicers.",
      ],
      [
        "folder",
        "Bring every report",
        "Combine multiple reports to get a more complete view of usage.",
      ],
      [
        "chart",
        "Find your next cleanup",
        "Explore unused objects, broken references, and hidden date tables.",
      ],
    ]
      .map(([i, h, p]) => `<div>${icon(i)}<h3>${h}</h3><p>${p}</p></div>`)
      .join("")}</div>`;
}

function reportsView() {
  return `<div class="model-strip">${icon("model")}<div><small>SELECTED MODEL</small><strong>${escape(name(model))}</strong></div><button id="change-model">Change model</button></div>
    <section class="panel selection"><div class="section-heading"><span class="section-icon">${icon("folder")}</span><div><h2>Connect the reports that use it</h2><p>Add reports individually, or search an entire folder.</p></div><span class="step-count">STEP 02 / 03</span></div>
    <div class="report-actions"><button class="primary" id="report-files">+ Add .pbip projects</button><button id="report-folder">${icon("folder")} Add report / search folder</button></div>
    ${reports.length ? `<ul class="file-list">${reports.map((path, i) => `<li>${icon("folder")}<div><strong>${escape(name(path))}</strong><span>${escape(path)}</span></div><button data-remove="${i}" aria-label="Remove ${escape(name(path))}">Remove</button></li>`).join("")}</ul>` : '<div class="empty-reports"><h3>Your reports belong here.</h3><p>Add one or more .Report folders or .pbip projects.<br>A search folder is scanned recursively for connected reports.</p></div>'}
    <div class="coverage-note"><strong>More reports, better context.</strong> Only reports matched to this model are included. Objects used by reports outside this selection may appear unused.</div>
    <div class="panel-bottom"><span>${reports.length} source${reports.length === 1 ? "" : "s"} selected</span><button class="primary" id="analyze" ${busy || !reports.length ? "disabled" : ""}>${busy ? '<span class="spinner"></span> Analyzing model…' : `Analyze model ${icon("arrow")}`}</button></div></section>
    ${busy ? '<p class="scan-status" role="status">Discovering connected reports and tracing dependencies. Large folders can take a little longer.</p>' : ""}`;
}

function metric(title: string, value: string, detail: string, accent = "") {
  return `<article class="metric ${accent}"><span>${title}</span><strong>${value}</strong><small>${detail}</small></article>`;
}

function resultsView() {
  if (!analysis) return "";
  const r = analysis.result,
    s = r.summary;
  const percentage = s.objects
    ? Math.round((s.reachable / s.objects) * 100)
    : 0;
  const counts = new Map<string, number>();
  r.unused.forEach((f) => counts.set(f.type, (counts.get(f.type) || 0) + 1));
  const max = Math.max(1, ...counts.values());
  return `<div class="result-top"><span class="success">● Analysis complete <span>· ${escape(name(r.target))}</span></span><div><button id="edit-inputs">Edit inputs</button> <button id="export">Export JSON ↓</button></div></div>
    <div class="metrics">${metric("Model objects", number(s.objects), `${number(s.roots)} report binding roots`)}${metric("Reachable objects", number(s.reachable), `${percentage}% of the dependency graph`, "green")}${metric("Unused objects", number(s.unused_total), "Includes hidden date machinery", "amber")}${metric("Connected reports", number(r.reports.length), `${number(s.broken + s.broken_artifacts)} reported broken references`)}</div>
    <div class="overview-grid"><section class="panel overview"><div class="heading-inline"><h2>Usage at a glance</h2><span class="tag">GRAPH REACHABILITY</span></div><div class="usage-number">${percentage}<span>% reachable</span></div><div class="usage-bar" role="img" aria-label="${percentage}% reachable"><span style="width:${percentage}%"></span></div><div class="legend"><span><i class="green-dot"></i>Reachable <b>${number(s.reachable)}</b></span><span><i class="amber-dot"></i>Unused <b>${number(s.unused_total)}</b></span></div><p>Reachability includes indirect dependencies and engine-required objects. It does not measure memory or file size.</p></section>
    <section class="panel overview"><div class="heading-inline"><h2>Cleanup candidates</h2><span class="tag">BY OBJECT TYPE</span></div><div class="bars">${
      [...counts]
        .sort((a, b) => b[1] - a[1])
        .slice(0, 5)
        .map(
          ([type, count]) =>
            `<div><span>${escape(label(type))}</span><div class="bar"><i style="width:${(count / max) * 100}%"></i></div><b>${number(count)}</b></div>`,
        )
        .join("") || "<p>No unused object findings in this selection.</p>"
    }</div><p>${number(s.ignored)} ignored by project configuration. Hidden date tables are listed separately.</p></section></div>
    <div class="coverage-note"><strong>Scope matters.</strong> These findings reflect the connected reports below. Review other consumers before removing objects.${r.skips.count ? ` <strong>${r.skips.count} parser notices may limit coverage.</strong>` : ""}</div>
    <section class="panel findings"><div class="tabs" role="group" aria-label="Finding category">${[
      ["unused", `Unused objects (${r.unused.length})`],
      ["broken", `Broken references (${s.broken + s.broken_artifacts})`],
      ["dates", `Date tables (${r.auto_date_time.length})`],
      ["coverage", "Coverage & notices"],
    ]
      .map(
        ([id, text]) =>
          `<button data-tab="${id}" aria-pressed="${tab === id}" class="${tab === id ? "active" : ""}">${text}</button>`,
      )
      .join("")}</div>
    ${
      tab === "unused"
        ? `<div class="filters"><input id="search" aria-label="Search unused objects" placeholder="Search objects or tables…" value="${escape(query)}"><select id="kind" aria-label="Filter object type"><option value="all">All object types</option>${[
            ...counts.keys(),
          ]
            .sort()
            .map(
              (type) =>
                `<option value="${escape(type)}" ${kind === type ? "selected" : ""}>${escape(label(type))}</option>`,
            )
            .join(
              "",
            )}</select></div><div id="finding-list">${findingList()}</div>`
        : otherFindings()
    }
    </section>`;
}

function findingList() {
  const rows = analysis!.result.unused.filter(
    (f) =>
      (kind === "all" || kind === f.type) &&
      `${f.id} ${f.table || ""}`.toLowerCase().includes(query.toLowerCase()),
  );
  return `<div class="table-summary">${number(rows.length)} matching objects · Expand an object to inspect dependencies</div>${
    rows.length
      ? rows
          .slice(0, 300)
          .map(
            (f) =>
              `<details class="finding"><summary><span class="type-pill">${escape(label(f.type))}</span><strong>${escape(f.id)}</strong><span>${escape(f.table || "—")}</span><span class="chevron">⌄</span></summary><div class="finding-detail"><h4>Referenced by</h4>${f.used_by.length ? `<ul>${f.used_by.map((u) => `<li><code>${escape(u.id)}</code> · ${escape(u.provenance)} ${u.also_unused ? '<span class="tag">ALSO UNUSED</span>' : ""}</li>`).join("")}</ul>` : "<p>No other model objects reference this object.</p>"}${f.named_in_power_query.length ? `<h4>Named in Power Query</h4><p>These expressions supply the column; they do not establish report usage.</p><ul>${f.named_in_power_query.map((p) => `<li>${escape(p)}</li>`).join("")}</ul>` : ""}</div></details>`,
          )
          .join("")
      : '<div class="empty-reports">No objects match this filter.</div>'
  }${rows.length > 300 ? '<p class="table-summary">Showing the first 300 matches. Refine your search or export JSON for the full list.</p>' : ""}`;
}

function otherFindings() {
  const r = analysis!.result;
  if (tab === "broken")
    return `<div class="detail-list">${[...r.broken.map((f) => `<article><strong>${escape(f.target)}</strong><p>${escape(label(f.reason))} · ${escape(f.provenance)}</p></article>`), ...r.broken_artifacts.map((f) => `<article><strong>${escape(f.id)}</strong><p>Unresolved: ${f.unresolved_references.map(escape).join(", ")}</p></article>`)].join("") || "<p>No broken references reported. Check coverage notices for parser limitations.</p>"}</div>`;
  if (tab === "dates")
    return `<div class="detail-list">${r.auto_date_time.map((f) => `<article><span class="type-pill">${escape(label(f.verdict))}</span><strong>${escape(f.id)}</strong><p>${escape(f.source_column || "Date table template")}${f.finding ? ` · ${f.finding.used_by.length} referencing objects` : ""}</p></article>`).join("") || "<p>No auto date/time tables found.</p>"}</div>`;
  return `<div class="detail-list"><h3>Connected reports (${r.reports.length})</h3><ul>${r.reports.map((p) => `<li>${escape(p)}</li>`).join("")}</ul><h3>Parser notices (${r.skips.count})</h3>${r.skips.notices.map((n) => `<article><strong>${escape(n.path)}</strong><p>${escape(n.detail)}</p></article>`).join("") || "<p>No parser skips reported.</p>"}<h3>Discovery & configuration log</h3><pre>${escape(analysis!.diagnostics)}</pre></div>`;
}

async function pick(target: "model" | "reports", directory: boolean) {
  error = "";
  try {
    if (!isTauri())
      throw new Error(
        "Open the Tauri desktop app to use native file selection.",
      );
    const selected = await open({
      directory,
      multiple: target === "reports" && !directory,
      title:
        target === "model"
          ? "Select a semantic model"
          : "Select reports or a search folder",
      ...(!directory
        ? { filters: [{ name: "Power BI project", extensions: ["pbip"] }] }
        : {}),
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    if (target === "model") {
      model = paths[0];
      reports = [];
    } else reports = [...new Set([...reports, ...paths])];
    analysis = undefined;
  } catch (e) {
    error = String(e);
  }
  render();
}

async function analyze() {
  busy = true;
  error = "";
  saved = false;
  analysis = undefined;
  render();
  try {
    analysis = await invoke<Analysis>("analyze", { model, reports });
    step = 3;
    tab = "unused";
    query = "";
    kind = "all";
  } catch (e) {
    error = String(e);
  }
  busy = false;
  render();
}

function bind() {
  const on = (id: string, fn: () => void) =>
    document.getElementById(id)?.addEventListener("click", fn);
  on("model-folder", () => void pick("model", true));
  on("model-file", () => void pick("model", false));
  on("report-files", () => void pick("reports", false));
  on("report-folder", () => void pick("reports", true));
  on("next", () => {
    step = 2;
    error = "";
    render();
  });
  on("change-model", () => {
    step = 1;
    render();
  });
  on("edit-inputs", () => {
    step = 2;
    render();
  });
  on("analyze", () => void analyze());
  on(
    "export",
    () =>
      void (async () => {
        error = "";
        saved = false;
        try {
          saved = await invoke<boolean>("export_analysis", {
            contents: JSON.stringify(analysis, null, 2),
          });
        } catch (e) {
          error = String(e);
        }
        render();
      })(),
  );
  app.querySelectorAll<HTMLButtonElement>("[data-step]").forEach((button) =>
    button.addEventListener("click", () => {
      step = Number(button.dataset.step);
      error = "";
      render();
    }),
  );
  app.querySelectorAll<HTMLButtonElement>("[data-remove]").forEach((button) =>
    button.addEventListener("click", () => {
      reports.splice(Number(button.dataset.remove), 1);
      analysis = undefined;
      render();
    }),
  );
  app.querySelectorAll<HTMLButtonElement>("[data-tab]").forEach((button) =>
    button.addEventListener("click", () => {
      tab = button.dataset.tab!;
      render();
    }),
  );
  document.getElementById("search")?.addEventListener("input", (e) => {
    query = (e.target as HTMLInputElement).value;
    document.getElementById("finding-list")!.innerHTML = findingList();
  });
  document.getElementById("kind")?.addEventListener("change", (e) => {
    kind = (e.target as HTMLSelectElement).value;
    document.getElementById("finding-list")!.innerHTML = findingList();
  });
  if (busy)
    app.querySelectorAll<HTMLButtonElement>("button").forEach((button) => {
      button.disabled = true;
    });
}

render();
