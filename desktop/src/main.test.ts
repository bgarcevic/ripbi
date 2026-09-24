// @vitest-environment jsdom
import { beforeEach, expect, test, vi } from "vitest";

const mocks = vi.hoisted(() => ({ open: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  isTauri: () => true,
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.open }));
const click = (id: string) => document.getElementById(id)!.click();
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(async () => {
  vi.resetModules();
  vi.clearAllMocks();
  window.scrollTo = vi.fn();
  document.body.innerHTML = '<div id="app"></div>';
  await import("./main");
});

async function selectInputs() {
  mocks.open.mockResolvedValueOnce("C:/Sales.SemanticModel");
  click("model-folder");
  await settle();
  click("next");
  mocks.open.mockResolvedValueOnce(["C:/Sales.pbip", "C:/Executive.pbip"]);
  click("report-files");
  await settle();
}

test("requires a model then reports; cancellation preserves selection and duplicates are removed", async () => {
  expect((document.getElementById("next") as HTMLButtonElement).disabled).toBe(
    true,
  );
  await selectInputs();
  mocks.open.mockResolvedValueOnce(["C:/Sales.pbip"]);
  click("report-files");
  await settle();
  expect(document.querySelectorAll("[data-remove]")).toHaveLength(2);
  mocks.open.mockResolvedValueOnce(null);
  click("report-folder");
  await settle();
  expect(document.querySelectorAll("[data-remove]")).toHaveLength(2);
  document.querySelector<HTMLButtonElement>("[data-remove]")!.click();
  document.querySelector<HTMLButtonElement>("[data-remove]")?.click();
  expect(
    (document.getElementById("analyze") as HTMLButtonElement).disabled,
  ).toBe(true);
});

test("displays scan failure and allows retry without losing inputs", async () => {
  await selectInputs();
  mocks.invoke.mockRejectedValueOnce("No connected reports were found.");
  click("analyze");
  await settle();
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "No connected reports",
  );
  expect(document.querySelectorAll("[data-remove]")).toHaveLength(2);
  expect(
    (document.getElementById("analyze") as HTMLButtonElement).disabled,
  ).toBe(false);
});

test("renders real metric contract, escapes object names, filters findings, and exports", async () => {
  await selectInputs();
  const result = {
    target: "C:/Sales.SemanticModel",
    reports: ["C:/Sales.Report"],
    summary: {
      objects: 10,
      reachable: 7,
      roots: 2,
      unused_total: 3,
      ignored: 0,
      broken: 0,
      broken_artifacts: 0,
    },
    unused: [
      {
        type: "measure",
        id: "<script>unsafe</script>",
        table: "Sales",
        used_by: [],
        named_in_power_query: [],
      },
    ],
    broken: [],
    broken_artifacts: [],
    auto_date_time: [],
    skips: { count: 0, notices: [] },
  };
  mocks.invoke.mockResolvedValueOnce({
    result,
    diagnostics: "Matched one report",
  });
  click("analyze");
  await settle();
  expect(mocks.invoke).toHaveBeenCalledWith("analyze", {
    model: "C:/Sales.SemanticModel",
    reports: ["C:/Sales.pbip", "C:/Executive.pbip"],
  });
  expect(document.querySelector(".usage-number")?.textContent).toContain("70");
  expect(document.querySelectorAll("script")).toHaveLength(0);
  const search = document.getElementById("search") as HTMLInputElement;
  search.value = "missing";
  search.dispatchEvent(new Event("input"));
  expect(document.getElementById("finding-list")?.textContent).toContain(
    "No objects match",
  );
  mocks.invoke.mockResolvedValueOnce(true);
  click("export");
  await settle();
  expect(mocks.invoke).toHaveBeenLastCalledWith("export_analysis", {
    contents: JSON.stringify(
      { result, diagnostics: "Matched one report" },
      null,
      2,
    ),
  });
  expect(document.querySelector('[role="status"]')?.textContent).toContain(
    "exported",
  );
});
